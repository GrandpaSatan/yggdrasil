/// Static registry of MCP tools available to the agent loop.
///
/// Each tool has a name, description, JSON Schema for parameters, a safety tier,
/// and an endpoint describing how to execute it via HTTP.  The registry is built
/// once at startup and shared via `AppState`.
///
/// Tool metadata (name, description, tier, keywords) comes from the canonical
/// catalog in `ygg_domain::tools`.  This module adds Odin-specific endpoint
/// routing and JSON parameter schemas.
use std::time::Duration;

use serde_json::{json, Value as JsonValue};
use ygg_domain::tools as catalog;

use crate::openai::{FunctionDefinition, ToolDefinition};
use crate::state::AppState;

// ─────────────────────────────────────────────────────────────────
// Tier & endpoint types
// ─────────────────────────────────────────────────────────────────

/// Safety tier controlling which tools an LLM agent may call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolTier {
    /// Read-only, always allowed.
    Safe,
    /// Write operations, require explicit opt-in.
    Restricted,
    /// Never allowed for LLM agents (device control, filesystem writes).
    Blocked,
}

/// Convert from the canonical catalog tier to Odin's local tier type.
fn convert_tier(t: catalog::ToolTier) -> ToolTier {
    match t {
        catalog::ToolTier::Safe => ToolTier::Safe,
        catalog::ToolTier::Restricted => ToolTier::Restricted,
        catalog::ToolTier::Blocked => ToolTier::Blocked,
    }
}

/// Build a `ToolSpec` by pulling metadata from the canonical catalog.
///
/// Only the endpoint and parameter schema are Odin-specific; everything
/// else (name, description, tier, keywords, timeout, voice_always) comes
/// from `ygg_domain::tools::ALL_TOOLS`.
fn from_catalog(name: &str, parameters_schema: JsonValue, endpoint: ToolEndpoint) -> ToolSpec {
    let meta = catalog::find_meta(name)
        .unwrap_or_else(|| panic!("tool '{name}' not found in ygg_domain::tools catalog"));
    ToolSpec {
        name: meta.name,
        description: meta.description,
        parameters_schema,
        tier: convert_tier(meta.tier),
        endpoint,
        timeout_override_secs: meta.timeout_override_secs,
        keywords: meta.keywords,
        voice_always: meta.voice_always,
    }
}

/// Where a tool's HTTP request should be sent.
#[derive(Debug, Clone)]
pub enum ToolEndpoint {
    /// Mimir memory service — uses `state.mimir_url`.
    Mimir(&'static str),
    /// Muninn code search — uses `state.muninn_url`.
    Muninn(&'static str),
    /// Odin's own HTTP routes (e.g. /v1/models, /health).
    OdinSelf(&'static str),
    /// Home Assistant via the HA client in AppState.
    Ha(HaToolKind),
}

/// Sub-types for Home Assistant tool dispatch.
#[derive(Debug, Clone)]
pub enum HaToolKind {
    GetStates,
    ListEntities,
    CallService,
    GenerateAutomation,
}

// ─────────────────────────────────────────────────────────────────
// Tool spec
// ─────────────────────────────────────────────────────────────────

/// A tool available to the agent loop.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters_schema: JsonValue,
    pub tier: ToolTier,
    pub endpoint: ToolEndpoint,
    /// Optional per-tool timeout override (seconds). When `Some`, overrides
    /// the global `AgentLoopConfig.tool_timeout_secs` for this tool only.
    /// Used for long-running operations like gaming VM launches (WOL + boot).
    pub timeout_override_secs: Option<u64>,
    /// Keyword triggers for voice query-based tool selection.
    /// When the user's voice query contains any of these substrings (case-insensitive),
    /// this tool is included in the agent loop context.
    pub keywords: &'static [&'static str],
    /// Core tool — always included in keyword-based selection regardless of query.
    pub voice_always: bool,
}

// ─────────────────────────────────────────────────────────────────
// Registry builder
// ─────────────────────────────────────────────────────────────────

/// Build the complete tool registry.  Called once at startup.
///
/// Tool metadata (name, description, tier, keywords, timeout, voice_always) is
/// pulled from the canonical catalog in `ygg_domain::tools::ALL_TOOLS`.
/// Only the endpoint routing and JSON parameter schemas are Odin-specific.
pub fn build_registry() -> Vec<ToolSpec> {
    vec![
        // ── Safe tier (read-only) ───────────────────────────────
        from_catalog("search_code", json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "languages": { "type": "array", "items": { "type": "string" }, "description": "Filter by language (e.g. [\"rust\", \"python\"])" },
                "limit": { "type": "integer", "description": "Max results (default 10)" }
            },
            "required": ["query"]
        }), ToolEndpoint::Muninn("/api/v1/search")),

        from_catalog("query_memory", json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Query text to search" },
                "limit": { "type": "integer", "description": "Max results (default 5)" }
            },
            "required": ["text"]
        }), ToolEndpoint::Mimir("/api/v1/query")),

        from_catalog("memory_intersect", json!({
            "type": "object",
            "properties": {
                "texts": { "type": "array", "items": { "type": "string" }, "minItems": 2, "description": "Texts to intersect (min 2)" },
                "operation": { "type": "string", "description": "Operation type (default: intersect)" }
            },
            "required": ["texts"]
        }), ToolEndpoint::Mimir("/api/v1/sdr/operations")),

        from_catalog("get_sprint_history", json!({
            "type": "object",
            "properties": {
                "project": { "type": "string", "description": "Project name" },
                "limit": { "type": "integer", "description": "Max sprints to return" }
            },
            "required": ["project"]
        }), ToolEndpoint::Mimir("/api/v1/sprints/list")),

        from_catalog("memory_timeline", json!({
            "type": "object",
            "properties": {
                "start": { "type": "string", "description": "Start time (ISO 8601)" },
                "end": { "type": "string", "description": "End time (ISO 8601)" },
                "limit": { "type": "integer", "description": "Max results" }
            }
        }), ToolEndpoint::Mimir("/api/v1/timeline")),

        from_catalog("list_models",
            json!({ "type": "object", "properties": {} }),
            ToolEndpoint::OdinSelf("/v1/models"),
        ),

        from_catalog("service_health",
            json!({ "type": "object", "properties": {} }),
            ToolEndpoint::OdinSelf("/health"),
        ),

        from_catalog("ast_analyze", json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Symbol name or pattern" },
                "filters": { "type": "array", "items": { "type": "string" }, "description": "Filter by symbol type" }
            },
            "required": ["query"]
        }), ToolEndpoint::Muninn("/api/v1/symbols")),

        from_catalog("impact_analysis", json!({
            "type": "object",
            "properties": {
                "symbol": { "type": "string", "description": "Symbol name to trace" },
                "limit": { "type": "integer", "description": "Max references" }
            },
            "required": ["symbol"]
        }), ToolEndpoint::Muninn("/api/v1/references")),

        from_catalog("ha_get_states", json!({
            "type": "object",
            "properties": {
                "entity_id": { "type": "string", "description": "Specific entity ID for full state details" },
                "domain": { "type": "string", "description": "Filter by domain (e.g. light, switch, sensor, climate)" }
            }
        }), ToolEndpoint::Ha(HaToolKind::GetStates)),

        from_catalog("ha_list_entities", json!({
            "type": "object",
            "properties": {
                "domain": { "type": "string", "description": "Filter by domain (e.g. light, switch)" }
            }
        }), ToolEndpoint::Ha(HaToolKind::ListEntities)),

        from_catalog("config_version",
            json!({ "type": "object", "properties": {} }),
            ToolEndpoint::OdinSelf("/api/v1/version"),
        ),

        from_catalog("web_search", json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "count": { "type": "integer", "description": "Number of results (default 5, max 10)" }
            },
            "required": ["query"]
        }), ToolEndpoint::OdinSelf("/api/v1/web_search")),

        // ── Restricted tier (write operations) ──────────────────
        from_catalog("ha_call_service", json!({
            "type": "object",
            "properties": {
                "domain": { "type": "string", "description": "HA service domain (e.g. light, switch, climate)" },
                "service": { "type": "string", "description": "Service name (e.g. turn_on, turn_off, toggle)" },
                "data": { "type": "object", "description": "Service call data (e.g. {\"entity_id\": \"switch.gaming_pc\"})" }
            },
            "required": ["domain", "service", "data"]
        }), ToolEndpoint::Ha(HaToolKind::CallService)),

        from_catalog("gaming", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "description": "Action to perform", "enum": ["status", "launch", "start", "stop", "list-gpus", "pair"] },
                "vm_name": { "type": "string", "description": "VM or container name (required for launch/start/stop/pair)" },
                "pin": { "type": "string", "description": "4-digit Moonlight pairing PIN (required for pair action)" }
            },
            "required": ["action"]
        }), ToolEndpoint::OdinSelf("/api/v1/gaming")),

        from_catalog("store_memory", json!({
            "type": "object",
            "properties": {
                "cause": { "type": "string", "description": "The trigger or question" },
                "effect": { "type": "string", "description": "The outcome or answer" },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "Categorization tags" }
            },
            "required": ["cause", "effect"]
        }), ToolEndpoint::Mimir("/api/v1/store")),

        from_catalog("context_offload", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "description": "Action: store, retrieve, or list", "enum": ["store", "retrieve", "list"] },
                "content": { "type": "string", "description": "Content to store (required for 'store')" },
                "label": { "type": "string", "description": "Optional label for stored content" },
                "handle": { "type": "string", "description": "Handle to retrieve (required for 'retrieve')" }
            },
            "required": ["action"]
        }), ToolEndpoint::Mimir("/api/v1/context")),

        from_catalog("context_bridge", json!({
            "type": "object",
            "properties": {
                "cause": { "type": "string", "description": "Bridge label (e.g. 'context_bridge:export:sprint-049')" },
                "effect": { "type": "string", "description": "Context snapshot to export" },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "Tags (defaults to ['context_bridge'])" }
            },
            "required": ["cause", "effect"]
        }), ToolEndpoint::Mimir("/api/v1/store")),

        from_catalog("task_queue", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "description": "Queue action: push, pop, complete, cancel, list" },
                "content": { "type": "string", "description": "Task content (for push)" },
                "task_id": { "type": "string", "description": "Task ID (for complete/cancel)" }
            },
            "required": ["action"]
        }), ToolEndpoint::Mimir("/api/v1/tasks")),

        from_catalog("memory_graph", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "description": "Graph action: link, unlink, neighbors, traverse" },
                "source_id": { "type": "string", "description": "Source engram UUID" },
                "target_id": { "type": "string", "description": "Target engram UUID" },
                "relation": { "type": "string", "description": "Relationship type" }
            },
            "required": ["action"]
        }), ToolEndpoint::Mimir("/api/v1/graph")),

        // ── Previously MCP-only tools (Sprint 049 Phase 2B) ─────

        from_catalog("vault", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "description": "Action: store, retrieve, list, delete", "enum": ["store", "retrieve", "list", "delete"] },
                "key": { "type": "string", "description": "Secret key name (required for store/retrieve/delete)" },
                "value": { "type": "string", "description": "Secret value (required for store)" },
                "scope": { "type": "string", "description": "Scope: global, project, or user:name (default: global)" },
                "tags": { "type": "string", "description": "Comma-separated tags for categorization" }
            },
            "required": ["action"]
        }), ToolEndpoint::Mimir("/api/v1/vault")),

        from_catalog("network_topology", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "description": "Action: nodes, services, or health", "enum": ["nodes", "services", "health"] },
                "node_name": { "type": "string", "description": "Filter by node name (optional)" }
            }
        }), ToolEndpoint::OdinSelf("/api/v1/mesh/nodes")),

        from_catalog("ha_generate_automation", json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "Natural language description of the automation (e.g. 'turn off lights at 11pm')" }
            },
            "required": ["description"]
        }), ToolEndpoint::Ha(HaToolKind::GenerateAutomation)),

        from_catalog("config_sync", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "description": "Action: status, pull, push", "enum": ["status", "pull", "push"] },
                "file_type": { "type": "string", "description": "Config file type to sync (e.g. 'odin', 'mimir')" }
            },
            "required": ["action"]
        }), ToolEndpoint::OdinSelf("/api/v1/version")),

        from_catalog("build_check", json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "description": "Build mode: check, build, clippy, test", "enum": ["check", "build", "clippy", "test"] },
                "package": { "type": "string", "description": "Specific package to check (e.g. 'odin', 'mimir')" }
            }
        }), ToolEndpoint::OdinSelf("/api/v1/build_check")),

        from_catalog("deploy", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "description": "Action: build, deploy, build_and_deploy, status", "enum": ["build", "deploy", "build_and_deploy", "status"] },
                "service": { "type": "string", "description": "Service binary name (e.g. 'odin', 'mimir', 'ygg-node')" },
                "target": { "type": "string", "description": "Target node hostname (default: munin)" }
            },
            "required": ["action", "service"]
        }), ToolEndpoint::OdinSelf("/api/v1/deploy")),

        from_catalog("generate", json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "Prompt to send to the LLM" },
                "model": { "type": "string", "description": "Model name (optional, uses default routing)" },
                "max_tokens": { "type": "integer", "description": "Max tokens to generate (default 4096)" }
            },
            "required": ["prompt"]
        }), ToolEndpoint::OdinSelf("/v1/chat/completions")),

        from_catalog("delegate", json!({
            "type": "object",
            "properties": {
                "instructions": { "type": "string", "description": "Task instructions for the local LLM" },
                "agent_type": { "type": "string", "description": "Agent type: executor, docs, qa, review, general" },
                "model": { "type": "string", "description": "Model override (optional)" },
                "language": { "type": "string", "description": "Language hint (optional)" }
            },
            "required": ["instructions"]
        }), ToolEndpoint::OdinSelf("/v1/chat/completions")),

        from_catalog("task_delegate", json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "Task description to delegate" },
                "model": { "type": "string", "description": "Model override (optional)" },
                "constraints": { "type": "array", "items": { "type": "string" }, "description": "Constraints list" }
            },
            "required": ["task"]
        }), ToolEndpoint::OdinSelf("/v1/chat/completions")),

        from_catalog("diff_review", json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "Git diff or code to review" },
                "focus": { "type": "string", "description": "Review focus: all, security, performance, correctness" },
                "description": { "type": "string", "description": "Optional context about the changes" }
            },
            "required": ["content"]
        }), ToolEndpoint::OdinSelf("/v1/chat/completions")),
    ]
}

// ─────────────────────────────────────────────────────────────────
// Conversion to OpenAI tool definitions
// ─────────────────────────────────────────────────────────────────

/// Filter tools by allowed tiers and convert to OpenAI `ToolDefinition` format.
pub fn to_tool_definitions(specs: &[ToolSpec], allowed_tiers: &[ToolTier]) -> Vec<ToolDefinition> {
    specs
        .iter()
        .filter(|s| allowed_tiers.contains(&s.tier))
        .map(|s| ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: s.name.to_string(),
                description: s.description.to_string(),
                parameters: s.parameters_schema.clone(),
            },
        })
        .collect()
}

/// Filter tools by allowed tiers AND a name allowlist.
pub fn to_tool_definitions_filtered(
    specs: &[ToolSpec],
    allowed_tiers: &[ToolTier],
    allowed_names: &[String],
) -> Vec<ToolDefinition> {
    specs
        .iter()
        .filter(|s| allowed_tiers.contains(&s.tier) && allowed_names.iter().any(|n| n == s.name))
        .map(|s| ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: s.name.to_string(),
                description: s.description.to_string(),
                parameters: s.parameters_schema.clone(),
            },
        })
        .collect()
}

/// Select tools for a voice query using keyword matching.
///
/// Returns tools whose `keywords` match substrings in the query (case-insensitive),
/// plus any tools marked `voice_always`. Falls back to only `voice_always` tools
/// when no keywords match.
pub fn select_tools_for_query(
    specs: &[ToolSpec],
    query: &str,
    allowed_tiers: &[ToolTier],
) -> Vec<ToolDefinition> {
    let query_lower = query.to_lowercase();
    specs
        .iter()
        .filter(|s| {
            allowed_tiers.contains(&s.tier)
                && (s.voice_always
                    || s.keywords.iter().any(|kw| query_lower.contains(kw)))
        })
        .map(|s| ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: s.name.to_string(),
                description: s.description.to_string(),
                parameters: s.parameters_schema.clone(),
            },
        })
        .collect()
}

/// Look up a tool spec by name.
pub fn find_tool<'a>(registry: &'a [ToolSpec], name: &str) -> Option<&'a ToolSpec> {
    registry.iter().find(|s| s.name == name)
}

/// Check whether a tool name is allowed given the tier filter.
pub fn is_tool_allowed(registry: &[ToolSpec], name: &str, allowed_tiers: &[ToolTier]) -> bool {
    registry
        .iter()
        .any(|s| s.name == name && allowed_tiers.contains(&s.tier))
}

// ─────────────────────────────────────────────────────────────────
// Tool execution (HTTP dispatch)
// ─────────────────────────────────────────────────────────────────

/// Execute a tool call by dispatching to the appropriate backend service.
///
/// Returns the response body as a string (success) or an error message (failure).
/// The LLM sees both — it can interpret errors and decide to retry or give up.
///
/// Mimir and Muninn endpoints are protected by a circuit breaker: after 3
/// consecutive failures the endpoint is short-circuited for 30 seconds.
pub async fn execute_tool(
    state: &AppState,
    spec: &ToolSpec,
    arguments: &JsonValue,
    timeout: Duration,
) -> Result<String, String> {
    // Resolve the base URL for circuit breaker tracking.
    let (base_url, use_breaker) = match &spec.endpoint {
        ToolEndpoint::Mimir(_) => (Some(state.mimir_url.as_str()), true),
        ToolEndpoint::Muninn(_) => (Some(state.muninn_url.as_str()), true),
        _ => (None, false),
    };

    // Check circuit breaker before dispatching.
    let breaker = if use_breaker {
        let b = state.circuit_breakers.get(base_url.unwrap());
        if !b.allow_request() {
            return Err(format!(
                "Service at {} is temporarily unavailable (circuit breaker open). \
                 Try a different approach or skip this tool.",
                base_url.unwrap()
            ));
        }
        Some(b)
    } else {
        None
    };

    let result = match &spec.endpoint {
        ToolEndpoint::Mimir(path) => {
            let url = format!("{}{}", state.mimir_url, path);
            http_post(&state.http_client, &url, arguments, timeout).await
        }
        ToolEndpoint::Muninn(path) => {
            let url = format!("{}{}", state.muninn_url, path);
            http_post(&state.http_client, &url, arguments, timeout).await
        }
        ToolEndpoint::OdinSelf(path) => {
            // Fast-path: handle trivial OdinSelf endpoints directly to avoid
            // HTTP loopback overhead. Complex handlers still use HTTP.
            match *path {
                "/api/v1/version" => {
                    Ok(json!({ "version": env!("CARGO_PKG_VERSION") }).to_string())
                }
                _ => {
                    // Fall back to HTTP loopback for complex handlers
                    // (web_search, gaming, list_models, health).
                    let url = format!("http://{}{}", state.config.listen_addr, path);
                    if arguments.as_object().is_some_and(|o| o.is_empty()) || arguments.is_null() {
                        http_get(&state.http_client, &url, timeout).await
                    } else {
                        http_post(&state.http_client, &url, arguments, timeout).await
                    }
                }
            }
        }
        ToolEndpoint::Ha(kind) => execute_ha_tool(state, kind, arguments).await,
    };

    // Update circuit breaker state based on result.
    if let Some(b) = breaker {
        match &result {
            Ok(_) => b.record_success(),
            Err(_) => b.record_failure(),
        }
    }

    result
}

/// Whether a failed HTTP response warrants a retry.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::SERVICE_UNAVAILABLE       // 503
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS  // 429
        || status == reqwest::StatusCode::GATEWAY_TIMEOUT    // 504
}

/// Whether a reqwest send error is transient (connection refused, reset, DNS).
fn is_retryable_error(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout()
}

/// Retry backoff delays: 200ms, then 800ms.
const RETRY_DELAYS_MS: [u64; 2] = [200, 800];

async fn http_post(
    client: &reqwest::Client,
    url: &str,
    body: &JsonValue,
    timeout: Duration,
) -> Result<String, String> {
    let mut last_err = String::new();

    for attempt in 0..=RETRY_DELAYS_MS.len() {
        let result = client
            .post(url)
            .json(body)
            .timeout(timeout)
            .send()
            .await;

        match result {
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.map_err(|e| format!("Failed to read response: {e}"))?;
                if status.is_success() {
                    return Ok(text);
                }
                if attempt < RETRY_DELAYS_MS.len() && is_retryable_status(status) {
                    tracing::debug!(url, attempt, %status, "retryable HTTP status, backing off");
                    tokio::time::sleep(Duration::from_millis(RETRY_DELAYS_MS[attempt])).await;
                    last_err = format!("HTTP {status}: {text}");
                    continue;
                }
                return Err(format!("HTTP {status}: {text}"));
            }
            Err(e) => {
                if attempt < RETRY_DELAYS_MS.len() && is_retryable_error(&e) {
                    tracing::debug!(url, attempt, error = %e, "retryable connection error, backing off");
                    tokio::time::sleep(Duration::from_millis(RETRY_DELAYS_MS[attempt])).await;
                    last_err = format!("HTTP request failed: {e}");
                    continue;
                }
                return Err(format!("HTTP request failed: {e}"));
            }
        }
    }

    Err(last_err)
}

async fn http_get(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<String, String> {
    let mut last_err = String::new();

    for attempt in 0..=RETRY_DELAYS_MS.len() {
        let result = client
            .get(url)
            .timeout(timeout)
            .send()
            .await;

        match result {
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.map_err(|e| format!("Failed to read response: {e}"))?;
                if status.is_success() {
                    return Ok(text);
                }
                if attempt < RETRY_DELAYS_MS.len() && is_retryable_status(status) {
                    tracing::debug!(url, attempt, %status, "retryable HTTP status, backing off");
                    tokio::time::sleep(Duration::from_millis(RETRY_DELAYS_MS[attempt])).await;
                    last_err = format!("HTTP {status}: {text}");
                    continue;
                }
                return Err(format!("HTTP {status}: {text}"));
            }
            Err(e) => {
                if attempt < RETRY_DELAYS_MS.len() && is_retryable_error(&e) {
                    tracing::debug!(url, attempt, error = %e, "retryable connection error, backing off");
                    tokio::time::sleep(Duration::from_millis(RETRY_DELAYS_MS[attempt])).await;
                    last_err = format!("HTTP request failed: {e}");
                    continue;
                }
                return Err(format!("HTTP request failed: {e}"));
            }
        }
    }

    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_correct_counts() {
        let registry = build_registry();
        assert_eq!(registry.len(), 30);

        let safe = registry.iter().filter(|s| s.tier == ToolTier::Safe).count();
        let restricted = registry.iter().filter(|s| s.tier == ToolTier::Restricted).count();
        let blocked = registry.iter().filter(|s| s.tier == ToolTier::Blocked).count();
        assert_eq!(safe, 14);
        assert_eq!(restricted, 16);
        assert_eq!(blocked, 0);
    }

    #[test]
    fn all_tools_have_valid_schemas() {
        let registry = build_registry();
        for spec in &registry {
            assert!(!spec.name.is_empty());
            assert!(!spec.description.is_empty());
            let schema_type = spec.parameters_schema.get("type").and_then(|v| v.as_str());
            assert_eq!(schema_type, Some("object"), "tool '{}' schema missing type:object", spec.name);
        }
    }

    #[test]
    fn tier_filtering_safe_only() {
        let registry = build_registry();
        let defs = to_tool_definitions(&registry, &[ToolTier::Safe]);
        assert_eq!(defs.len(), 14);
        for def in &defs {
            assert_eq!(def.tool_type, "function");
        }
    }

    #[test]
    fn tier_filtering_safe_and_restricted() {
        let registry = build_registry();
        let defs = to_tool_definitions(&registry, &[ToolTier::Safe, ToolTier::Restricted]);
        assert_eq!(defs.len(), 30);
    }

    #[test]
    fn tier_filtering_empty_returns_none() {
        let registry = build_registry();
        let defs = to_tool_definitions(&registry, &[]);
        assert_eq!(defs.len(), 0);
    }

    #[test]
    fn find_tool_known() {
        let registry = build_registry();
        assert!(find_tool(&registry, "search_code").is_some());
        assert!(find_tool(&registry, "store_memory").is_some());
    }

    #[test]
    fn find_tool_unknown() {
        let registry = build_registry();
        assert!(find_tool(&registry, "nonexistent").is_none());
    }

    #[test]
    fn is_allowed_respects_tiers() {
        let registry = build_registry();
        // Safe tool with Safe tier → allowed
        assert!(is_tool_allowed(&registry, "search_code", &[ToolTier::Safe]));
        // Restricted tool with Safe tier → blocked
        assert!(!is_tool_allowed(&registry, "store_memory", &[ToolTier::Safe]));
        // Restricted tool with both tiers → allowed
        assert!(is_tool_allowed(&registry, "store_memory", &[ToolTier::Safe, ToolTier::Restricted]));
        // Unknown tool → never allowed
        assert!(!is_tool_allowed(&registry, "nonexistent", &[ToolTier::Safe]));
    }
}

async fn execute_ha_tool(
    state: &AppState,
    kind: &HaToolKind,
    arguments: &JsonValue,
) -> Result<String, String> {
    let ha = state
        .ha_client
        .as_ref()
        .ok_or_else(|| "Home Assistant is not configured".to_string())?;

    match kind {
        HaToolKind::GetStates => {
            let entity_id = arguments.get("entity_id").and_then(|v| v.as_str());
            let domain = arguments.get("domain").and_then(|v| v.as_str());
            let states = ha.get_states().await.map_err(|e| format!("HA error: {e}"))?;

            if let Some(eid) = entity_id {
                // Specific entity → full details
                let filtered: Vec<_> = states.iter().filter(|s| s.entity_id == eid).collect();
                serde_json::to_string_pretty(&filtered).map_err(|e| format!("JSON error: {e}"))
            } else if let Some(d) = domain {
                // Domain filter → full details for matching entities
                let prefix = format!("{d}.");
                let filtered: Vec<_> = states.iter().filter(|s| s.entity_id.starts_with(&prefix)).collect();
                serde_json::to_string_pretty(&filtered).map_err(|e| format!("JSON error: {e}"))
            } else {
                // No filter → compact summary (entity_id + state only) to avoid
                // blowing the agent loop's tool output truncation limit.
                let compact: Vec<_> = states
                    .iter()
                    .map(|s| json!({ "entity_id": s.entity_id, "state": s.state }))
                    .collect();
                serde_json::to_string_pretty(&compact).map_err(|e| format!("JSON error: {e}"))
            }
        }
        HaToolKind::ListEntities => {
            let domain = arguments.get("domain").and_then(|v| v.as_str());
            let states = ha.get_states().await.map_err(|e| format!("HA error: {e}"))?;
            let entities: Vec<&str> = states
                .iter()
                .filter(|s| {
                    domain
                        .map(|d| s.entity_id.starts_with(&format!("{d}.")))
                        .unwrap_or(true)
                })
                .map(|s| s.entity_id.as_str())
                .collect();
            serde_json::to_string_pretty(&entities).map_err(|e| format!("JSON error: {e}"))
        }
        HaToolKind::CallService => {
            let domain = arguments
                .get("domain")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing required field 'domain'".to_string())?;
            let service = arguments
                .get("service")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing required field 'service'".to_string())?;
            let data = arguments
                .get("data")
                .cloned()
                .unwrap_or(json!({}));

            const ALLOWED_DOMAINS: &[&str] = &[
                "light", "switch", "cover", "fan", "media_player", "scene",
                "script", "input_boolean", "input_number", "input_select",
                "input_text", "automation", "climate", "vacuum", "button",
                "number", "select", "humidifier", "water_heater",
            ];
            if !ALLOWED_DOMAINS.contains(&domain) {
                return Err(format!(
                    "Domain '{}' is not allowed. Allowed: {}",
                    domain,
                    ALLOWED_DOMAINS.join(", ")
                ));
            }

            ha.call_service(domain, service, data)
                .await
                .map_err(|e| format!("HA call_service error: {e}"))?;

            Ok(format!("Successfully called {domain}.{service}"))
        }
        HaToolKind::GenerateAutomation => {
            let description = arguments
                .get("description")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing required field 'description'".to_string())?;

            // AutomationGenerator needs both the HA client and the LLM.
            // Create a generator that routes through Odin for LLM calls.
            let generator = ygg_ha::AutomationGenerator::new(
                &format!("http://{}", state.config.listen_addr),
                &state.config.routing.default_model,
            );

            generator
                .generate_automation(ha, description)
                .await
                .map(|yaml| format!("## Generated Automation\n\n```yaml\n{yaml}\n```"))
                .map_err(|e| format!("HA automation generation error: {e}"))
        }
    }
}
