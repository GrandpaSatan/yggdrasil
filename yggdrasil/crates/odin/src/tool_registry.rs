/// Static registry of MCP tools available to the agent loop.
///
/// Each tool has a name, description, JSON Schema for parameters, a safety tier,
/// and an endpoint describing how to execute it via HTTP.  The registry is built
/// once at startup and shared via `AppState`.
use std::time::Duration;

use serde_json::{json, Value as JsonValue};

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
}

// ─────────────────────────────────────────────────────────────────
// Registry builder
// ─────────────────────────────────────────────────────────────────

/// Build the complete tool registry.  Called once at startup.
pub fn build_registry() -> Vec<ToolSpec> {
    vec![
        // ── Safe tier (read-only) ───────────────────────────────
        ToolSpec {
            name: "search_code",
            description: "Search the codebase using semantic and keyword search. Returns matching code chunks with file paths and line numbers.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "languages": { "type": "array", "items": { "type": "string" }, "description": "Filter by language (e.g. [\"rust\", \"python\"])" },
                    "limit": { "type": "integer", "description": "Max results (default 10)" }
                },
                "required": ["query"]
            }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::Muninn("/api/v1/search"),
        },
        ToolSpec {
            name: "query_memory",
            description: "Search engram memory for relevant past context. Returns cause/effect pairs with similarity scores.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Query text to search" },
                    "limit": { "type": "integer", "description": "Max results (default 5)" }
                },
                "required": ["text"]
            }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::Mimir("/api/v1/query"),
        },
        ToolSpec {
            name: "memory_intersect",
            description: "Find semantic intersection of multiple texts using SDR operations.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "texts": { "type": "array", "items": { "type": "string" }, "minItems": 2, "description": "Texts to intersect (min 2)" },
                    "operation": { "type": "string", "description": "Operation type (default: intersect)" }
                },
                "required": ["texts"]
            }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::Mimir("/api/v1/sdr/operations"),
        },
        ToolSpec {
            name: "get_sprint_history",
            description: "Retrieve archived sprint documents for a project.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Project name" },
                    "limit": { "type": "integer", "description": "Max sprints to return" }
                },
                "required": ["project"]
            }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::Mimir("/api/v1/sprints/list"),
        },
        ToolSpec {
            name: "memory_timeline",
            description: "Query memory events within a time range.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "start": { "type": "string", "description": "Start time (ISO 8601)" },
                    "end": { "type": "string", "description": "End time (ISO 8601)" },
                    "limit": { "type": "integer", "description": "Max results" }
                }
            }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::Mimir("/api/v1/timeline"),
        },
        ToolSpec {
            name: "list_models",
            description: "List all LLM models available through Odin.",
            parameters_schema: json!({ "type": "object", "properties": {} }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::OdinSelf("/v1/models"),
        },
        ToolSpec {
            name: "service_health",
            description: "Check health status of Yggdrasil services.",
            parameters_schema: json!({ "type": "object", "properties": {} }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::OdinSelf("/health"),
        },
        ToolSpec {
            name: "ast_analyze",
            description: "Look up code symbols (functions, structs, traits) using AST analysis.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Symbol name or pattern" },
                    "filters": { "type": "array", "items": { "type": "string" }, "description": "Filter by symbol type" }
                },
                "required": ["query"]
            }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::Muninn("/api/v1/symbols"),
        },
        ToolSpec {
            name: "impact_analysis",
            description: "Find all references to a symbol across the codebase.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Symbol name to trace" },
                    "limit": { "type": "integer", "description": "Max references" }
                },
                "required": ["symbol"]
            }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::Muninn("/api/v1/references"),
        },
        ToolSpec {
            name: "ha_get_states",
            description: "Get current state of Home Assistant entities.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "entity_id": { "type": "string", "description": "Specific entity ID (optional, returns all if omitted)" }
                }
            }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::Ha(HaToolKind::GetStates),
        },
        ToolSpec {
            name: "ha_list_entities",
            description: "List Home Assistant entities, optionally filtered by domain.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "domain": { "type": "string", "description": "Filter by domain (e.g. light, switch)" }
                }
            }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::Ha(HaToolKind::ListEntities),
        },
        ToolSpec {
            name: "config_version",
            description: "Get the current Yggdrasil configuration version.",
            parameters_schema: json!({ "type": "object", "properties": {} }),
            tier: ToolTier::Safe,
            endpoint: ToolEndpoint::OdinSelf("/api/v1/version"),
        },
        // ── Restricted tier (write operations) ──────────────────
        ToolSpec {
            name: "ha_call_service",
            description: "Call a Home Assistant service to control devices. Allowed domains: light, switch, cover, fan, media_player, scene, script, input_boolean, input_number, input_select, input_text, automation, climate, vacuum, button, number, select, humidifier, water_heater.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "domain": { "type": "string", "description": "HA service domain (e.g. light, switch, climate)" },
                    "service": { "type": "string", "description": "Service name (e.g. turn_on, turn_off, toggle)" },
                    "data": { "type": "object", "description": "Service call data (e.g. {\"entity_id\": \"switch.gaming_pc\"})" }
                },
                "required": ["domain", "service", "data"]
            }),
            tier: ToolTier::Restricted,
            endpoint: ToolEndpoint::Ha(HaToolKind::CallService),
        },
        ToolSpec {
            name: "gaming",
            description: "Manage cloud gaming VMs on Thor (Proxmox). Actions: status (check all VMs and GPUs), launch (wake Thor + assign GPU + start VM), stop (shutdown VM + release GPU), list-gpus (show GPU availability).",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "Action: status, launch, stop, list-gpus", "enum": ["status", "launch", "stop", "list-gpus"] },
                    "vm_name": { "type": "string", "description": "VM name (required for launch/stop, e.g. Harpy)" }
                },
                "required": ["action"]
            }),
            tier: ToolTier::Restricted,
            endpoint: ToolEndpoint::OdinSelf("/api/v1/gaming"),
        },
        ToolSpec {
            name: "store_memory",
            description: "Store a new engram as a cause/effect pair in memory.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "cause": { "type": "string", "description": "The trigger or question" },
                    "effect": { "type": "string", "description": "The outcome or answer" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Categorization tags" }
                },
                "required": ["cause", "effect"]
            }),
            tier: ToolTier::Restricted,
            endpoint: ToolEndpoint::Mimir("/api/v1/store"),
        },
        ToolSpec {
            name: "context_offload",
            description: "Offload large context to server-side storage.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Content to offload" },
                    "label": { "type": "string", "description": "Optional label" }
                },
                "required": ["content"]
            }),
            tier: ToolTier::Restricted,
            endpoint: ToolEndpoint::Mimir("/api/v1/context/offload"),
        },
        ToolSpec {
            name: "context_bridge",
            description: "Bridge context between sessions.",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "Bridge action (export/import)" },
                    "session_id": { "type": "string", "description": "Target session ID" }
                },
                "required": ["action"]
            }),
            tier: ToolTier::Restricted,
            endpoint: ToolEndpoint::Mimir("/api/v1/context/bridge"),
        },
        ToolSpec {
            name: "task_queue",
            description: "Manage the task queue (push, pop, complete, cancel, list).",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "Queue action: push, pop, complete, cancel, list" },
                    "content": { "type": "string", "description": "Task content (for push)" },
                    "task_id": { "type": "string", "description": "Task ID (for complete/cancel)" }
                },
                "required": ["action"]
            }),
            tier: ToolTier::Restricted,
            endpoint: ToolEndpoint::Mimir("/api/v1/tasks"),
        },
        ToolSpec {
            name: "memory_graph",
            description: "Manage memory graph relationships (link, unlink, neighbors, traverse).",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "Graph action: link, unlink, neighbors, traverse" },
                    "source_id": { "type": "string", "description": "Source engram UUID" },
                    "target_id": { "type": "string", "description": "Target engram UUID" },
                    "relation": { "type": "string", "description": "Relationship type" }
                },
                "required": ["action"]
            }),
            tier: ToolTier::Restricted,
            endpoint: ToolEndpoint::Mimir("/api/v1/graph"),
        },
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
pub async fn execute_tool(
    state: &AppState,
    spec: &ToolSpec,
    arguments: &JsonValue,
    timeout: Duration,
) -> Result<String, String> {
    match &spec.endpoint {
        ToolEndpoint::Mimir(path) => {
            let url = format!("{}{}", state.mimir_url, path);
            http_post(&state.http_client, &url, arguments, timeout).await
        }
        ToolEndpoint::Muninn(path) => {
            let url = format!("{}{}", state.muninn_url, path);
            http_post(&state.http_client, &url, arguments, timeout).await
        }
        ToolEndpoint::OdinSelf(path) => {
            // Call Odin's own HTTP routes via localhost.
            let url = format!("http://{}{}", state.config.listen_addr, path);
            if arguments.as_object().is_some_and(|o| o.is_empty()) || arguments.is_null() {
                http_get(&state.http_client, &url, timeout).await
            } else {
                http_post(&state.http_client, &url, arguments, timeout).await
            }
        }
        ToolEndpoint::Ha(kind) => execute_ha_tool(state, kind, arguments).await,
    }
}

async fn http_post(
    client: &reqwest::Client,
    url: &str,
    body: &JsonValue,
    timeout: Duration,
) -> Result<String, String> {
    let resp = client
        .post(url)
        .json(body)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("Failed to read response: {e}"))?;

    if status.is_success() {
        Ok(text)
    } else {
        Err(format!("HTTP {status}: {text}"))
    }
}

async fn http_get(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<String, String> {
    let resp = client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("Failed to read response: {e}"))?;

    if status.is_success() {
        Ok(text)
    } else {
        Err(format!("HTTP {status}: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_correct_counts() {
        let registry = build_registry();
        assert_eq!(registry.len(), 19);

        let safe = registry.iter().filter(|s| s.tier == ToolTier::Safe).count();
        let restricted = registry.iter().filter(|s| s.tier == ToolTier::Restricted).count();
        let blocked = registry.iter().filter(|s| s.tier == ToolTier::Blocked).count();
        assert_eq!(safe, 12);
        assert_eq!(restricted, 7);
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
        assert_eq!(defs.len(), 12);
        for def in &defs {
            assert_eq!(def.tool_type, "function");
        }
    }

    #[test]
    fn tier_filtering_safe_and_restricted() {
        let registry = build_registry();
        let defs = to_tool_definitions(&registry, &[ToolTier::Safe, ToolTier::Restricted]);
        assert_eq!(defs.len(), 19);
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
            let states = ha.get_states().await.map_err(|e| format!("HA error: {e}"))?;
            if let Some(eid) = entity_id {
                let filtered: Vec<_> = states.iter().filter(|s| s.entity_id == eid).collect();
                serde_json::to_string_pretty(&filtered).map_err(|e| format!("JSON error: {e}"))
            } else {
                serde_json::to_string_pretty(&states).map_err(|e| format!("JSON error: {e}"))
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
    }
}
