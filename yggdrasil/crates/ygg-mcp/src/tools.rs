//! MCP tool implementations for the Yggdrasil system.
//!
//! Each function makes HTTP calls to Odin or Muninn and returns a formatted
//! `CallToolResult` suitable for the MCP protocol. All downstream failures are
//! captured as `is_error: true` results rather than propagated as Rust errors.

use reqwest::Client;
use rmcp::model::{CallToolResult, Content};
// schemars 1.x — must be the same version that rmcp 1.1 depends on.
// This is pinned via workspace to "1" so both this crate and rmcp resolve the
// same schemars::JsonSchema trait object.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use tracing::instrument;
use ygg_domain::config::McpServerConfig;
use ygg_ha::{AutomationGenerator, HaClient};

// ---------------------------------------------------------------------------
// Parameter structs
// ---------------------------------------------------------------------------

/// Parameters for the `search_code` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchCodeParams {
    /// Natural language or code search query.
    pub query: String,
    /// Optional filter by programming language (e.g. ["rust", "python"]).
    pub languages: Option<Vec<String>>,
    /// Maximum number of results (default 10, max 50).
    #[serde(default = "default_search_limit")]
    pub limit: Option<u32>,
}

fn default_search_limit() -> Option<u32> {
    Some(10)
}

/// Parameters for the `query_memory` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryMemoryParams {
    /// Query text to search engram memory.
    pub text: String,
    /// Maximum number of engrams to return (default 5, max 20).
    #[serde(default = "default_query_limit")]
    pub limit: Option<u32>,
}

fn default_query_limit() -> Option<u32> {
    Some(5)
}

/// Parameters for the `store_memory` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StoreMemoryParams {
    /// The trigger or question (what happened).
    pub cause: String,
    /// The outcome or answer (what resulted).
    pub effect: String,
    /// Optional tags for categorization (e.g. "fact", "decision", "coding").
    /// Passed through to Mimir and used to set trigger_type on the engram.
    pub tags: Option<Vec<String>>,
    /// Optional engram UUID for update-by-ID. When provided, bypasses the novelty
    /// gate and updates the existing engram in place instead of creating a new one.
    pub id: Option<String>,
    /// Set to true to bypass the novelty gate and force-create a new engram
    /// even when a similar one exists.
    pub force: Option<bool>,
}

/// Parameters for the `generate` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GenerateParams {
    /// The prompt or question to send to the LLM.
    pub prompt: String,
    /// Model name (e.g. "qwen3-coder-30b-a3b", "qwq-32b").
    /// If omitted, uses Odin's default routing.
    pub model: Option<String>,
    /// Maximum tokens to generate (optional, default 4096).
    pub max_tokens: Option<u64>,
}

/// Parameters for the `get_sprint_history` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetSprintHistoryParams {
    /// Project name to filter by (e.g. "yggdrasil"). Optional — returns all sprint engrams if omitted.
    pub project: Option<String>,
    /// Maximum number of sprint summaries to return (default 5).
    #[serde(default = "default_sprint_history_limit")]
    pub limit: Option<u32>,
}

fn default_sprint_history_limit() -> Option<u32> {
    Some(5)
}

/// Parameters for the `sync_docs` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncDocsParams {
    /// Lifecycle event: "sprint_start", "sprint_end", or "setup".
    pub event: String,
    /// Sprint identifier, e.g. "027". Required for sprint_start and sprint_end; ignored for setup.
    #[serde(default)]
    pub sprint_id: String,
    /// Full content of the sprint document. Required for sprint_start and sprint_end; used as
    /// project context for setup (can describe the project for initial doc scaffolding).
    #[serde(default)]
    pub sprint_content: String,
    /// Workspace root path (e.g. "/home/user/project"). Overrides config.workspace_path.
    /// The tool resolves workspace as: this param → config.workspace_path → error.
    #[serde(default)]
    pub workspace_path: Option<String>,
}

/// Parameters for the `memory_intersect` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryIntersectParams {
    /// Two or more texts to embed and combine.
    pub texts: Vec<String>,
    /// SDR set operation: "and" (intersection), "or" (union), or "xor" (difference).
    #[serde(default = "default_sdr_operation")]
    pub operation: String,
    /// Maximum number of matching engrams (default 5, max 20).
    #[serde(default = "default_intersect_limit")]
    pub limit: Option<u32>,
}

fn default_sdr_operation() -> String {
    "and".to_string()
}

fn default_intersect_limit() -> Option<u32> {
    Some(5)
}

/// Parameters for the `screenshot` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenshotParams {
    /// URL to capture (e.g. "http://localhost:3000/dashboard").
    pub url: String,
    /// Optional CSS selector to wait for before capture (useful for SPAs).
    #[serde(default)]
    pub selector: Option<String>,
    /// Capture the full scrollable page instead of just the viewport (default: false).
    #[serde(default)]
    pub full_page: Option<bool>,
    /// Viewport width in pixels (default: 1280).
    #[serde(default)]
    pub viewport_width: Option<u32>,
    /// Viewport height in pixels (default: 720).
    #[serde(default)]
    pub viewport_height: Option<u32>,
}

/// Parameters for the `service_health` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ServiceHealthParams {
    /// Optional: only check specific services. Valid names: odin, mimir, muninn,
    /// ollama_munin, ollama_hugin, postgres, qdrant. Default: check all.
    #[serde(default)]
    pub services: Option<Vec<String>>,
}

/// Parameters for the `build_check` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuildCheckParams {
    /// Check mode: "check" (default), "clippy", or "test".
    #[serde(default = "default_build_mode")]
    pub mode: Option<String>,
    /// Optional: specific crate to check (e.g. "ygg-mcp"). Default: whole workspace.
    #[serde(default)]
    pub crate_name: Option<String>,
}

fn default_build_mode() -> Option<String> {
    Some("check".to_string())
}

/// Parameters for the `memory_timeline` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryTimelineParams {
    /// Optional semantic search text (combined with time/tag filters).
    #[serde(default)]
    pub text: Option<String>,
    /// ISO 8601 datetime — only engrams created after this time.
    #[serde(default)]
    pub after: Option<String>,
    /// ISO 8601 datetime — only engrams created before this time.
    #[serde(default)]
    pub before: Option<String>,
    /// Filter: engrams must have ALL of these tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Filter: memory tier ("core", "recall", or "archival").
    #[serde(default)]
    pub tier: Option<String>,
    /// Max results (default 10, max 50).
    #[serde(default = "default_timeline_limit")]
    pub limit: Option<u32>,
}

fn default_timeline_limit() -> Option<u32> {
    Some(10)
}

/// Parameters for the `context_offload` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextOffloadParams {
    /// Action: "store", "retrieve", or "list".
    pub action: String,
    /// For "store": the content to offload (required).
    #[serde(default)]
    pub content: Option<String>,
    /// For "store": optional label for the offloaded content.
    #[serde(default)]
    pub label: Option<String>,
    /// For "retrieve": the handle ID to fetch (required).
    #[serde(default)]
    pub handle: Option<String>,
}

/// Parameters for the `task_delegate` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskDelegateParams {
    /// Natural language description of what to implement.
    pub task: String,
    /// Optional: existing code to use as reference pattern.
    #[serde(default)]
    pub reference_pattern: Option<String>,
    /// Optional: specific files/modules to focus on.
    #[serde(default)]
    pub scope_files: Option<Vec<String>>,
    /// Optional: constraints ("no unsafe", "must be async", "follow existing error handling").
    #[serde(default)]
    pub constraints: Option<Vec<String>>,
    /// Optional: target language (default: infer from scope_files or "rust").
    #[serde(default)]
    pub language: Option<String>,
    /// Optional: model override (e.g. "qwen3-coder:30b-a3b-q4_K_M").
    #[serde(default)]
    pub model: Option<String>,
    /// Max tokens for response (default: 8192).
    #[serde(default = "default_delegate_max_tokens")]
    pub max_tokens: Option<u64>,
}

fn default_delegate_max_tokens() -> Option<u64> {
    Some(8192)
}

/// Inline file content for the delegate tool (client reads files, passes content here).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileContext {
    /// File path (for display/context, not read from server disk).
    pub path: String,
    /// File content.
    pub content: String,
}

/// Parameters for the unified `delegate` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DelegateParams {
    /// Agent type determines system prompt: "executor", "docs", "qa", "review", "general".
    #[serde(default)]
    pub agent_type: Option<String>,
    /// Task instructions from the client AI.
    pub instructions: String,
    /// Engram UUIDs to fetch and inject as memory context.
    #[serde(default)]
    pub memory_ids: Option<Vec<String>>,
    /// Code search queries to run for context assembly.
    #[serde(default)]
    pub search_queries: Option<Vec<String>>,
    /// Inline file content to include as context (client reads files, passes content here).
    #[serde(default)]
    pub file_context: Option<Vec<FileContext>>,
    /// Reference pattern code to follow.
    #[serde(default)]
    pub reference_pattern: Option<String>,
    /// Whether to parse output as file blocks (```path/to/file.rs content ```).
    #[serde(default)]
    pub structured_output: Option<bool>,
    /// Model override.
    #[serde(default)]
    pub model: Option<String>,
    /// Max tokens for generation (default 8192).
    #[serde(default = "default_delegate_max_tokens")]
    pub max_tokens: Option<u64>,
    /// Language hint.
    #[serde(default)]
    pub language: Option<String>,
    /// Constraints list.
    #[serde(default)]
    pub constraints: Option<Vec<String>>,
    /// Enable agentic tool-use mode. When true, the local LLM can call
    /// MCP tools autonomously via Odin's agent loop.
    #[serde(default)]
    pub agentic: Option<bool>,
    /// Tool allowlist for agentic mode (tool names). When absent, all
    /// safe-tier tools are available. Only used when `agentic` is true.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
}

/// Parameters for the `diff_review` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiffReviewParams {
    /// Git diff text, or file content to review.
    pub content: String,
    /// Review focus: "security", "performance", "architecture", "bugs", "all" (default: "all").
    #[serde(default = "default_review_focus")]
    pub focus: Option<String>,
    /// Description of the change's intent.
    #[serde(default)]
    pub description: Option<String>,
}

fn default_review_focus() -> Option<String> {
    Some("all".to_string())
}

/// Parameters for the `context_bridge` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextBridgeParams {
    /// Action: "export" or "import".
    pub action: String,
    /// For "export": optional label. For "import": context snapshot ID.
    #[serde(default)]
    pub context_id: Option<String>,
}

/// Parameters for the `ast_analyze` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AstAnalyzeParams {
    /// Symbol name to look up (e.g. "AppState", "health_handler").
    #[serde(default)]
    pub name: Option<String>,
    /// Chunk type filter: "function", "struct", "enum", "impl", "trait", "module".
    #[serde(default)]
    pub chunk_type: Option<String>,
    /// Language filter: "rust", "go", "python", "typescript".
    #[serde(default)]
    pub language: Option<String>,
    /// File path filter (exact match).
    #[serde(default)]
    pub file_path: Option<String>,
    /// Max results (default 20, max 100).
    #[serde(default = "default_ast_limit")]
    pub limit: Option<u32>,
}

fn default_ast_limit() -> Option<u32> {
    Some(20)
}

/// Parameters for the `impact_analysis` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImpactAnalysisParams {
    /// Symbol name to find references for.
    pub symbol: String,
    /// Optional language filter.
    #[serde(default)]
    pub language: Option<String>,
    /// Optional: UUID of the symbol's definition chunk to exclude from results.
    #[serde(default)]
    pub exclude_id: Option<String>,
    /// Max results (default 20, max 50).
    #[serde(default = "default_impact_limit")]
    pub limit: Option<u32>,
}

fn default_impact_limit() -> Option<u32> {
    Some(20)
}

/// Parameters for the `task_queue` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskQueueParams {
    /// Action: "push", "pop", "complete", "cancel", or "list".
    pub action: String,
    // ── push fields ──
    /// Task title (required for "push").
    #[serde(default)]
    pub title: Option<String>,
    /// Task description (for "push").
    #[serde(default)]
    pub description: Option<String>,
    /// Priority: higher = more urgent (for "push", default 0).
    #[serde(default)]
    pub priority: Option<i32>,
    /// Tags for categorization (for "push").
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    // ── pop fields ──
    /// Agent name claiming the task (required for "pop").
    #[serde(default)]
    pub agent: Option<String>,
    // ── complete/cancel fields ──
    /// Task UUID (required for "complete" and "cancel").
    #[serde(default)]
    pub task_id: Option<String>,
    /// Whether the task succeeded (for "complete", default true).
    #[serde(default)]
    pub success: Option<bool>,
    /// Result or error message (for "complete").
    #[serde(default)]
    pub result: Option<String>,
    // ── shared filters ──
    /// Project scope (for "push", "pop", "list").
    #[serde(default)]
    pub project: Option<String>,
    /// Status filter (for "list": "pending", "in_progress", "completed", "failed", "cancelled").
    #[serde(default)]
    pub status: Option<String>,
    /// Max results for "list" (default 20).
    #[serde(default = "default_task_queue_limit")]
    pub limit: Option<u32>,
}

fn default_task_queue_limit() -> Option<u32> {
    Some(20)
}

/// Parameters for the `memory_graph` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryGraphParams {
    /// Action: "link", "unlink", "neighbors", or "traverse".
    pub action: String,
    // ── link/unlink fields ──
    /// Source engram UUID (required for "link" and "unlink").
    #[serde(default)]
    pub source_id: Option<String>,
    /// Target engram UUID (required for "link" and "unlink").
    #[serde(default)]
    pub target_id: Option<String>,
    /// Relation type: "related_to", "depends_on", "supersedes", "caused_by".
    #[serde(default)]
    pub relation: Option<String>,
    /// Edge weight 0.0-1.0 (for "link", default 1.0).
    #[serde(default)]
    pub weight: Option<f32>,
    // ── neighbors fields ──
    /// Engram UUID to query neighbors for (required for "neighbors").
    #[serde(default)]
    pub engram_id: Option<String>,
    /// Direction: "outgoing", "incoming", or "both" (default "both").
    #[serde(default)]
    pub direction: Option<String>,
    // ── traverse fields ──
    /// Starting engram UUID for BFS traversal (required for "traverse").
    #[serde(default)]
    pub start_id: Option<String>,
    /// Max BFS depth/hops (default 2, max 5).
    #[serde(default)]
    pub max_depth: Option<u32>,
    /// Max results (default 20).
    #[serde(default = "default_graph_tool_limit")]
    pub limit: Option<u32>,
}

fn default_graph_tool_limit() -> Option<u32> {
    Some(20)
}

/// Parameters for the `config_version` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigVersionParams {
    /// Action: "check" (compare versions), "bump" (increment version), or "info" (show all).
    pub action: String,
    /// Bump type: "minor" or "patch" (required for "bump" action).
    #[serde(default)]
    pub bump_type: Option<String>,
    /// Component to bump: "server", "client", or "config" (default: "config").
    #[serde(default)]
    pub component: Option<String>,
}

/// Parameters for the `config_sync` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigSyncParams {
    /// Action: "push" (upload config), "pull" (download config), or "status" (show all configs).
    pub action: String,
    /// File type: "global_settings", "global_claude_md", "project_settings", "project_claude_md".
    #[serde(default)]
    pub file_type: Option<String>,
    /// Config file content (optional for "push" — reads from local disk if omitted).
    #[serde(default)]
    pub content: Option<String>,
    /// Workstation identifier (for "push", defaults to hostname).
    #[serde(default)]
    pub workstation_id: Option<String>,
    /// Project ID for project-scoped configs (optional).
    #[serde(default)]
    pub project_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Internal HTTP response types (Muninn / Odin shapes)
// ---------------------------------------------------------------------------

/// Mirrors `ygg_domain::chunk::CodeChunk` (flat deserialization of the nested chunk object).
#[derive(Debug, Deserialize)]
struct SearchChunk {
    file_path: Option<String>,
    language: Option<serde_json::Value>, // Language enum serialized as string
    content: Option<String>,
    name: Option<String>,
    start_line: Option<u64>,
    end_line: Option<u64>,
}

/// Mirrors `ygg_domain::chunk::SearchResult` — one element of the `results` array.
#[derive(Debug, Deserialize)]
struct SearchResultItem {
    chunk: Option<SearchChunk>,
    score: Option<f64>,
}

/// Mirrors `ygg_domain::chunk::SearchResponse`.
#[derive(Debug, Deserialize)]
struct SearchApiResponse {
    results: Option<Vec<SearchResultItem>>,
}

#[derive(Debug, Deserialize)]
struct EngramResult {
    cause: Option<String>,
    effect: Option<String>,
    similarity: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct QueryApiResponse {
    results: Option<Vec<EngramResult>>,
}

#[derive(Debug, Deserialize)]
struct StoreApiResponse {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: Option<ChatMessage>,
}

#[derive(Debug, Deserialize)]
struct ChatApiResponse {
    choices: Option<Vec<ChatChoice>>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: Option<String>,
    owned_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelsApiResponse {
    data: Option<Vec<ModelEntry>>,
}

/// Build an error `CallToolResult` with a human-readable message.
fn tool_error(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.into())])
}

/// Build a success `CallToolResult` with a single text block.
fn tool_ok(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text.into())])
}

// ---------------------------------------------------------------------------
// Tool: search_code
// ---------------------------------------------------------------------------

/// Maximum input field size (100KB) — matches Mimir's downstream limit.
const MAX_INPUT_BYTES: usize = 100 * 1024;

/// POST to Muninn /api/v1/search and format results as markdown.
#[instrument(skip(client, config), fields(query = %params.query))]
pub async fn search_code(
    client: &Client,
    config: &McpServerConfig,
    params: SearchCodeParams,
) -> CallToolResult {
    if params.query.len() > MAX_INPUT_BYTES {
        return tool_error(format!(
            "query exceeds maximum size of {MAX_INPUT_BYTES} bytes"
        ));
    }

    let muninn_url = match &config.muninn_url {
        Some(u) => u.clone(),
        None => {
            return tool_error(
                "Code search unavailable. No Muninn URL configured. \
                 Set muninn_url in the MCP server config.",
            );
        }
    };

    let timeout = Duration::from_secs(config.timeout_secs);
    let url = format!("{}/api/v1/search", muninn_url.trim_end_matches('/'));

    #[derive(Serialize)]
    struct Req<'a> {
        query: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        languages: Option<&'a Vec<String>>,
        limit: u32,
    }

    let body = Req {
        query: &params.query,
        languages: params.languages.as_ref(),
        limit: params.limit.unwrap_or(10).min(50),
    };

    let resp = match client
        .post(&url)
        .json(&body)
        .timeout(timeout)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!(
                "Request timed out after {} seconds",
                config.timeout_secs
            ));
        }
        Err(e) => {
            return tool_error(format!(
                "Code search unavailable. Muninn is not reachable at {}: {}",
                muninn_url, e
            ));
        }
    };

    let status = resp.status();
    if status.is_client_error() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Code search failed (HTTP {}): {}", status, body));
    }
    if status.is_server_error() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Internal error from Muninn (HTTP {}): {}", status, body));
    }

    let api: SearchApiResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse Muninn response: {}", e)),
    };

    let results = api.results.unwrap_or_default();
    if results.is_empty() {
        return tool_ok(format!(
            "## Code Search Results for: \"{}\"\n\nNo results found.",
            params.query
        ));
    }

    let mut out = format!("## Code Search Results for: \"{}\"\n\n", params.query);
    for (i, item) in results.iter().enumerate() {
        let score = item.score.unwrap_or(0.0);
        let chunk = item.chunk.as_ref();
        let path = chunk.and_then(|c| c.file_path.as_deref()).unwrap_or("unknown");
        let lang = chunk
            .and_then(|c| c.language.as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("text");
        let name = chunk.and_then(|c| c.name.as_deref()).unwrap_or("");
        let start = chunk.and_then(|c| c.start_line).unwrap_or(0);
        let end = chunk.and_then(|c| c.end_line).unwrap_or(0);
        let content = chunk.and_then(|c| c.content.as_deref()).unwrap_or("");

        out.push_str(&format!(
            "### {}. `{}` in {} [score: {:.3}, lines {}-{}]\n```{}\n{}\n```\n\n",
            i + 1,
            name,
            path,
            score,
            start,
            end,
            lang,
            content.trim()
        ));
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: query_memory
// ---------------------------------------------------------------------------

/// POST to Odin /api/v1/query and format engram results as markdown.
#[instrument(skip(client, config), fields(text = %params.text))]
pub async fn query_memory(
    client: &Client,
    config: &McpServerConfig,
    params: QueryMemoryParams,
) -> CallToolResult {
    if params.text.len() > MAX_INPUT_BYTES {
        return tool_error(format!(
            "text exceeds maximum size of {MAX_INPUT_BYTES} bytes"
        ));
    }

    let timeout = Duration::from_secs(config.timeout_secs);
    let url = format!(
        "{}/api/v1/query",
        config.odin_url.trim_end_matches('/')
    );

    #[derive(Serialize)]
    struct Req<'a> {
        text: &'a str,
        limit: u32,
    }

    let body = Req {
        text: &params.text,
        limit: params.limit.unwrap_or(5).min(20),
    };

    let resp = match client
        .post(&url)
        .json(&body)
        .timeout(timeout)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!(
                "Request timed out after {} seconds",
                config.timeout_secs
            ));
        }
        Err(e) => {
            return tool_error(format!(
                "Odin is not reachable at {}. \
                 Ensure the Yggdrasil orchestrator is running. Error: {}",
                config.odin_url, e
            ));
        }
    };

    let status = resp.status();
    if status.is_client_error() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Memory query failed (HTTP {}): {}", status, body));
    }
    if status.is_server_error() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Internal error from Odin (HTTP {}): {}", status, body));
    }

    let api: QueryApiResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse Odin response: {}", e)),
    };

    let results = api.results.unwrap_or_default();
    if results.is_empty() {
        return tool_ok(format!(
            "## Memory Results for: \"{}\"\n\nNo engrams found.",
            params.text
        ));
    }

    let mut out = format!("## Memory Results for: \"{}\"\n\n", params.text);
    for (i, engram) in results.iter().enumerate() {
        let cause = engram.cause.as_deref().unwrap_or("(unknown)");
        let effect = engram.effect.as_deref().unwrap_or("(unknown)");
        let sim = engram.similarity.unwrap_or(0.0);

        out.push_str(&format!(
            "{}. **Cause:** {}\n   **Effect:** {}\n   **Similarity:** {:.2}\n\n",
            i + 1,
            cause,
            effect,
            sim
        ));
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: store_memory
// ---------------------------------------------------------------------------

/// POST to Odin /api/v1/store and return the created engram ID.
#[instrument(skip(client, config), fields(cause = %params.cause))]
pub async fn store_memory(
    client: &Client,
    config: &McpServerConfig,
    params: StoreMemoryParams,
) -> CallToolResult {
    if params.cause.len() > MAX_INPUT_BYTES {
        return tool_error(format!(
            "cause exceeds maximum size of {MAX_INPUT_BYTES} bytes"
        ));
    }
    if params.effect.len() > MAX_INPUT_BYTES {
        return tool_error(format!(
            "effect exceeds maximum size of {MAX_INPUT_BYTES} bytes"
        ));
    }

    let timeout = Duration::from_secs(config.timeout_secs);
    let url = format!(
        "{}/api/v1/store",
        config.odin_url.trim_end_matches('/')
    );

    #[derive(Serialize)]
    struct Req<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<&'a str>,
        cause: &'a str,
        effect: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        tags: Option<&'a Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    }

    let body = Req {
        id: params.id.as_deref(),
        cause: &params.cause,
        effect: &params.effect,
        tags: params.tags.as_ref(),
        force: params.force,
    };

    let resp = match client
        .post(&url)
        .json(&body)
        .timeout(timeout)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!(
                "Request timed out after {} seconds",
                config.timeout_secs
            ));
        }
        Err(e) => {
            return tool_error(format!(
                "Odin is not reachable at {}. \
                 Ensure the Yggdrasil orchestrator is running. Error: {}",
                config.odin_url, e
            ));
        }
    };

    let status = resp.status();
    if status == reqwest::StatusCode::CONFLICT {
        // Novelty gate fired — parse the match and return a tiebreak prompt
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        let dup_id = body["duplicate_id"].as_str().unwrap_or("unknown");
        let sim = body["similarity"].as_f64().unwrap_or(0.0);
        let existing_cause = body["existing_cause"].as_str().unwrap_or("");
        let existing_effect = body["existing_effect"].as_str().unwrap_or("");

        return tool_ok(format!(
            "Near-duplicate detected (similarity: {sim:.2}).\n\n\
             **Existing memory** (ID: {dup_id}):\n\
             Cause: {existing_cause}\n\
             Effect: {existing_effect}\n\n\
             **Your new memory:**\n\
             Cause: {}\n\
             Effect: {}\n\n\
             To UPDATE the existing memory, call store_memory again with id=\"{dup_id}\".\n\
             To CREATE a new separate memory, call store_memory again with force=true.",
            params.cause, params.effect
        ));
    } else if status.is_client_error() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Memory store failed (HTTP {}): {}", status, body));
    }
    if status.is_server_error() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Internal error from Odin (HTTP {}): {}", status, body));
    }

    let api: StoreApiResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse Odin response: {}", e)),
    };

    let id = api.id.unwrap_or_else(|| "(unknown)".to_string());
    tool_ok(format!("Memory stored successfully. ID: {}", id))
}

// ---------------------------------------------------------------------------
// Tool: memory_intersect
// ---------------------------------------------------------------------------

/// POST to Odin /api/v1/sdr/operations and return matching engrams.
#[instrument(skip(client, config), fields(op = %params.operation))]
pub async fn memory_intersect(
    client: &Client,
    config: &McpServerConfig,
    params: MemoryIntersectParams,
) -> CallToolResult {
    if params.texts.len() < 2 {
        return tool_error("at least 2 texts required".to_string());
    }
    for t in &params.texts {
        if t.len() > MAX_INPUT_BYTES {
            return tool_error(format!(
                "text exceeds maximum size of {MAX_INPUT_BYTES} bytes"
            ));
        }
    }

    let timeout = Duration::from_secs(config.timeout_secs);
    let url = format!(
        "{}/api/v1/sdr/operations",
        config.odin_url.trim_end_matches('/')
    );

    #[derive(Serialize)]
    struct Req<'a> {
        texts: &'a [String],
        operation: &'a str,
        limit: u32,
    }

    let body = Req {
        texts: &params.texts,
        operation: &params.operation,
        limit: params.limit.unwrap_or(5).min(20),
    };

    let resp = match client
        .post(&url)
        .json(&body)
        .timeout(timeout)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!(
                "Request timed out after {} seconds",
                config.timeout_secs
            ));
        }
        Err(e) => {
            return tool_error(format!(
                "Odin is not reachable at {}. Error: {}",
                config.odin_url, e
            ));
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!(
            "SDR operation failed (HTTP {}): {}",
            status, body
        ));
    }

    #[derive(Deserialize)]
    struct SdrOpEvent {
        id: Option<String>,
        similarity: Option<f64>,
        tier: Option<String>,
        tags: Option<Vec<String>>,
    }

    #[derive(Deserialize)]
    struct SdrOpResponse {
        jaccard: Option<f64>,
        combined_popcount: Option<u32>,
        events: Option<Vec<SdrOpEvent>>,
    }

    let api: SdrOpResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse response: {}", e)),
    };

    let jaccard = api.jaccard.unwrap_or(0.0);
    let popcount = api.combined_popcount.unwrap_or(0);
    let events = api.events.unwrap_or_default();

    let mut out = format!(
        "## SDR {} Results\n\n**Jaccard similarity:** {:.3}\n**Combined popcount:** {}/256\n\n",
        params.operation.to_uppercase(),
        jaccard,
        popcount,
    );

    if events.is_empty() {
        out.push_str("No matching engrams found.\n");
    } else {
        for (i, evt) in events.iter().enumerate() {
            let id = evt.id.as_deref().unwrap_or("?");
            let sim = evt.similarity.unwrap_or(0.0);
            let tier = evt.tier.as_deref().unwrap_or("?");
            let tags = evt
                .tags
                .as_ref()
                .map(|t| t.join(", "))
                .unwrap_or_default();
            out.push_str(&format!(
                "{}. `{}` (sim: {:.3}, tier: {}, tags: [{}])\n",
                i + 1,
                id,
                sim,
                tier,
                tags
            ));
        }
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: generate
// ---------------------------------------------------------------------------

/// Maximum prompt size (1MB) — generous but prevents abuse.
const MAX_PROMPT_BYTES: usize = 1024 * 1024;

/// Overhead budget (seconds) for Odin routing, RAG fetch, model loading, and
/// backend semaphore queueing. Added on top of the token-rate-based estimate.
const GENERATE_OVERHEAD_SECS: u64 = 30;

/// POST to Odin /v1/chat/completions (non-streaming) and return the response text.
#[instrument(skip(client, config), fields(model = ?params.model))]
pub async fn generate(
    client: &Client,
    config: &McpServerConfig,
    params: GenerateParams,
    session_id: Option<&str>,
    project_id: Option<&str>,
) -> CallToolResult {
    if params.prompt.len() > MAX_PROMPT_BYTES {
        return tool_error(format!(
            "prompt exceeds maximum size of {MAX_PROMPT_BYTES} bytes"
        ));
    }

    let max_tokens = params.max_tokens.unwrap_or(4096);
    let token_based_secs = if config.generate_tok_per_sec > 0.0 {
        (max_tokens as f64 / config.generate_tok_per_sec).ceil() as u64
    } else {
        0
    };
    let dynamic_timeout_secs = config.timeout_secs.max(token_based_secs + GENERATE_OVERHEAD_SECS);
    let timeout = Duration::from_secs(dynamic_timeout_secs);
    let url = format!(
        "{}/v1/chat/completions",
        config.odin_url.trim_end_matches('/')
    );

    #[derive(Serialize)]
    struct Message<'a> {
        role: &'a str,
        content: &'a str,
    }

    #[derive(Serialize)]
    struct Req<'a> {
        model: Option<&'a str>,
        messages: Vec<Message<'a>>,
        stream: bool,
        max_tokens: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<&'a str>,
    }

    let body = Req {
        model: params.model.as_deref(),
        messages: vec![Message {
            role: "user",
            content: &params.prompt,
        }],
        stream: false,
        max_tokens,
        session_id,
        project_id,
    };

    let resp = match client
        .post(&url)
        .json(&body)
        .timeout(timeout)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!(
                "Generation timed out after {}s ({}tok / {:.1}tok/s + {}s overhead)",
                dynamic_timeout_secs, max_tokens, config.generate_tok_per_sec, GENERATE_OVERHEAD_SECS
            ));
        }
        Err(e) => {
            return tool_error(format!(
                "Odin is not reachable at {}. \
                 Ensure the Yggdrasil orchestrator is running. Error: {}",
                config.odin_url, e
            ));
        }
    };

    let status = resp.status();
    if status.is_client_error() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Generate failed (HTTP {}): {}", status, body));
    }
    if status.is_server_error() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Internal error from Odin (HTTP {}): {}", status, body));
    }

    let api: ChatApiResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse Odin response: {}", e)),
    };

    let text = api
        .choices
        .unwrap_or_default()
        .into_iter()
        .next()
        .and_then(|c| c.message)
        .and_then(|m| m.content)
        .unwrap_or_else(|| "(empty response)".to_string());

    tool_ok(text)
}

// ---------------------------------------------------------------------------
// Tool: list_models
// ---------------------------------------------------------------------------

/// GET Odin /v1/models and format as a markdown table.
#[instrument(skip(client, config))]
pub async fn list_models(client: &Client, config: &McpServerConfig) -> CallToolResult {
    let timeout = Duration::from_secs(config.timeout_secs);
    let url = format!("{}/v1/models", config.odin_url.trim_end_matches('/'));

    let resp = match client.get(&url).timeout(timeout).send().await {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!(
                "Request timed out after {} seconds",
                config.timeout_secs
            ));
        }
        Err(e) => {
            return tool_error(format!(
                "Odin is not reachable at {}. \
                 Ensure the Yggdrasil orchestrator is running. Error: {}",
                config.odin_url, e
            ));
        }
    };

    let status = resp.status();
    if status.is_client_error() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Model listing failed (HTTP {}): {}", status, body));
    }
    if status.is_server_error() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Internal error from Odin (HTTP {}): {}", status, body));
    }

    let api: ModelsApiResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse Odin response: {}", e)),
    };

    let models = api.data.unwrap_or_default();
    if models.is_empty() {
        return tool_ok("## Available Models\n\nNo models found.");
    }

    let mut out = "## Available Models\n\n| Model | Backend |\n|-------|---------|".to_string();
    for m in &models {
        let id = m.id.as_deref().unwrap_or("(unknown)");
        let backend = m.owned_by.as_deref().unwrap_or("-");
        out.push_str(&format!("\n| {} | {} |", id, backend));
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: get_sprint_history
// ---------------------------------------------------------------------------

/// Query Mimir's dedicated `sprints` Qdrant collection for sprint engrams.
///
/// Uses `/api/v1/sprints/list` which searches the category-scoped `sprints`
/// collection with dense embeddings, then fetches full records from PostgreSQL.
#[instrument(skip(client, config), fields(project = ?params.project))]
pub async fn get_sprint_history(
    client: &Client,
    config: &McpServerConfig,
    params: GetSprintHistoryParams,
) -> CallToolResult {
    let timeout = Duration::from_secs(config.timeout_secs);
    let url = format!(
        "{}/api/v1/sprints/list",
        config.odin_url.trim_end_matches('/')
    );

    let limit = params.limit.unwrap_or(10).min(50);

    #[derive(Serialize)]
    struct Req<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        project: Option<&'a str>,
        limit: u32,
    }

    let body = Req {
        project: params.project.as_deref(),
        limit,
    };

    let resp = match client
        .post(&url)
        .json(&body)
        .timeout(timeout)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!(
                "Request timed out after {} seconds",
                config.timeout_secs
            ));
        }
        Err(e) => {
            return tool_error(format!(
                "Odin is not reachable at {}: {}",
                config.odin_url, e
            ));
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Sprint history query failed (HTTP {}): {}", status, body));
    }

    let api: QueryApiResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse response: {}", e)),
    };

    let results = api.results.unwrap_or_default();
    if results.is_empty() {
        let project = params.project.as_deref().unwrap_or("");
        let project_note = if project.is_empty() {
            String::new()
        } else {
            format!(" for project '{project}'")
        };
        return tool_ok(format!(
            "## Sprint History{}\n\nNo sprint engrams found in the sprints collection. \
             Ensure sprint memories are tagged with \"sprint\" so they are routed to the \
             dedicated sprints collection.",
            project_note
        ));
    }

    let project = params.project.as_deref().unwrap_or("");
    let project_note = if project.is_empty() {
        String::new()
    } else {
        format!(" — {project}")
    };
    let mut out = format!(
        "## Sprint History{} ({} results)\n\n",
        project_note,
        results.len()
    );
    for (i, engram) in results.iter().enumerate() {
        let cause = engram.cause.as_deref().unwrap_or("(unknown)");
        let effect = engram.effect.as_deref().unwrap_or("(unknown)");
        let sim = engram.similarity.unwrap_or(0.0);
        out.push_str(&format!(
            "### {}. {} [score: {:.2}]\n{}\n\n",
            i + 1,
            cause,
            sim,
            effect
        ));
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: sync_docs
// ---------------------------------------------------------------------------

/// Sprint lifecycle doc agent — updates USAGE.md on sprint_start, archives sprint on sprint_end.
///
/// Requires `workspace_path` to be set in the MCP server config. Makes local
/// file system reads/writes and calls Odin (Qwen3-Coder) for content generation.
#[instrument(skip(client, config), fields(event = %params.event, sprint_id = %params.sprint_id))]
pub async fn sync_docs(
    client: &Client,
    config: &McpServerConfig,
    params: SyncDocsParams,
    session_id: Option<&str>,
) -> CallToolResult {
    // Resolve workspace: param override → config fallback → error.
    let workspace = params
        .workspace_path
        .as_deref()
        .or(config.workspace_path.as_deref())
        .map(|p| p.trim_end_matches('/').to_string());

    let workspace = match workspace {
        Some(w) => w,
        None => {
            return tool_error(
                "No workspace_path provided. Pass it as a parameter or set it in config.",
            );
        }
    };

    if params.sprint_content.len() > MAX_PROMPT_BYTES {
        return tool_error(format!(
            "sprint_content exceeds maximum size of {MAX_PROMPT_BYTES} bytes"
        ));
    }

    match params.event.as_str() {
        "setup" => sync_docs_setup(client, config, &workspace, &params, session_id).await,
        "sprint_start" => sync_docs_sprint_start(client, config, &workspace, &params, session_id).await,
        "sprint_end" => sync_docs_sprint_end(client, config, &workspace, &params, session_id).await,
        other => tool_error(format!(
            "Unknown event '{}'. Use 'setup', 'sprint_start', or 'sprint_end'.",
            other
        )),
    }
}

/// Required docs that every workspace should have in /docs/.
const REQUIRED_DOCS: &[&str] = &[
    "ARCHITECTURE.md",
    "NAMING_CONVENTIONS.md",
    "USAGE.md",
];

/// Handle setup: initialize a new workspace's /docs/ and /sprints/ structure.
///
/// 1. Creates /docs/ and /sprints/ directories if missing.
/// 2. Scans existing /docs/ files — deletes stale ones that reference a different project.
/// 3. Scaffolds missing required docs via Odin using sprint_content as project context.
/// 4. Cleans /sprints/ of leftover files from previous projects.
async fn sync_docs_setup(
    client: &Client,
    config: &McpServerConfig,
    workspace: &str,
    params: &SyncDocsParams,
    session_id: Option<&str>,
) -> CallToolResult {
    let docs_dir = format!("{workspace}/docs");
    let sprints_dir = format!("{workspace}/sprints");
    let mut actions: Vec<String> = Vec::new();

    // Step 1: Ensure directories exist.
    for dir in [&docs_dir, &sprints_dir] {
        if !tokio::fs::try_exists(dir).await.unwrap_or(false) {
            if let Err(e) = tokio::fs::create_dir_all(dir).await {
                return tool_error(format!("Failed to create {dir}: {e}"));
            }
            actions.push(format!("Created {dir}"));
        }
    }

    // Step 2: Scan /docs/ — identify stale files.
    // A file is "stale" if it's a required doc but its content references a different project.
    // We ask Odin to evaluate staleness if sprint_content provides project context.
    let has_context = !params.sprint_content.is_empty();
    if let Ok(mut entries) = tokio::fs::read_dir(&docs_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let filename = entry.file_name().to_string_lossy().to_string();
            if !filename.ends_with(".md") {
                continue;
            }

            // Read existing file content.
            let path = entry.path();
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Skip empty files — they'll be regenerated below.
            if content.trim().is_empty() {
                let _ = tokio::fs::remove_file(&path).await;
                actions.push(format!("Removed empty {filename}"));
                continue;
            }

            // If we have project context, ask Odin if this doc is stale.
            if has_context && REQUIRED_DOCS.contains(&filename.as_str()) {
                let stale_check_prompt = format!(
                    "You are evaluating whether a documentation file is stale or belongs to a different project.\n\n\
                     The current project context:\n{context}\n\n\
                     The file '{filename}' contains:\n{content}\n\n\
                     Does this file appear to describe a DIFFERENT project than the current one? \
                     Answer ONLY 'STALE' if it clearly describes a different project, or 'CURRENT' if it's \
                     relevant or generic enough to keep. One word only.",
                    context = params.sprint_content,
                );

                let check_result = generate(
                    client,
                    config,
                    GenerateParams {
                        prompt: stale_check_prompt,
                        model: None,
                        max_tokens: Some(16),
                    },
                    session_id,
                    config.project.as_deref(),
                )
                .await;

                let response = check_result
                    .content
                    .into_iter()
                    .next()
                    .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
                    .unwrap_or_default();

                if response.trim().to_uppercase().contains("STALE") {
                    let _ = tokio::fs::remove_file(&path).await;
                    actions.push(format!("Removed stale {filename} (belongs to different project)"));
                }
            }
        }
    }

    // Step 3: Scaffold missing required docs.
    for filename in REQUIRED_DOCS {
        let path = format!("{docs_dir}/{filename}");
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            continue;
        }

        // Generate scaffold content if we have project context.
        let content = if has_context {
            let scaffold_prompt = format!(
                "You are a technical writer initializing documentation for a new project.\n\
                 Generate a minimal {filename} scaffold for this project.\n\
                 Project context:\n{context}\n\n\
                 Output ONLY the markdown content for {filename}. Keep it concise — \
                 this is a starting point that will be expanded during sprints.",
                context = params.sprint_content,
            );

            let gen_result = generate(
                client,
                config,
                GenerateParams {
                    prompt: scaffold_prompt,
                    model: None,
                    max_tokens: Some(2048),
                },
                session_id,
                config.project.as_deref(),
            )
            .await;

            gen_result
                .content
                .into_iter()
                .next()
                .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
                .unwrap_or_else(|| format!("# {}\n\nTODO: Document this project.\n", filename.trim_end_matches(".md")))
        } else {
            format!("# {}\n\nTODO: Document this project.\n", filename.trim_end_matches(".md"))
        };

        if let Err(e) = tokio::fs::write(&path, &content).await {
            actions.push(format!("WARNING: Failed to write {filename}: {e}"));
        } else {
            actions.push(format!("Scaffolded {filename}"));
        }
    }

    // Step 4: Clean /sprints/ — remove leftover sprint files.
    if let Ok(mut entries) = tokio::fs::read_dir(&sprints_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let filename = entry.file_name().to_string_lossy().to_string();
            if filename.ends_with(".md") {
                let path = entry.path();
                let _ = tokio::fs::remove_file(&path).await;
                actions.push(format!("Cleaned stale sprint file: {filename}"));
            }
        }
    }

    if actions.is_empty() {
        tool_ok(format!("Workspace {workspace} already set up. No changes needed."))
    } else {
        tool_ok(format!(
            "Workspace setup complete for {workspace}:\n\n{}",
            actions.join("\n")
        ))
    }
}

/// Handle sprint_start: update USAGE.md and check /docs/ + /sprints/ invariants.
///
/// Auto-triggers setup if /docs/ doesn't exist yet (new workspace).
async fn sync_docs_sprint_start(
    client: &Client,
    config: &McpServerConfig,
    workspace: &str,
    params: &SyncDocsParams,
    session_id: Option<&str>,
) -> CallToolResult {
    let docs_dir = format!("{workspace}/docs");

    // Auto-setup: if /docs/ doesn't exist, this is a new workspace.
    if !tokio::fs::try_exists(&docs_dir).await.unwrap_or(false) {
        tracing::info!(workspace, "New workspace detected — running auto-setup");
        let setup_result = sync_docs_setup(client, config, workspace, params, session_id).await;
        if setup_result.is_error.unwrap_or(false) {
            return setup_result;
        }
        // Log setup actions but continue with sprint_start.
        if let Some(text) = setup_result
            .content.first()
            .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
        {
            tracing::info!("Auto-setup: {text}");
        }
    }

    let usage_path = format!("{docs_dir}/USAGE.md");

    // Read or initialise USAGE.md.
    let current_usage = match tokio::fs::read_to_string(&usage_path).await {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return tool_error(format!("Failed to read USAGE.md: {e}")),
    };

    // Ask Odin (Qwen3-Coder) to update USAGE.md based on the sprint plan.
    let prompt = format!(
        "You are a technical writer updating USAGE.md for a software project.\n\
         Given the sprint plan below, update USAGE.md to add or modify any new \
         API endpoints, startup commands, or deployment commands introduced in this sprint.\n\
         Preserve ALL existing content. Output the FULL updated USAGE.md only — no commentary.\n\n\
         ## Current USAGE.md\n{current_usage}\n\n\
         ## Sprint Plan (Sprint {sprint_id})\n{sprint_content}",
        sprint_id = params.sprint_id,
        sprint_content = params.sprint_content,
    );

    let gen_result = generate(
        client,
        config,
        GenerateParams {
            prompt,
            model: None, // Let Odin route to Qwen3-Coder
            max_tokens: Some(8192),
        },
        session_id,
        config.project.as_deref(),
    )
    .await;

    if gen_result.is_error.unwrap_or(false) {
        return gen_result;
    }

    let updated_usage = gen_result
        .content
        .into_iter()
        .next()
        .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
        .unwrap_or_default();

    if updated_usage.is_empty() {
        return tool_error("LLM returned empty USAGE.md content.");
    }

    // Write updated USAGE.md.
    if let Err(e) = tokio::fs::create_dir_all(&docs_dir).await {
        return tool_error(format!("Failed to create docs/ directory: {e}"));
    }
    if let Err(e) = tokio::fs::write(&usage_path, &updated_usage).await {
        return tool_error(format!("Failed to write USAGE.md: {e}"));
    }

    // Check /docs/ for required files.
    let required_docs = ["ARCHITECTURE.md", "NetworkHardware.md", "NAMING_CONVENTIONS.md", "USAGE.md"];
    let mut warnings: Vec<String> = Vec::new();
    for filename in &required_docs {
        let path = format!("{docs_dir}/{filename}");
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            warnings.push(format!("WARNING: /docs/{filename} is missing"));
        }
    }

    // Check /sprints/ for single-file invariant.
    let sprints_dir = format!("{workspace}/sprints");
    if let Ok(mut entries) = tokio::fs::read_dir(&sprints_dir).await {
        let mut count = 0usize;
        while let Ok(Some(_)) = entries.next_entry().await {
            count += 1;
        }
        if count > 1 {
            warnings.push(format!(
                "WARNING: /sprints/ contains {count} files. Sprint discipline requires exactly 1 active sprint file."
            ));
        }
    }

    let mut result = format!(
        "Sprint {} started.\n\nUSAGE.md updated ({} bytes).",
        params.sprint_id,
        updated_usage.len()
    );
    if !warnings.is_empty() {
        result.push_str("\n\n");
        result.push_str(&warnings.join("\n"));
    }

    tool_ok(result)
}

/// Handle sprint_end: archive sprint to Mimir, update ARCHITECTURE.md, delete sprint file.
async fn sync_docs_sprint_end(
    client: &Client,
    config: &McpServerConfig,
    workspace: &str,
    params: &SyncDocsParams,
    session_id: Option<&str>,
) -> CallToolResult {
    let project = config.project.as_deref().unwrap_or("unknown");

    // Step 1: Generate a concise sprint summary via Odin.
    let summary_prompt = format!(
        "Summarize this sprint plan into 3-5 bullet points covering: \
         key features added, breaking changes, deployment steps required, and gotchas found. \
         Output only the bullet points, no headers.\n\n\
         Sprint Plan (Sprint {sprint_id}):\n{sprint_content}",
        sprint_id = params.sprint_id,
        sprint_content = params.sprint_content,
    );

    let summary_result = generate(
        client,
        config,
        GenerateParams {
            prompt: summary_prompt,
            model: None,
            max_tokens: Some(1024),
        },
        session_id,
        config.project.as_deref(),
    )
    .await;

    let summary_text = if summary_result.is_error.unwrap_or(false) {
        // Fall back to raw sprint content if summarization fails.
        params.sprint_content.chars().take(2000).collect::<String>()
    } else {
        summary_result
            .content
            .into_iter()
            .next()
            .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
            .unwrap_or_else(|| params.sprint_content.chars().take(2000).collect())
    };

    // Step 2: Archive sprint to Mimir via Odin proxy.
    let store_url = format!(
        "{}/api/v1/store",
        config.odin_url.trim_end_matches('/')
    );
    let tags = vec![
        "sprint".to_string(),
        format!("project:{project}"),
        format!("sprint:{}", params.sprint_id),
    ];
    let cause = format!("Sprint {}: archived", params.sprint_id);

    #[derive(Serialize)]
    struct StoreReq<'a> {
        cause: &'a str,
        effect: &'a str,
        tags: &'a [String],
    }

    let store_body = StoreReq {
        cause: &cause,
        effect: &summary_text,
        tags: &tags,
    };

    let timeout = Duration::from_secs(config.timeout_secs);
    let store_resp = match client
        .post(&store_url)
        .json(&store_body)
        .timeout(timeout)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return tool_error(format!("Failed to store sprint engram in Mimir: {e}")),
    };

    let engram_id = if store_resp.status().is_success() {
        let api: StoreApiResponse = store_resp.json().await.unwrap_or(StoreApiResponse { id: None });
        api.id.unwrap_or_else(|| "(unknown)".to_string())
    } else {
        let body = store_resp.text().await.unwrap_or_default();
        return tool_error(format!("Mimir store failed: {body}"));
    };

    // Step 3: Generate ARCHITECTURE.md delta from sprint content.
    let arch_path = format!("{workspace}/docs/ARCHITECTURE.md");
    if let Ok(current_arch) = tokio::fs::read_to_string(&arch_path).await {
        let arch_prompt = format!(
            "Given this sprint plan, output ONLY the new or changed sections \
             of ARCHITECTURE.md that this sprint introduces. \
             If there are no architectural changes, output exactly: NO_CHANGES\n\n\
             Sprint Plan (Sprint {sprint_id}):\n{sprint_content}",
            sprint_id = params.sprint_id,
            sprint_content = params.sprint_content,
        );

        let arch_result = generate(
            client,
            config,
            GenerateParams {
                prompt: arch_prompt,
                model: None,
                max_tokens: Some(2048),
            },
            session_id,
            config.project.as_deref(),
        )
        .await;

        if let Some(content) = arch_result
            .content
            .into_iter()
            .next()
            .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
            && !content.contains("NO_CHANGES") && !content.trim().is_empty() {
                let updated_arch = format!(
                    "{}\n\n## Sprint {} Changes\n\n{}",
                    current_arch.trim_end(),
                    params.sprint_id,
                    content
                );
                let _ = tokio::fs::write(&arch_path, &updated_arch).await;
            }
    }

    // Step 4: Delete the sprint file.
    let sprint_file = format!("{workspace}/sprints/sprint-{}.md", params.sprint_id);
    let deleted_file = if tokio::fs::try_exists(&sprint_file).await.unwrap_or(false) {
        match tokio::fs::remove_file(&sprint_file).await {
            Ok(()) => format!("Deleted {sprint_file}"),
            Err(e) => format!("WARNING: Could not delete {sprint_file}: {e}"),
        }
    } else {
        // Try underscore variant for legacy naming.
        let alt_file = format!("{workspace}/sprints/sprint_{}.md", params.sprint_id);
        if tokio::fs::try_exists(&alt_file).await.unwrap_or(false) {
            match tokio::fs::remove_file(&alt_file).await {
                Ok(()) => format!("Deleted {alt_file}"),
                Err(e) => format!("WARNING: Could not delete {alt_file}: {e}"),
            }
        } else {
            format!("No sprint file found at {sprint_file} or {alt_file}")
        }
    };

    tool_ok(format!(
        "Sprint {} archived.\n\nEngram ID: {}\nProject: {}\n\n{}\n\nSummary stored:\n{}",
        params.sprint_id,
        engram_id,
        project,
        deleted_file,
        summary_text
    ))
}

/// Format the models table as a plain string (shared with the resource handler).
pub async fn models_table(client: &Client, config: &McpServerConfig) -> String {
    let result = list_models(client, config).await;
    result
        .content
        .into_iter()
        .next()
        .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
        .unwrap_or_else(|| "Model listing unavailable.".to_string())
}

// ---------------------------------------------------------------------------
// Tool: service_health
// ---------------------------------------------------------------------------

/// Service endpoint definition for health probing.
struct ServiceEndpoint {
    name: &'static str,
    url: String,
}

/// Probe all Yggdrasil services and return a status table.
#[instrument(skip_all)]
pub async fn service_health(
    client: &Client,
    config: &McpServerConfig,
    params: ServiceHealthParams,
) -> CallToolResult {
    let filter = params.services.as_ref();

    let mut endpoints = vec![
        ServiceEndpoint {
            name: "odin",
            url: format!("{}/health", config.odin_url.trim_end_matches('/')),
        },
    ];

    // Mimir is on the same host as Odin for now — derive from odin_url by swapping port.
    // The typical topology is Odin:8080, Mimir:9090 on Munin.
    let mimir_url = config.odin_url.replace(":8080", ":9090");
    endpoints.push(ServiceEndpoint {
        name: "mimir",
        url: format!("{}/health", mimir_url.trim_end_matches('/')),
    });

    if let Some(ref muninn_url) = config.muninn_url {
        endpoints.push(ServiceEndpoint {
            name: "muninn",
            url: format!("{}/health", muninn_url.trim_end_matches('/')),
        });
    }

    // Ollama backends — derive hosts from config URLs, probe /api/tags on port 11434
    let munin_ip = std::env::var("MUNIN_IP").unwrap_or_else(|_| "localhost".to_string());
    endpoints.push(ServiceEndpoint {
        name: "ollama_munin",
        url: format!("http://{}:11434/api/tags", munin_ip),
    });

    if config.muninn_url.is_some() {
        let hugin_ip = std::env::var("HUGIN_IP").unwrap_or_else(|_| "localhost".to_string());
        endpoints.push(ServiceEndpoint {
            name: "ollama_hugin",
            url: format!("http://{}:11434/api/tags", hugin_ip),
        });
    }

    // Qdrant
    let hades_ip = std::env::var("HADES_IP").unwrap_or_else(|_| "localhost".to_string());
    endpoints.push(ServiceEndpoint {
        name: "qdrant",
        url: format!("http://{}:6333/collections", hades_ip),
    });

    // Filter if specific services requested
    let endpoints: Vec<_> = if let Some(filter_list) = filter {
        endpoints
            .into_iter()
            .filter(|e| filter_list.iter().any(|f| f == e.name))
            .collect()
    } else {
        endpoints
    };

    if endpoints.is_empty() {
        return tool_error("No matching services found. Valid: odin, mimir, muninn, ollama_munin, ollama_hugin, qdrant");
    }

    // Probe all endpoints in parallel
    let probe_timeout = Duration::from_secs(3);
    let mut handles = Vec::new();
    for ep in &endpoints {
        let c = client.clone();
        let url = ep.url.clone();
        let name = ep.name;
        handles.push(tokio::spawn(async move {
            let start = std::time::Instant::now();
            match c.get(&url).timeout(probe_timeout).send().await {
                Ok(resp) => {
                    let latency = start.elapsed().as_millis();
                    let status = resp.status();
                    if status.is_success() {
                        (name, "up", latency as u64, String::new())
                    } else {
                        (name, "degraded", latency as u64, format!("HTTP {}", status))
                    }
                }
                Err(e) if e.is_timeout() => {
                    (name, "down", 3000, "timeout (3s)".to_string())
                }
                Err(e) => {
                    let latency = start.elapsed().as_millis();
                    (name, "down", latency as u64, format!("{}", e))
                }
            }
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        if let Ok(result) = handle.await {
            results.push(result);
        }
    }

    let up_count = results.iter().filter(|r| r.1 == "up").count();
    let total = results.len();

    let mut out = format!(
        "## Service Health ({}/{} up)\n\n| Service | Status | Latency | Error |\n|---------|--------|---------|-------|\n",
        up_count, total
    );
    for (name, status, latency, error) in &results {
        let status_icon = match *status {
            "up" => "UP",
            "degraded" => "DEGRADED",
            _ => "DOWN",
        };
        out.push_str(&format!(
            "| {} | {} | {}ms | {} |\n",
            name, status_icon, latency, error
        ));
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: build_check
// ---------------------------------------------------------------------------

/// Run cargo check/clippy/test and return structured diagnostics.
#[instrument(skip_all)]
pub async fn build_check(
    _client: &Client,
    config: &McpServerConfig,
    params: BuildCheckParams,
) -> CallToolResult {
    let mode = params.mode.as_deref().unwrap_or("check");

    // Resolve workspace path for the build
    let workspace = match &config.workspace_path {
        Some(w) => w.clone(),
        None => {
            return tool_error(
                "No workspace_path configured. Set workspace_path in MCP server config.",
            );
        }
    };

    // Resolve cargo binary — the MCP server may run as a service user without
    // cargo in PATH. Check $CARGO, then common rustup install locations.
    let cargo_bin = {
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(c) = std::env::var("CARGO") {
            candidates.push(c);
        }
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(format!("{home}/.cargo/bin/cargo"));
        }
        // Also check the workspace owner's cargo (for service-user execution)
        if let Some(ref ws) = config.workspace_path {
            // Walk up to find a .cargo/bin/cargo relative to repo owner's home
            let ws_path = std::path::Path::new(ws);
            for ancestor in ws_path.ancestors() {
                let candidate = ancestor.join(".cargo/bin/cargo");
                if candidate.exists() {
                    candidates.push(candidate.to_string_lossy().into_owned());
                    break;
                }
            }
        }
        candidates.push("/usr/local/bin/cargo".to_string());
        candidates.push("/usr/bin/cargo".to_string());
        candidates
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .unwrap_or_else(|| "cargo".to_string())
    };

    let mut cmd_args = vec![cargo_bin];
    match mode {
        "check" => cmd_args.push("check".to_string()),
        "clippy" => {
            cmd_args.push("clippy".to_string());
            cmd_args.push("--all-targets".to_string());
        }
        "test" => {
            cmd_args.push("test".to_string());
            cmd_args.push("--no-run".to_string());
        }
        other => {
            return tool_error(format!(
                "Unknown mode '{}'. Use 'check', 'clippy', or 'test'.",
                other
            ));
        }
    }

    cmd_args.push("--message-format=json".to_string());

    if let Some(ref krate) = params.crate_name {
        cmd_args.push("-p".to_string());
        cmd_args.push(krate.clone());
    }

    let output = match tokio::process::Command::new(&cmd_args[0])
        .args(&cmd_args[1..])
        .current_dir(&workspace)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            return tool_error(format!("Failed to run cargo: {}", e));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Parse JSON messages from cargo
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    for line in stdout.lines() {
        if let Ok(msg) = serde_json::from_str::<serde_json::Value>(line)
            && msg.get("reason").and_then(|r| r.as_str()) == Some("compiler-message")
                && let Some(message) = msg.get("message") {
                    let level = message
                        .get("level")
                        .and_then(|l| l.as_str())
                        .unwrap_or("");
                    let text = message
                        .get("rendered")
                        .and_then(|r| r.as_str())
                        .unwrap_or("");

                    if !text.is_empty() {
                        match level {
                            "error" => errors.push(text.to_string()),
                            "warning" => warnings.push(text.to_string()),
                            _ => {}
                        }
                    }
                }
    }

    let success = output.status.success();
    let mut out = format!(
        "## Build Check ({})\n\n**Result:** {}\n**Errors:** {}\n**Warnings:** {}\n\n",
        mode,
        if success { "PASS" } else { "FAIL" },
        errors.len(),
        warnings.len(),
    );

    if !errors.is_empty() {
        out.push_str("### Errors\n\n");
        for (i, err) in errors.iter().take(10).enumerate() {
            out.push_str(&format!("{}. ```\n{}\n```\n\n", i + 1, err.trim()));
        }
        if errors.len() > 10 {
            out.push_str(&format!("... and {} more errors\n\n", errors.len() - 10));
        }
    }

    if !warnings.is_empty() {
        out.push_str("### Warnings\n\n");
        for (i, warn) in warnings.iter().take(10).enumerate() {
            out.push_str(&format!("{}. ```\n{}\n```\n\n", i + 1, warn.trim()));
        }
        if warnings.len() > 10 {
            out.push_str(&format!(
                "... and {} more warnings\n\n",
                warnings.len() - 10
            ));
        }
    }

    // If no JSON diagnostics but stderr has content, include it
    if errors.is_empty() && warnings.is_empty() && !success {
        out.push_str(&format!("### stderr\n```\n{}\n```\n", stderr.trim()));
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: memory_timeline
// ---------------------------------------------------------------------------

/// Query engrams with temporal and tag filters.
#[instrument(skip(client, config))]
pub async fn memory_timeline(
    client: &Client,
    config: &McpServerConfig,
    params: MemoryTimelineParams,
) -> CallToolResult {
    let timeout = Duration::from_secs(config.timeout_secs);

    // Build the timeline query endpoint on Mimir (via Odin proxy)
    let url = format!(
        "{}/api/v1/timeline",
        config.odin_url.trim_end_matches('/')
    );

    #[derive(Serialize)]
    struct Req<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        after: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        before: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tags: Option<&'a Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tier: Option<&'a str>,
        limit: u32,
    }

    let body = Req {
        text: params.text.as_deref(),
        after: params.after.as_deref(),
        before: params.before.as_deref(),
        tags: params.tags.as_ref(),
        tier: params.tier.as_deref(),
        limit: params.limit.unwrap_or(10).min(50),
    };

    let resp = match client
        .post(&url)
        .json(&body)
        .timeout(timeout)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!(
                "Request timed out after {} seconds",
                config.timeout_secs
            ));
        }
        Err(e) => {
            return tool_error(format!(
                "Timeline query failed. Odin not reachable at {}: {}",
                config.odin_url, e
            ));
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Timeline query failed (HTTP {}): {}", status, body));
    }

    #[derive(Deserialize)]
    struct TimelineEngram {
        cause: Option<String>,
        effect: Option<String>,
        tier: Option<String>,
        tags: Option<Vec<String>>,
        created_at: Option<String>,
    }

    #[derive(Deserialize)]
    struct TimelineResponse {
        results: Option<Vec<TimelineEngram>>,
    }

    let api: TimelineResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse timeline response: {}", e)),
    };

    let results = api.results.unwrap_or_default();
    if results.is_empty() {
        return tool_ok("## Memory Timeline\n\nNo engrams found matching filters.");
    }

    let mut out = format!("## Memory Timeline ({} results)\n\n", results.len());
    for (i, engram) in results.iter().enumerate() {
        let cause = engram.cause.as_deref().unwrap_or("(unknown)");
        let effect = engram.effect.as_deref().unwrap_or("(unknown)");
        let tier = engram.tier.as_deref().unwrap_or("?");
        let created = engram.created_at.as_deref().unwrap_or("?");
        let tags = engram
            .tags
            .as_ref()
            .map(|t| t.join(", "))
            .unwrap_or_default();

        out.push_str(&format!(
            "### {}. {} [{}] ({})\n**Tags:** [{}]\n**Effect:** {}\n\n",
            i + 1,
            cause,
            tier,
            created,
            tags,
            effect,
        ));
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: context_offload
// ---------------------------------------------------------------------------

/// Store, retrieve, or list offloaded context blocks.
#[instrument(skip(client, config))]
pub async fn context_offload(
    client: &Client,
    config: &McpServerConfig,
    params: ContextOffloadParams,
) -> CallToolResult {
    let timeout = Duration::from_secs(config.timeout_secs);
    let base = format!(
        "{}/api/v1/context",
        config.odin_url.trim_end_matches('/')
    );

    match params.action.as_str() {
        "store" => {
            let content = match &params.content {
                Some(c) if !c.is_empty() => c,
                _ => return tool_error("'content' is required for action 'store'"),
            };
            if content.len() > MAX_PROMPT_BYTES {
                return tool_error(format!(
                    "content exceeds maximum size of {MAX_PROMPT_BYTES} bytes"
                ));
            }

            #[derive(Serialize)]
            struct StoreReq<'a> {
                content: &'a str,
                #[serde(skip_serializing_if = "Option::is_none")]
                label: Option<&'a str>,
            }

            let body = StoreReq {
                content,
                label: params.label.as_deref(),
            };

            let resp = match client
                .post(&base)
                .json(&body)
                .timeout(timeout)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Context store failed: {}", e)),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!("Context store failed (HTTP {}): {}", status, body));
            }

            #[derive(Deserialize)]
            struct StoreResp {
                handle: Option<String>,
            }

            let api: StoreResp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Failed to parse response: {}", e)),
            };

            let handle = api.handle.unwrap_or_else(|| "?".to_string());
            let label = params.label.as_deref().unwrap_or("(unlabeled)");
            let size = content.len();
            tool_ok(format!(
                "Context stored. Handle: `{}` | Label: {} | Size: {} bytes",
                handle, label, size
            ))
        }
        "retrieve" => {
            let handle = match &params.handle {
                Some(h) if !h.is_empty() => h,
                _ => return tool_error("'handle' is required for action 'retrieve'"),
            };

            let url = format!("{}/{}", base, handle);
            let resp = match client.get(&url).timeout(timeout).send().await {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Context retrieve failed: {}", e)),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!("Context not found (HTTP {}): {}", status, body));
            }

            #[derive(Deserialize)]
            struct RetrieveResp {
                content: Option<String>,
                label: Option<String>,
            }

            let api: RetrieveResp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Failed to parse response: {}", e)),
            };

            let content = api.content.unwrap_or_default();
            let label = api.label.unwrap_or_else(|| "(unlabeled)".to_string());
            tool_ok(format!(
                "## Context: {} (handle: `{}`)\n\n{}",
                label, handle, content
            ))
        }
        "list" => {
            let resp = match client.get(&base).timeout(timeout).send().await {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Context list failed: {}", e)),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!("Context list failed (HTTP {}): {}", status, body));
            }

            #[derive(Deserialize)]
            struct ListItem {
                handle: Option<String>,
                label: Option<String>,
                size: Option<usize>,
            }

            #[derive(Deserialize)]
            struct ListResp {
                items: Option<Vec<ListItem>>,
            }

            let api: ListResp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Failed to parse response: {}", e)),
            };

            let items = api.items.unwrap_or_default();
            if items.is_empty() {
                return tool_ok("## Offloaded Contexts\n\nNo stored contexts.");
            }

            let mut out = format!(
                "## Offloaded Contexts ({} items)\n\n| Handle | Label | Size |\n|--------|-------|------|\n",
                items.len()
            );
            for item in &items {
                let handle = item.handle.as_deref().unwrap_or("?");
                let label = item.label.as_deref().unwrap_or("-");
                let size = item.size.unwrap_or(0);
                out.push_str(&format!("| `{}` | {} | {} bytes |\n", handle, label, size));
            }
            tool_ok(out)
        }
        other => tool_error(format!(
            "Unknown action '{}'. Use 'store', 'retrieve', or 'list'.",
            other
        )),
    }
}

// ---------------------------------------------------------------------------
// Tool: task_delegate
// ---------------------------------------------------------------------------

/// Assemble project context and delegate code generation to local LLM.
#[instrument(skip(client, config), fields(task = %params.task))]
pub async fn task_delegate(
    client: &Client,
    config: &McpServerConfig,
    params: TaskDelegateParams,
    session_id: Option<&str>,
    project_id: Option<&str>,
) -> CallToolResult {
    if params.task.len() > MAX_INPUT_BYTES {
        return tool_error(format!(
            "task exceeds maximum size of {MAX_INPUT_BYTES} bytes"
        ));
    }

    // Phase 1: Parallel context assembly
    let code_future = {
        let c = client.clone();
        let cfg = config.clone();
        let query = params.task.clone();
        let langs = params.language.as_ref().map(|l| vec![l.clone()]);
        tokio::spawn(async move {
            let p = SearchCodeParams {
                query,
                languages: langs,
                limit: Some(5),
            };
            search_code(&c, &cfg, p).await
        })
    };

    let memory_future = {
        let c = client.clone();
        let cfg = config.clone();
        let text = params.task.clone();
        tokio::spawn(async move {
            let p = QueryMemoryParams {
                text,
                limit: Some(5),
            };
            query_memory(&c, &cfg, p).await
        })
    };

    // Wait for both with a timeout
    let (code_result, memory_result) = tokio::join!(code_future, memory_future);

    let code_context = code_result
        .ok()
        .map(|r| {
            r.content
                .into_iter()
                .next()
                .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
                .unwrap_or_default()
        })
        .unwrap_or_default();

    let memory_context = memory_result
        .ok()
        .map(|r| {
            r.content
                .into_iter()
                .next()
                .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
                .unwrap_or_default()
        })
        .unwrap_or_default();

    // Phase 2: Build prompt
    let language = params.language.as_deref().unwrap_or("rust");
    let mut prompt = format!(
        "You are implementing {} code for a project.\n\n## Task\n{}\n",
        language, params.task
    );

    if !code_context.is_empty() {
        prompt.push_str(&format!(
            "\n## Existing Code Context\n{}\n",
            code_context
        ));
    }

    if !memory_context.is_empty() {
        prompt.push_str(&format!(
            "\n## Project Memory (architectural decisions)\n{}\n",
            memory_context
        ));
    }

    if let Some(ref pattern) = params.reference_pattern {
        prompt.push_str(&format!(
            "\n## Reference Pattern (follow this style exactly)\n```{}\n{}\n```\n",
            language, pattern
        ));
    }

    if let Some(ref constraints) = params.constraints {
        prompt.push_str("\n## Constraints\n");
        for c in constraints {
            prompt.push_str(&format!("- {}\n", c));
        }
    }

    prompt.push_str("\nOutput ONLY the implementation code. No explanations, no markdown fences wrapping the whole output.");

    // Phase 3: Delegate to local LLM
    let max_tokens = params.max_tokens.unwrap_or(8192);
    let gen_params = GenerateParams {
        prompt,
        model: params.model.clone(),
        max_tokens: Some(max_tokens),
    };

    let result = generate(client, config, gen_params, session_id, project_id).await;

    let generated = result
        .content
        .into_iter()
        .next()
        .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
        .unwrap_or_else(|| "(empty response)".to_string());

    let model_used = params.model.as_deref().unwrap_or("(default routing)");
    let mut out = format!(
        "## Task Delegate Result\n\n**Task:** {}\n**Model:** {}\n**Language:** {}\n**Context sources:** code search + memory\n\n### Generated Code\n\n{}\n",
        params.task, model_used, language, generated
    );

    if params.reference_pattern.is_some() {
        out.push_str("\n*Reference pattern was provided and included in prompt.*\n");
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: delegate (unified — replaces generate + task_delegate)
// ---------------------------------------------------------------------------

/// Parse markdown code blocks with file paths into (path, content) pairs.
///
/// Expected format:
/// ```path/to/file.rs
/// fn main() {}
/// ```
pub fn parse_file_blocks(content: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if let Some(path) = trimmed.strip_prefix("```") {
            let path = path.trim();
            // Skip blocks with no file path or generic language tags
            if path.is_empty() || !path.contains('.') {
                for inner in lines.by_ref() {
                    if inner.trim() == "```" {
                        break;
                    }
                }
                continue;
            }

            let file_path = path.to_string();
            let mut block_content = String::new();

            for inner in lines.by_ref() {
                if inner.trim() == "```" {
                    break;
                }
                if !block_content.is_empty() {
                    block_content.push('\n');
                }
                block_content.push_str(inner);
            }

            if !file_path.is_empty() && !block_content.is_empty() {
                results.push((file_path, block_content));
            }
        }
    }

    results
}

/// Unified delegate tool — assembles context, calls local LLM, returns
/// structured or raw output.
#[instrument(skip(client, config))]
pub async fn delegate(
    client: &Client,
    config: &McpServerConfig,
    params: DelegateParams,
    session_id: Option<&str>,
    project_id: Option<&str>,
) -> CallToolResult {
    use crate::agent_prompts;

    if params.instructions.len() > MAX_INPUT_BYTES {
        return tool_error(format!(
            "instructions exceed maximum size of {MAX_INPUT_BYTES} bytes"
        ));
    }

    let agent_type = params.agent_type.as_deref().unwrap_or("general");

    // Load agent system prompt (re-read from disk each call).
    let workspace = config
        .workspace_path
        .as_deref()
        .unwrap_or("/home/jesushernandez/Documents/Code/Yggdrasil/yggdrasil");
    let prompt_config = agent_prompts::load_prompt(
        std::path::Path::new(workspace),
        agent_type,
    );

    // Phase 1: Parallel context assembly
    let code_future = if let Some(ref queries) = params.search_queries {
        let c = client.clone();
        let cfg = config.clone();
        let queries = queries.clone();
        let langs = params.language.as_ref().map(|l| vec![l.clone()]);
        Some(tokio::spawn(async move {
            let mut combined = String::new();
            for q in queries {
                let p = SearchCodeParams {
                    query: q,
                    languages: langs.clone(),
                    limit: Some(5),
                };
                let result = search_code(&c, &cfg, p).await;
                let text = result
                    .content
                    .into_iter()
                    .next()
                    .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
                    .unwrap_or_default();
                if !text.is_empty() {
                    if !combined.is_empty() {
                        combined.push_str("\n---\n");
                    }
                    combined.push_str(&text);
                }
            }
            combined
        }))
    } else {
        None
    };

    let memory_future = if let Some(ref ids) = params.memory_ids {
        let c = client.clone();
        let odin_url = config.odin_url.trim_end_matches('/').to_string();
        let timeout = Duration::from_secs(config.timeout_secs);
        let ids = ids.clone();
        Some(tokio::spawn(async move {
            let mut combined = String::new();
            for id in ids {
                let url = format!("{}/api/v1/engrams/{}", odin_url, id);
                match c.get(&url).timeout(timeout).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        if let Ok(body) = resp.text().await {
                            if !combined.is_empty() {
                                combined.push_str("\n---\n");
                            }
                            combined.push_str(&body);
                        }
                    }
                    _ => {} // Skip failed fetches
                }
            }
            combined
        }))
    } else {
        None
    };

    // Await both futures
    let code_context = match code_future {
        Some(f) => f.await.unwrap_or_default(),
        None => String::new(),
    };

    let memory_context = match memory_future {
        Some(f) => f.await.unwrap_or_default(),
        None => String::new(),
    };

    // Phase 2: Build prompt with token budgets
    let budget = &prompt_config.budget;
    let mut system_prompt = prompt_config.prompt.system.clone();

    // Append constraints from config + params
    if !prompt_config.prompt.constraints.is_empty() || params.constraints.is_some() {
        system_prompt.push_str("\n\n## Constraints\n");
        for c in &prompt_config.prompt.constraints {
            system_prompt.push_str(&format!("- {}\n", c));
        }
        if let Some(ref extra) = params.constraints {
            for c in extra {
                system_prompt.push_str(&format!("- {}\n", c));
            }
        }
    }

    let mut user_prompt = format!("## Task\n{}\n", params.instructions);

    if !memory_context.is_empty() {
        let truncated = agent_prompts::truncate_to_budget(
            &memory_context,
            budget.max_memory_tokens,
        );
        user_prompt.push_str(&format!(
            "\n## Project Memory\n{}\n",
            truncated
        ));
    }

    if !code_context.is_empty() {
        let truncated = agent_prompts::truncate_to_budget(
            &code_context,
            budget.max_code_tokens,
        );
        user_prompt.push_str(&format!(
            "\n## Code Context\n{}\n",
            truncated
        ));
    }

    if let Some(ref files) = params.file_context {
        let mut file_section = String::new();
        for fc in files {
            file_section.push_str(&format!(
                "\n### {}\n```\n{}\n```\n",
                fc.path, fc.content
            ));
        }
        let truncated = agent_prompts::truncate_to_budget(
            &file_section,
            budget.max_file_tokens,
        );
        user_prompt.push_str(&format!("\n## File Context\n{}\n", truncated));
    }

    if let Some(ref pattern) = params.reference_pattern {
        let language = params.language.as_deref().unwrap_or("rust");
        user_prompt.push_str(&format!(
            "\n## Reference Pattern (follow this style exactly)\n```{}\n{}\n```\n",
            language, pattern
        ));
    }

    // Phase 3: Call Odin
    let max_tokens = params.max_tokens.unwrap_or(8192);

    #[derive(Serialize)]
    struct Message {
        role: String,
        content: String,
    }

    #[derive(Serialize)]
    struct ToolDef {
        #[serde(rename = "type")]
        tool_type: String,
        function: FnDef,
    }

    #[derive(Serialize)]
    struct FnDef {
        name: String,
        description: String,
        parameters: serde_json::Value,
    }

    #[derive(Serialize)]
    struct Req {
        model: Option<String>,
        messages: Vec<Message>,
        stream: bool,
        max_tokens: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<ToolDef>>,
    }

    let token_based_secs = if config.generate_tok_per_sec > 0.0 {
        (max_tokens as f64 / config.generate_tok_per_sec).ceil() as u64
    } else {
        0
    };
    let dynamic_timeout_secs = config.timeout_secs.max(token_based_secs + GENERATE_OVERHEAD_SECS);
    let timeout = Duration::from_secs(dynamic_timeout_secs);

    let url = format!(
        "{}/v1/chat/completions",
        config.odin_url.trim_end_matches('/')
    );

    // Build tool definitions for agentic mode if requested.
    let tools = if params.agentic.unwrap_or(false) {
        // Use Odin's tool registry format. When allowed_tools is specified,
        // only include those tools; otherwise include all safe-tier tools.
        let all_safe_tools: Vec<(&str, &str, serde_json::Value)> = vec![
            ("search_code", "Search the codebase using semantic and keyword search", serde_json::json!({"type":"object","properties":{"query":{"type":"string"},"languages":{"type":"array","items":{"type":"string"}},"limit":{"type":"integer"}},"required":["query"]})),
            ("query_memory", "Search engram memory for relevant past context", serde_json::json!({"type":"object","properties":{"text":{"type":"string"},"limit":{"type":"integer"}},"required":["text"]})),
            ("ast_analyze", "Look up code symbols using AST analysis", serde_json::json!({"type":"object","properties":{"query":{"type":"string"},"filters":{"type":"array","items":{"type":"string"}}},"required":["query"]})),
            ("impact_analysis", "Find all references to a symbol across the codebase", serde_json::json!({"type":"object","properties":{"symbol":{"type":"string"},"limit":{"type":"integer"}},"required":["symbol"]})),
        ];
        let filtered: Vec<ToolDef> = all_safe_tools
            .into_iter()
            .filter(|(name, _, _)| {
                params.allowed_tools.as_ref().map_or(true, |list| list.iter().any(|t| t == name))
            })
            .map(|(name, desc, schema)| ToolDef {
                tool_type: "function".to_string(),
                function: FnDef {
                    name: name.to_string(),
                    description: desc.to_string(),
                    parameters: schema,
                },
            })
            .collect();
        if filtered.is_empty() { None } else { Some(filtered) }
    } else {
        None
    };

    let body = Req {
        model: params.model.clone(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: system_prompt,
            },
            Message {
                role: "user".to_string(),
                content: user_prompt,
            },
        ],
        stream: false,
        max_tokens,
        session_id: session_id.map(|s| s.to_string()),
        project_id: project_id.map(|s| s.to_string()),
        tools,
    };

    let resp = match client.post(&url).json(&body).timeout(timeout).send().await {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!(
                "Delegate timed out after {}s ({}tok / {:.1}tok/s + {}s overhead)",
                dynamic_timeout_secs, max_tokens, config.generate_tok_per_sec, GENERATE_OVERHEAD_SECS
            ));
        }
        Err(e) => {
            return tool_error(format!(
                "Odin unreachable at {}. Error: {}",
                config.odin_url, e
            ));
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Delegate failed (HTTP {}): {}", status, body));
    }

    let api: ChatApiResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse Odin response: {}", e)),
    };

    let text = api
        .choices
        .unwrap_or_default()
        .into_iter()
        .next()
        .and_then(|c| c.message)
        .and_then(|m| m.content)
        .unwrap_or_else(|| "(empty response)".to_string());

    // Phase 4: Format output
    let model_used = params.model.as_deref().unwrap_or("(default routing)");

    if params.structured_output.unwrap_or(false) {
        let files = parse_file_blocks(&text);
        let file_entries: Vec<serde_json::Value> = files
            .iter()
            .map(|(path, content)| {
                serde_json::json!({
                    "path": path,
                    "content": content,
                })
            })
            .collect();

        let result = serde_json::json!({
            "files": file_entries,
            "summary": format!("Generated {} file(s)", file_entries.len()),
            "model_used": model_used,
            "agent_type": agent_type,
        });

        tool_ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    } else {
        tool_ok(text)
    }
}

// ---------------------------------------------------------------------------
// Tool: diff_review
// ---------------------------------------------------------------------------

/// Review code diff or file content using local LLM with project context.
#[instrument(skip(client, config))]
pub async fn diff_review(
    client: &Client,
    config: &McpServerConfig,
    params: DiffReviewParams,
    session_id: Option<&str>,
    project_id: Option<&str>,
) -> CallToolResult {
    if params.content.len() > MAX_PROMPT_BYTES {
        return tool_error(format!(
            "content exceeds maximum size of {MAX_PROMPT_BYTES} bytes"
        ));
    }

    let focus = params.focus.as_deref().unwrap_or("all");
    let description = params
        .description
        .as_deref()
        .unwrap_or("Code change review");

    // Fetch memory context about the changed code
    let memory_context = {
        let p = QueryMemoryParams {
            text: description.to_string(),
            limit: Some(5),
        };
        let result = query_memory(client, config, p).await;
        result
            .content
            .into_iter()
            .next()
            .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
            .unwrap_or_default()
    };

    let prompt = format!(
        "You are a senior engineer reviewing a code change.\n\n\
        ## Change Description\n{}\n\n\
        ## Diff / Code\n```\n{}\n```\n\n\
        ## Architectural Decisions & Prior Art\n{}\n\n\
        ## Review Focus: {}\n\n\
        Provide a structured review:\n\
        1. **Issues** (bugs, security, correctness) — with severity HIGH/MEDIUM/LOW\n\
        2. **Architecture alignment** — does this follow established patterns?\n\
        3. **Performance** — any obvious bottlenecks or regressions?\n\
        4. **Suggestions** — specific improvements with code snippets\n\n\
        Be concise. Only flag real issues, not style nits.",
        description, params.content, memory_context, focus
    );

    let gen_params = GenerateParams {
        prompt,
        model: None,
        max_tokens: Some(4096),
    };

    let result = generate(client, config, gen_params, session_id, project_id).await;

    let review = result
        .content
        .into_iter()
        .next()
        .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
        .unwrap_or_else(|| "(empty review)".to_string());

    // Store the review as an engram for future reference (fire-and-forget)
    let store_client = client.clone();
    let store_config = config.clone();
    let review_clone = review.clone();
    let desc_clone = description.to_string();
    let focus_owned = focus.to_string();
    tokio::spawn(async move {
        let p = StoreMemoryParams {
            id: None,
            cause: format!("diff_review: {}", desc_clone),
            effect: review_clone,
            tags: Some(vec!["review".to_string(), focus_owned]),
            force: None,
        };
        let _ = store_memory(&store_client, &store_config, p).await;
    });

    tool_ok(format!("## Diff Review (focus: {})\n\n{}", focus, review))
}

// ---------------------------------------------------------------------------
// Tool: context_bridge
// ---------------------------------------------------------------------------

/// Export or import cross-IDE context snapshots.
#[instrument(skip(client, config))]
pub async fn context_bridge(
    client: &Client,
    config: &McpServerConfig,
    params: ContextBridgeParams,
) -> CallToolResult {
    match params.action.as_str() {
        "export" => {
            // Gather current context: recent engrams + sprint info
            let query_text = "active sprint current work context".to_string();
            let p = QueryMemoryParams {
                text: query_text,
                limit: Some(10),
            };
            let memory_result = query_memory(client, config, p).await;
            let context = memory_result
                .content
                .into_iter()
                .next()
                .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
                .unwrap_or_default();

            let label = params
                .context_id
                .as_deref()
                .unwrap_or("cross-ide-context");

            // Store as a context_bridge engram
            let store_params = StoreMemoryParams {
                id: None,
                cause: format!("context_bridge:export:{}", label),
                effect: context.clone(),
                tags: Some(vec![
                    "context_bridge".to_string(),
                    label.to_string(),
                ]),
                force: None,
            };
            let store_result = store_memory(client, config, store_params).await;
            let id = store_result
                .content
                .into_iter()
                .next()
                .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
                .unwrap_or_else(|| "unknown".to_string());

            tool_ok(format!(
                "## Context Exported\n\n**Label:** {}\n**Engram:** {}\n**Size:** {} chars\n\n\
                To import in another IDE:\n```\ncontext_bridge_tool(action: \"import\", context_id: \"{}\")\n```",
                label, id, context.len(), label
            ))
        }
        "import" => {
            let label = match &params.context_id {
                Some(id) if !id.is_empty() => id,
                _ => return tool_error("'context_id' (label) is required for import"),
            };

            // Search for the context_bridge engram by label
            let p = QueryMemoryParams {
                text: format!("context_bridge:export:{}", label),
                limit: Some(1),
            };
            let result = query_memory(client, config, p).await;
            let context = result
                .content
                .into_iter()
                .next()
                .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
                .unwrap_or_default();

            if context.is_empty() || context.contains("No engrams found") {
                return tool_error(format!(
                    "No context snapshot found for label '{}'. Export first from the source IDE.",
                    label
                ));
            }

            tool_ok(format!(
                "## Context Imported\n\n**Label:** {}\n\n### Restored Context\n\n{}",
                label, context
            ))
        }
        other => tool_error(format!(
            "Unknown action '{}'. Use 'export' or 'import'.",
            other
        )),
    }
}

// ---------------------------------------------------------------------------
// Tool: ast_analyze (symbol lookup)
// ---------------------------------------------------------------------------

/// POST to Muninn /api/v1/symbols and format results as markdown.
#[instrument(skip(client, config))]
pub async fn ast_analyze(
    client: &Client,
    config: &McpServerConfig,
    params: AstAnalyzeParams,
) -> CallToolResult {
    if params.name.is_none()
        && params.chunk_type.is_none()
        && params.language.is_none()
        && params.file_path.is_none()
    {
        return tool_error(
            "At least one filter is required: name, chunk_type, language, or file_path.",
        );
    }

    let muninn_url = match &config.muninn_url {
        Some(u) => u.clone(),
        None => return tool_error("AST analysis unavailable. No Muninn URL configured."),
    };

    let timeout = Duration::from_secs(config.timeout_secs);
    let url = format!("{}/api/v1/symbols", muninn_url.trim_end_matches('/'));

    #[derive(Serialize)]
    struct Req<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        chunk_type: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_path: Option<&'a str>,
        limit: u32,
    }

    let body = Req {
        name: params.name.as_deref(),
        chunk_type: params.chunk_type.as_deref(),
        language: params.language.as_deref(),
        file_path: params.file_path.as_deref(),
        limit: params.limit.unwrap_or(20).min(100),
    };

    let resp = match client.post(&url).json(&body).timeout(timeout).send().await {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!("Request timed out after {}s", config.timeout_secs));
        }
        Err(e) => {
            return tool_error(format!("Muninn unreachable at {}: {}", muninn_url, e));
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Symbol lookup failed (HTTP {}): {}", status, body));
    }

    #[derive(Deserialize)]
    struct Symbol {
        file_path: String,
        name: String,
        chunk_type: String,
        parent_context: Option<String>,
        language: String,
        start_line: i32,
        end_line: i32,
    }

    #[derive(Deserialize)]
    struct Resp {
        symbols: Vec<Symbol>,
    }

    let api: Resp = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse response: {}", e)),
    };

    if api.symbols.is_empty() {
        let mut desc = String::from("No symbols found matching:");
        if let Some(ref n) = params.name {
            desc.push_str(&format!(" name={}", n));
        }
        if let Some(ref ct) = params.chunk_type {
            desc.push_str(&format!(" type={}", ct));
        }
        if let Some(ref l) = params.language {
            desc.push_str(&format!(" lang={}", l));
        }
        if let Some(ref fp) = params.file_path {
            desc.push_str(&format!(" file={}", fp));
        }
        return tool_ok(format!("## Symbol Lookup\n\n{}", desc));
    }

    let mut out = format!("## Symbol Lookup ({} results)\n\n", api.symbols.len());
    out.push_str("| Name | Type | File | Lines | Parent | Language |\n");
    out.push_str("|------|------|------|-------|--------|----------|\n");
    for s in &api.symbols {
        let parent = s.parent_context.as_deref().unwrap_or("-");
        out.push_str(&format!(
            "| `{}` | {} | `{}` | {}-{} | {} | {} |\n",
            s.name, s.chunk_type, s.file_path, s.start_line, s.end_line, parent, s.language
        ));
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: impact_analysis (find references)
// ---------------------------------------------------------------------------

/// POST to Muninn /api/v1/references and format results as markdown.
#[instrument(skip(client, config), fields(symbol = %params.symbol))]
pub async fn impact_analysis(
    client: &Client,
    config: &McpServerConfig,
    params: ImpactAnalysisParams,
) -> CallToolResult {
    if params.symbol.trim().is_empty() {
        return tool_error("Symbol name must not be empty.");
    }

    let muninn_url = match &config.muninn_url {
        Some(u) => u.clone(),
        None => return tool_error("Impact analysis unavailable. No Muninn URL configured."),
    };

    let timeout = Duration::from_secs(config.timeout_secs);
    let url = format!("{}/api/v1/references", muninn_url.trim_end_matches('/'));

    #[derive(Serialize)]
    struct Req<'a> {
        symbol: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        exclude_id: Option<&'a str>,
        limit: u32,
    }

    let body = Req {
        symbol: &params.symbol,
        language: params.language.as_deref(),
        exclude_id: params.exclude_id.as_deref(),
        limit: params.limit.unwrap_or(20).min(50),
    };

    let resp = match client.post(&url).json(&body).timeout(timeout).send().await {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!("Request timed out after {}s", config.timeout_secs));
        }
        Err(e) => {
            return tool_error(format!("Muninn unreachable at {}: {}", muninn_url, e));
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return tool_error(format!("Find references failed (HTTP {}): {}", status, body));
    }

    #[derive(Deserialize)]
    struct Reference {
        file_path: String,
        name: String,
        chunk_type: String,
        parent_context: Option<String>,
        start_line: i32,
        end_line: i32,
        relevance: f64,
    }

    #[derive(Deserialize)]
    struct Resp {
        references: Vec<Reference>,
    }

    let api: Resp = match resp.json().await {
        Ok(v) => v,
        Err(e) => return tool_error(format!("Failed to parse response: {}", e)),
    };

    if api.references.is_empty() {
        return tool_ok(format!(
            "## Impact Analysis: `{}`\n\nNo references found in the indexed codebase.",
            params.symbol
        ));
    }

    let mut out = format!(
        "## Impact Analysis: `{}` ({} references)\n\n",
        params.symbol,
        api.references.len()
    );
    out.push_str("| File | Chunk | Type | Lines | Parent | Relevance |\n");
    out.push_str("|------|-------|------|-------|--------|-----------|\n");
    for r in &api.references {
        let parent = r.parent_context.as_deref().unwrap_or("-");
        out.push_str(&format!(
            "| `{}` | `{}` | {} | {}-{} | {} | {:.3} |\n",
            r.file_path, r.name, r.chunk_type, r.start_line, r.end_line, parent, r.relevance
        ));
    }

    out.push_str(&format!(
        "\n**Tip:** Changing `{}` may affect all {} locations above. \
        Review each reference before refactoring.",
        params.symbol,
        api.references.len()
    ));

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: task_queue (persistent task coordination)
// ---------------------------------------------------------------------------

/// POST to Mimir task endpoints and format results as markdown.
#[instrument(skip(client, config), fields(action = %params.action))]
pub async fn task_queue(
    client: &Client,
    config: &McpServerConfig,
    params: TaskQueueParams,
) -> CallToolResult {
    let odin_url = config.odin_url.trim_end_matches('/');
    let timeout = Duration::from_secs(config.timeout_secs);

    match params.action.as_str() {
        "push" => {
            let title = match &params.title {
                Some(t) if !t.trim().is_empty() => t,
                _ => return tool_error("'title' is required for push action."),
            };

            #[derive(Serialize)]
            struct Req<'a> {
                title: &'a str,
                #[serde(skip_serializing_if = "Option::is_none")]
                description: Option<&'a str>,
                priority: i32,
                #[serde(skip_serializing_if = "Option::is_none")]
                project: Option<&'a str>,
                tags: Vec<String>,
            }

            let body = Req {
                title,
                description: params.description.as_deref(),
                priority: params.priority.unwrap_or(0),
                project: params.project.as_deref(),
                tags: params.tags.unwrap_or_default(),
            };

            let resp = match client
                .post(format!("{odin_url}/api/v1/tasks/push"))
                .json(&body)
                .timeout(timeout)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Task push failed: {e}")),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!("Task push failed (HTTP {}): {}", status, body));
            }

            #[derive(Deserialize)]
            struct Resp {
                id: String,
            }

            let api: Resp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Parse error: {e}")),
            };

            tool_ok(format!(
                "## Task Created\n\n**ID:** `{}`\n**Title:** {}\n**Priority:** {}",
                api.id,
                title,
                params.priority.unwrap_or(0)
            ))
        }

        "pop" => {
            let agent = match &params.agent {
                Some(a) if !a.trim().is_empty() => a,
                _ => return tool_error("'agent' is required for pop action."),
            };

            #[derive(Serialize)]
            struct Req<'a> {
                agent: &'a str,
                #[serde(skip_serializing_if = "Option::is_none")]
                project: Option<&'a str>,
            }

            let body = Req {
                agent,
                project: params.project.as_deref(),
            };

            let resp = match client
                .post(format!("{odin_url}/api/v1/tasks/pop"))
                .json(&body)
                .timeout(timeout)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Task pop failed: {e}")),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!("Task pop failed (HTTP {}): {}", status, body));
            }

            #[derive(Deserialize)]
            struct TaskResp {
                id: String,
                title: String,
                description: String,
                priority: i32,
                tags: Vec<String>,
            }

            #[derive(Deserialize)]
            struct Resp {
                task: Option<TaskResp>,
            }

            let api: Resp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Parse error: {e}")),
            };

            match api.task {
                Some(t) => {
                    let tags = if t.tags.is_empty() {
                        "-".to_string()
                    } else {
                        t.tags.join(", ")
                    };
                    tool_ok(format!(
                        "## Task Claimed\n\n**ID:** `{}`\n**Title:** {}\n**Priority:** {}\n**Tags:** {}\n\n### Description\n\n{}",
                        t.id, t.title, t.priority, tags, t.description
                    ))
                }
                None => tool_ok("## No Pending Tasks\n\nThe task queue is empty."),
            }
        }

        "complete" => {
            let task_id = match &params.task_id {
                Some(id) if !id.trim().is_empty() => id,
                _ => return tool_error("'task_id' is required for complete action."),
            };

            #[derive(Serialize)]
            struct Req<'a> {
                task_id: &'a str,
                success: bool,
                #[serde(skip_serializing_if = "Option::is_none")]
                result: Option<&'a str>,
            }

            let body = Req {
                task_id,
                success: params.success.unwrap_or(true),
                result: params.result.as_deref(),
            };

            let resp = match client
                .post(format!("{odin_url}/api/v1/tasks/complete"))
                .json(&body)
                .timeout(timeout)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Task complete failed: {e}")),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!("Task complete failed (HTTP {}): {}", status, body));
            }

            #[derive(Deserialize)]
            struct Resp {
                updated: bool,
            }

            let api: Resp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Parse error: {e}")),
            };

            let outcome = if params.success.unwrap_or(true) {
                "completed"
            } else {
                "failed"
            };

            tool_ok(format!(
                "## Task {}\n\n**ID:** `{}`\n**Updated:** {}",
                outcome.to_uppercase(),
                task_id,
                api.updated
            ))
        }

        "cancel" => {
            let task_id = match &params.task_id {
                Some(id) if !id.trim().is_empty() => id,
                _ => return tool_error("'task_id' is required for cancel action."),
            };

            #[derive(Serialize)]
            struct Req<'a> {
                task_id: &'a str,
            }

            let resp = match client
                .post(format!("{odin_url}/api/v1/tasks/cancel"))
                .json(&Req { task_id })
                .timeout(timeout)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Task cancel failed: {e}")),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!("Task cancel failed (HTTP {}): {}", status, body));
            }

            #[derive(Deserialize)]
            struct Resp {
                cancelled: bool,
            }

            let api: Resp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Parse error: {e}")),
            };

            tool_ok(format!(
                "## Task Cancelled\n\n**ID:** `{}`\n**Cancelled:** {}",
                task_id, api.cancelled
            ))
        }

        "list" => {
            #[derive(Serialize)]
            struct Req<'a> {
                #[serde(skip_serializing_if = "Option::is_none")]
                status: Option<&'a str>,
                #[serde(skip_serializing_if = "Option::is_none")]
                project: Option<&'a str>,
                #[serde(skip_serializing_if = "Option::is_none")]
                agent: Option<&'a str>,
                limit: u32,
            }

            let body = Req {
                status: params.status.as_deref(),
                project: params.project.as_deref(),
                agent: params.agent.as_deref(),
                limit: params.limit.unwrap_or(20).min(100),
            };

            let resp = match client
                .post(format!("{odin_url}/api/v1/tasks/list"))
                .json(&body)
                .timeout(timeout)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Task list failed: {e}")),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!("Task list failed (HTTP {}): {}", status, body));
            }

            #[derive(Deserialize)]
            struct TaskItem {
                id: String,
                title: String,
                status: String,
                priority: i32,
                agent: Option<String>,
                project: Option<String>,
            }

            #[derive(Deserialize)]
            struct Resp {
                tasks: Vec<TaskItem>,
            }

            let api: Resp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Parse error: {e}")),
            };

            if api.tasks.is_empty() {
                return tool_ok("## Task Queue\n\nNo tasks found matching filters.");
            }

            let mut out = format!("## Task Queue ({} tasks)\n\n", api.tasks.len());
            out.push_str("| ID | Title | Status | Priority | Agent | Project |\n");
            out.push_str("|----|-------|--------|----------|-------|----------|\n");
            for t in &api.tasks {
                out.push_str(&format!(
                    "| `{}` | {} | {} | {} | {} | {} |\n",
                    &t.id[..8],
                    t.title,
                    t.status,
                    t.priority,
                    t.agent.as_deref().unwrap_or("-"),
                    t.project.as_deref().unwrap_or("-"),
                ));
            }

            tool_ok(out)
        }

        other => tool_error(format!(
            "Unknown action '{}'. Use: push, pop, complete, cancel, or list.",
            other
        )),
    }
}

// ---------------------------------------------------------------------------
// Tool: memory_graph (engram relationship graph)
// ---------------------------------------------------------------------------

/// POST to Mimir graph endpoints and format results as markdown.
#[instrument(skip(client, config), fields(action = %params.action))]
pub async fn memory_graph(
    client: &Client,
    config: &McpServerConfig,
    params: MemoryGraphParams,
) -> CallToolResult {
    let odin_url = config.odin_url.trim_end_matches('/');
    let timeout = Duration::from_secs(config.timeout_secs);

    match params.action.as_str() {
        "link" => {
            let source = match &params.source_id {
                Some(id) if !id.is_empty() => id,
                _ => return tool_error("'source_id' is required for link."),
            };
            let target = match &params.target_id {
                Some(id) if !id.is_empty() => id,
                _ => return tool_error("'target_id' is required for link."),
            };
            let relation = match &params.relation {
                Some(r) if !r.is_empty() => r,
                _ => return tool_error("'relation' is required for link."),
            };

            #[derive(Serialize)]
            struct Req<'a> {
                source_id: &'a str,
                target_id: &'a str,
                relation: &'a str,
                weight: f32,
            }

            let body = Req {
                source_id: source,
                target_id: target,
                relation,
                weight: params.weight.unwrap_or(1.0),
            };

            let resp = match client
                .post(format!("{odin_url}/api/v1/graph/link"))
                .json(&body)
                .timeout(timeout)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Graph link failed: {e}")),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!("Graph link failed (HTTP {}): {}", status, body));
            }

            #[derive(Deserialize)]
            struct Resp {
                id: String,
            }

            let api: Resp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Parse error: {e}")),
            };

            tool_ok(format!(
                "## Edge Created\n\n`{}` --[{}]--> `{}`\n**Edge ID:** `{}`\n**Weight:** {}",
                &source[..8.min(source.len())],
                relation,
                &target[..8.min(target.len())],
                api.id,
                params.weight.unwrap_or(1.0)
            ))
        }

        "unlink" => {
            let source = match &params.source_id {
                Some(id) if !id.is_empty() => id,
                _ => return tool_error("'source_id' is required for unlink."),
            };
            let target = match &params.target_id {
                Some(id) if !id.is_empty() => id,
                _ => return tool_error("'target_id' is required for unlink."),
            };
            let relation = match &params.relation {
                Some(r) if !r.is_empty() => r,
                _ => return tool_error("'relation' is required for unlink."),
            };

            #[derive(Serialize)]
            struct Req<'a> {
                source_id: &'a str,
                target_id: &'a str,
                relation: &'a str,
            }

            let resp = match client
                .post(format!("{odin_url}/api/v1/graph/unlink"))
                .json(&Req {
                    source_id: source,
                    target_id: target,
                    relation,
                })
                .timeout(timeout)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Graph unlink failed: {e}")),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!("Graph unlink failed (HTTP {}): {}", status, body));
            }

            #[derive(Deserialize)]
            struct Resp {
                removed: bool,
            }

            let api: Resp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Parse error: {e}")),
            };

            tool_ok(format!("## Edge Removed\n\n**Removed:** {}", api.removed))
        }

        "neighbors" => {
            let engram_id = match &params.engram_id {
                Some(id) if !id.is_empty() => id,
                _ => return tool_error("'engram_id' is required for neighbors."),
            };

            #[derive(Serialize)]
            struct Req<'a> {
                engram_id: &'a str,
                direction: &'a str,
                #[serde(skip_serializing_if = "Option::is_none")]
                relation: Option<&'a str>,
                limit: u32,
            }

            let body = Req {
                engram_id,
                direction: params.direction.as_deref().unwrap_or("both"),
                relation: params.relation.as_deref(),
                limit: params.limit.unwrap_or(20).min(100),
            };

            let resp = match client
                .post(format!("{odin_url}/api/v1/graph/neighbors"))
                .json(&body)
                .timeout(timeout)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Graph neighbors failed: {e}")),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!(
                    "Graph neighbors failed (HTTP {}): {}",
                    status, body
                ));
            }

            #[derive(Deserialize)]
            struct EdgeItem {
                source_id: String,
                target_id: String,
                relation: String,
                weight: f32,
            }

            #[derive(Deserialize)]
            struct Resp {
                edges: Vec<EdgeItem>,
            }

            let api: Resp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Parse error: {e}")),
            };

            if api.edges.is_empty() {
                return tool_ok(format!(
                    "## Neighbors of `{}`\n\nNo edges found.",
                    &engram_id[..8.min(engram_id.len())]
                ));
            }

            let mut out = format!(
                "## Neighbors of `{}` ({} edges)\n\n",
                &engram_id[..8.min(engram_id.len())],
                api.edges.len()
            );
            out.push_str("| Source | Relation | Target | Weight |\n");
            out.push_str("|--------|----------|--------|--------|\n");
            for e in &api.edges {
                out.push_str(&format!(
                    "| `{}` | {} | `{}` | {:.2} |\n",
                    &e.source_id[..8],
                    e.relation,
                    &e.target_id[..8],
                    e.weight
                ));
            }

            tool_ok(out)
        }

        "traverse" => {
            let start_id = match &params.start_id {
                Some(id) if !id.is_empty() => id,
                _ => return tool_error("'start_id' is required for traverse."),
            };

            #[derive(Serialize)]
            struct Req<'a> {
                start_id: &'a str,
                max_depth: u32,
                #[serde(skip_serializing_if = "Option::is_none")]
                relation: Option<&'a str>,
                limit: u32,
            }

            let body = Req {
                start_id,
                max_depth: params.max_depth.unwrap_or(2).min(5),
                relation: params.relation.as_deref(),
                limit: params.limit.unwrap_or(50).min(200),
            };

            let resp = match client
                .post(format!("{odin_url}/api/v1/graph/traverse"))
                .json(&body)
                .timeout(timeout)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return tool_error(format!("Graph traverse failed: {e}")),
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return tool_error(format!(
                    "Graph traverse failed (HTTP {}): {}",
                    status, body
                ));
            }

            #[derive(Deserialize)]
            struct EdgeItem {
                source_id: String,
                target_id: String,
                relation: String,
                weight: f32,
            }

            #[derive(Deserialize)]
            struct Resp {
                edges: Vec<EdgeItem>,
            }

            let api: Resp = match resp.json().await {
                Ok(v) => v,
                Err(e) => return tool_error(format!("Parse error: {e}")),
            };

            if api.edges.is_empty() {
                return tool_ok(format!(
                    "## Graph Traversal from `{}`\n\nNo connected engrams found.",
                    &start_id[..8.min(start_id.len())]
                ));
            }

            let mut out = format!(
                "## Graph Traversal from `{}` ({} edges, max {} hops)\n\n",
                &start_id[..8.min(start_id.len())],
                api.edges.len(),
                params.max_depth.unwrap_or(2)
            );
            out.push_str("| Source | Relation | Target | Weight |\n");
            out.push_str("|--------|----------|--------|--------|\n");
            for e in &api.edges {
                out.push_str(&format!(
                    "| `{}` | {} | `{}` | {:.2} |\n",
                    &e.source_id[..8],
                    e.relation,
                    &e.target_id[..8],
                    e.weight
                ));
            }

            tool_ok(out)
        }

        other => tool_error(format!(
            "Unknown action '{}'. Use: link, unlink, neighbors, or traverse.",
            other
        )),
    }
}

// ---------------------------------------------------------------------------
// HA tool parameter structs
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

/// Parameters for the `ha_get_states` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HaGetStatesParams {
    /// If true (default), return a compact markdown summary grouped by domain.
    /// If false, return the raw JSON array (truncated to first 50 entities).
    #[serde(default = "default_true")]
    pub summary: bool,
}

/// Parameters for the `ha_list_entities` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HaListEntitiesParams {
    /// HA domain to filter by (e.g., "light", "switch", "sensor", "climate").
    /// If omitted, returns all entities across all domains.
    pub domain: Option<String>,
}

/// Parameters for the `ha_call_service` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HaCallServiceParams {
    /// HA service domain (e.g., "light", "switch", "cover", "climate").
    pub domain: String,
    /// Service name (e.g., "turn_on", "turn_off", "toggle", "set_temperature").
    pub service: String,
    /// Service call data (e.g., {"entity_id": "light.living_room", "brightness": 128}).
    pub data: serde_json::Value,
}

/// Parameters for the `ha_generate_automation` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HaGenerateAutomationParams {
    /// Natural language description of the desired automation.
    pub description: String,
}

// ---------------------------------------------------------------------------
// Tool: ha_get_states
// ---------------------------------------------------------------------------

/// Fetch all HA entity states and format as a markdown summary or raw JSON.
#[instrument(skip(ha_client), fields(summary = params.summary))]
pub async fn ha_get_states(
    ha_client: Option<&HaClient>,
    params: HaGetStatesParams,
) -> CallToolResult {
    let client = match ha_client {
        Some(c) => c,
        None => return tool_error("Home Assistant is not configured."),
    };

    let states = match client.get_states().await {
        Ok(s) => s,
        Err(e) => return tool_error(format!("Failed to get HA states: {}", e)),
    };

    if params.summary {
        let out = format_states_summary(&states);
        tool_ok(out)
    } else {
        // Raw JSON, truncated to 50 entities.
        let truncated: Vec<_> = states.into_iter().take(50).collect();
        match serde_json::to_string_pretty(&truncated) {
            Ok(json) => tool_ok(json),
            Err(e) => tool_error(format!("Failed to serialize HA states: {}", e)),
        }
    }
}

/// Format entity states as a markdown summary grouped by domain.
///
/// Performance: O(n) grouping by prefix scan. Under 50ms for 500 entities.
fn format_states_summary(states: &[ygg_ha::EntityState]) -> String {
    // Group by domain (prefix before first '.')
    let mut by_domain: BTreeMap<&str, Vec<&ygg_ha::EntityState>> = BTreeMap::new();
    for state in states {
        let domain = state
            .entity_id
            .split_once('.')
            .map(|(d, _)| d)
            .unwrap_or("unknown");
        by_domain.entry(domain).or_default().push(state);
    }

    let total: usize = states.len();
    let mut out = format!(
        "## Home Assistant Entity States ({} entities)\n\n",
        total
    );

    for (domain, entities) in &by_domain {
        out.push_str(&format!("### {} ({})\n", domain, entities.len()));

        // Domain-specific columns
        match *domain {
            "light" => {
                out.push_str("| Entity | State | Brightness |\n|--------|-------|------------|\n");
                for e in entities.iter() {
                    let friendly = e
                        .attributes
                        .get("friendly_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let brightness = e
                        .attributes
                        .get("brightness")
                        .and_then(|v| v.as_f64())
                        .map(|b| format!("{:.0}", b))
                        .unwrap_or_else(|| "-".to_string());
                    let label = if friendly.is_empty() {
                        e.entity_id.clone()
                    } else {
                        format!("{} ({})", e.entity_id, friendly)
                    };
                    out.push_str(&format!("| {} | {} | {} |\n", label, e.state, brightness));
                }
            }
            "sensor" | "binary_sensor" => {
                out.push_str("| Entity | State | Unit |\n|--------|-------|------|\n");
                for e in entities.iter() {
                    let friendly = e
                        .attributes
                        .get("friendly_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let unit = e
                        .attributes
                        .get("unit_of_measurement")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-");
                    let label = if friendly.is_empty() {
                        e.entity_id.clone()
                    } else {
                        format!("{} ({})", e.entity_id, friendly)
                    };
                    out.push_str(&format!("| {} | {} | {} |\n", label, e.state, unit));
                }
            }
            _ => {
                out.push_str("| Entity | State |\n|--------|-------|\n");
                for e in entities.iter() {
                    let friendly = e
                        .attributes
                        .get("friendly_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let label = if friendly.is_empty() {
                        e.entity_id.clone()
                    } else {
                        format!("{} ({})", e.entity_id, friendly)
                    };
                    out.push_str(&format!("| {} | {} |\n", label, e.state));
                }
            }
        }
        out.push('\n');
    }

    out
}

// ---------------------------------------------------------------------------
// Tool: ha_list_entities
// ---------------------------------------------------------------------------

/// List HA entities filtered by domain, formatted as a markdown table.
#[instrument(skip(ha_client), fields(domain = ?params.domain))]
pub async fn ha_list_entities(
    ha_client: Option<&HaClient>,
    params: HaListEntitiesParams,
) -> CallToolResult {
    let client = match ha_client {
        Some(c) => c,
        None => return tool_error("Home Assistant is not configured."),
    };

    let entities = match client.list_entities(params.domain.as_deref()).await {
        Ok(e) => e,
        Err(e) => return tool_error(format!("Failed to list HA entities: {}", e)),
    };

    let domain_label = params.domain.as_deref().unwrap_or("all");
    let mut out = format!(
        "## Entities: {} ({} found)\n\n",
        domain_label,
        entities.len()
    );

    if entities.is_empty() {
        out.push_str("No entities found.");
        return tool_ok(out);
    }

    out.push_str("| Entity ID | Friendly Name | State | Last Changed |\n");
    out.push_str("|-----------|---------------|-------|---------------|\n");

    for entity in &entities {
        let friendly = entity
            .attributes
            .get("friendly_name")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        // Format last_changed: trim to date+time without sub-seconds.
        let last_changed = entity
            .last_changed
            .as_deref()
            .map(|ts| ts.get(..16).unwrap_or(ts))
            .unwrap_or("-");
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            entity.entity_id, friendly, entity.state, last_changed
        ));
    }

    tool_ok(out)
}

// ---------------------------------------------------------------------------
// Tool: ha_call_service
// ---------------------------------------------------------------------------

/// Call an HA service and return a success or failure message.
#[instrument(skip(ha_client), fields(domain = %params.domain, service = %params.service))]
pub async fn ha_call_service(
    ha_client: Option<&HaClient>,
    params: HaCallServiceParams,
) -> CallToolResult {
    let client = match ha_client {
        Some(c) => c,
        None => return tool_error("Home Assistant is not configured."),
    };

    // Domain allowlist: restrict callable services to safe domains.
    const ALLOWED_DOMAINS: &[&str] = &[
        "light", "switch", "cover", "fan", "media_player", "scene",
        "script", "input_boolean", "input_number", "input_select",
        "input_text", "automation", "climate", "vacuum", "button",
        "number", "select", "humidifier", "water_heater",
    ];
    if !ALLOWED_DOMAINS.contains(&params.domain.as_str()) {
        return tool_error(format!(
            "Domain '{}' is not in the allowed list. Allowed: {}",
            params.domain,
            ALLOWED_DOMAINS.join(", ")
        ));
    }

    match client
        .call_service(&params.domain, &params.service, params.data.clone())
        .await
    {
        Ok(()) => {
            let data_pretty = serde_json::to_string_pretty(&params.data)
                .unwrap_or_else(|_| params.data.to_string());
            tool_ok(format!(
                "Service called successfully: {}.{}\n\nData sent:\n{}",
                params.domain, params.service, data_pretty
            ))
        }
        Err(e) => tool_error(format!(
            "Service call failed: {}.{}\nError: {}",
            params.domain, params.service, e
        )),
    }
}

// ---------------------------------------------------------------------------
// Tool: ha_generate_automation
// ---------------------------------------------------------------------------

/// Generate Home Assistant automation YAML from a natural-language description.
///
/// Fetches entity states and services from HA, builds a structured prompt,
/// and delegates generation to Odin's reasoning model via `AutomationGenerator`.
#[instrument(skip(ha_client, generator), fields(description = %params.description))]
pub async fn ha_generate_automation(
    ha_client: Option<&HaClient>,
    generator: Option<&AutomationGenerator>,
    params: HaGenerateAutomationParams,
) -> CallToolResult {
    let client = match ha_client {
        Some(c) => c,
        None => return tool_error("Home Assistant is not configured."),
    };
    let automation_gen = match generator {
        Some(g) => g,
        None => return tool_error("Home Assistant is not configured."),
    };

    match automation_gen.generate_automation(client, &params.description).await {
        Ok(yaml) => tool_ok(format!(
            "## Generated Automation\n\n```yaml\n{}\n```\n\n\
             **Note:** Review this automation carefully before adding it to \
             your Home Assistant configuration.",
            yaml
        )),
        Err(e) => tool_error(format!("Automation generation failed: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// Tool: screenshot
// ---------------------------------------------------------------------------

/// Capture a screenshot of a web page via a headless Chromium browser instance.
///
/// The caller is responsible for launching the browser and keeping it alive. This
/// function opens a new page/tab on the provided browser, sets the viewport via CDP
/// `SetDeviceMetricsOverride`, navigates to the URL, optionally waits for a CSS
/// selector, captures the PNG screenshot, and writes it to `/tmp/ygg-screenshots/`.
///
/// # OPTIMIZATION: Uses CDP SetDeviceMetricsOverride for per-capture viewport control
/// rather than relying on the browser-wide viewport set at launch. This allows each
/// screenshot call to use a different resolution without restarting the browser.
/// Fallback: if the CDP command fails, navigation still proceeds at the browser's
/// default viewport size.
pub async fn screenshot(
    browser: &chromiumoxide::Browser,
    params: ScreenshotParams,
) -> CallToolResult {
    use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
    use chromiumoxide::page::ScreenshotParams as CdpScreenshotParams;

    let width = params.viewport_width.unwrap_or(1280);
    let height = params.viewport_height.unwrap_or(720);
    let full_page = params.full_page.unwrap_or(false);

    // Create output directory
    let output_dir = std::path::Path::new("/tmp/ygg-screenshots");
    if let Err(e) = tokio::fs::create_dir_all(output_dir).await {
        return tool_error(format!("Failed to create output dir: {e}"));
    }

    // Open a new page/tab
    let page = match browser.new_page("about:blank").await {
        Ok(p) => p,
        Err(e) => return tool_error(format!("Failed to open new tab: {e}")),
    };

    // Set viewport via CDP emulation command so each capture can use its own dimensions.
    // OPTIMIZATION: SetDeviceMetricsOverride (CDP emulation) is per-target, so we set it
    // after opening the tab rather than at browser launch. The browser default (800x600)
    // is used as fallback if this command fails.
    let viewport_cmd =
        SetDeviceMetricsOverrideParams::new(width as i64, height as i64, 1.0_f64, false);
    if let Err(e) = page.execute(viewport_cmd).await {
        tracing::warn!("Failed to set viewport {width}x{height}: {e}");
    }

    // Navigate to the target URL
    if let Err(e) = page.goto(params.url.as_str()).await {
        return tool_error(format!("Failed to navigate to {}: {e}", params.url));
    }

    // Optionally wait for a CSS selector (useful for SPAs that render asynchronously)
    if let Some(ref selector) = params.selector {
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            page.find_element(selector.as_str()),
        )
        .await
        {
            Ok(Ok(_)) => {} // Element found
            Ok(Err(e)) => {
                return tool_error(format!("Selector '{selector}' not found: {e}"));
            }
            Err(_) => {
                return tool_error(format!(
                    "Timeout waiting for selector '{selector}' (10s)"
                ));
            }
        }
    }

    // Capture the screenshot as a PNG byte buffer
    let screenshot_params = CdpScreenshotParams::builder().full_page(full_page).build();
    let png_data = match page.screenshot(screenshot_params).await {
        Ok(data) => data,
        Err(e) => return tool_error(format!("Screenshot capture failed: {e}")),
    };

    // Generate a human-readable filename from the URL slug + Unix timestamp
    let slug: String = params
        .url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .take(50)
        .collect();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let filename = format!("{slug}-{timestamp}.png");
    let output_path = output_dir.join(&filename);

    // Write PNG bytes to disk
    if let Err(e) = tokio::fs::write(&output_path, &png_data).await {
        return tool_error(format!("Failed to write screenshot to disk: {e}"));
    }

    let path_str = output_path.display().to_string();
    tool_ok(format!(
        "Screenshot saved to: {path_str}\n\n\
         Use the Read tool to view this image.\n\
         Viewport: {width}x{height}, full_page: {full_page}",
    ))
}

// ---------------------------------------------------------------------------
// config_version — version checking and management
// ---------------------------------------------------------------------------

/// Derive the config API base URL from the Odin URL.
/// Odin runs on :8080, the config API runs on :9093 on the same host.
fn config_api_base(config: &McpServerConfig) -> String {
    config
        .odin_url
        .replace(":8080", ":9093")
        .trim_end_matches('/')
        .to_string()
}

fn file_type_to_local_path(file_type: &str, config: &McpServerConfig) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()));
    match file_type {
        "global_settings" => Some(home.join(".claude").join("settings.json")),
        "global_claude_md" => Some(home.join(".claude").join("CLAUDE.md")),
        "project_settings" => config.workspace_path.as_ref().map(|w| {
            PathBuf::from(w).join(".claude").join("settings.local.json")
        }),
        "project_claude_md" => config.workspace_path.as_ref().map(|w| {
            PathBuf::from(w).join("CLAUDE.md")
        }),
        _ => None,
    }
}

fn write_with_backup(path: &PathBuf, content: &str) -> Result<(), String> {
    if path.exists() {
        let bak = path.with_extension("bak");
        std::fs::copy(path, &bak)
            .map_err(|e| format!("failed to create backup: {e}"))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory: {e}"))?;
    }
    std::fs::write(path, content)
        .map_err(|e| format!("failed to write file: {e}"))
}

pub async fn config_version(
    client: &Client,
    config: &McpServerConfig,
    params: ConfigVersionParams,
) -> CallToolResult {
    let base = config_api_base(config);

    match params.action.as_str() {
        "info" | "check" => {
            let resp = client
                .get(format!("{}/api/v1/version", base))
                .timeout(Duration::from_secs(5))
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    let body = r.text().await.unwrap_or_default();
                    let v: serde_json::Value =
                        serde_json::from_str(&body).unwrap_or(serde_json::json!({}));

                    let mut out = String::from("## Version Info\n\n");
                    out.push_str("| Component | Version |\n|---|---|\n");
                    out.push_str(&format!(
                        "| Server | {} |\n",
                        v["server_version"].as_str().unwrap_or("?")
                    ));
                    out.push_str(&format!(
                        "| Client (latest) | {} |\n",
                        v["client_latest"].as_str().unwrap_or("?")
                    ));
                    out.push_str(&format!(
                        "| Config | {} |\n",
                        v["config_version"].as_str().unwrap_or("?")
                    ));

                    if params.action == "check" {
                        let client_v = env!("CARGO_PKG_VERSION");
                        let latest = v["client_latest"].as_str().unwrap_or("?");
                        if client_v != latest {
                            out.push_str(&format!(
                                "\n**WARNING:** This client is v{client_v}, latest is v{latest}."
                            ));
                        } else {
                            out.push_str(&format!("\nClient v{client_v} is up to date."));
                        }
                    }

                    tool_ok(out)
                }
                Ok(r) => tool_error(format!("version endpoint returned {}", r.status())),
                Err(e) => tool_error(format!("failed to reach version endpoint: {e}")),
            }
        }
        "bump" => {
            let bump_type = params.bump_type.as_deref().unwrap_or("patch");
            let component = params.component.as_deref().unwrap_or("config");

            let body = serde_json::json!({
                "component": component,
                "bump_type": bump_type,
            });

            match client
                .post(format!("{}/api/v1/version/bump", base))
                .json(&body)
                .timeout(Duration::from_secs(5))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let resp = r.text().await.unwrap_or_default();
                    let v: serde_json::Value =
                        serde_json::from_str(&resp).unwrap_or(serde_json::json!({}));
                    tool_ok(format!(
                        "Bumped {} version: {} -> {}",
                        v["component"].as_str().unwrap_or(component),
                        v["old_version"].as_str().unwrap_or("?"),
                        v["new_version"].as_str().unwrap_or("?"),
                    ))
                }
                Ok(r) => tool_error(format!("bump failed: HTTP {}", r.status())),
                Err(e) => tool_error(format!("bump failed: {e}")),
            }
        }
        _ => tool_error(format!(
            "Unknown action '{}'. Expected: check, info, bump",
            params.action
        )),
    }
}

// ---------------------------------------------------------------------------
// config_sync — interactive push/pull/status of config files
// ---------------------------------------------------------------------------

pub async fn config_sync(
    client: &Client,
    config: &McpServerConfig,
    params: ConfigSyncParams,
) -> CallToolResult {
    let base = config_api_base(config);

    match params.action.as_str() {
        "status" => {
            // Fetch version info
            let version_resp = client
                .get(format!("{}/api/v1/version", base))
                .timeout(Duration::from_secs(5))
                .send()
                .await;

            let config_version = match version_resp {
                Ok(r) if r.status().is_success() => {
                    let body = r.text().await.unwrap_or_default();
                    let v: serde_json::Value =
                        serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
                    v["config_version"]
                        .as_str()
                        .unwrap_or("?")
                        .to_string()
                }
                _ => "unreachable".to_string(),
            };

            let mut out = format!("## Config Sync Status\n\nConfig version: {config_version}\n\n");
            out.push_str("| File Type | Hash | Updated By | Updated At |\n|---|---|---|---|\n");

            for ft in &[
                "global_settings",
                "global_claude_md",
                "project_settings",
                "project_claude_md",
            ] {
                let mut url = format!("{}/api/v1/config/{}", base, ft);
                if let Some(ref pid) = params.project_id {
                    url.push_str(&format!("?project_id={}", pid));
                }

                match client
                    .get(&url)
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => {
                        let body = r.text().await.unwrap_or_default();
                        let v: serde_json::Value =
                            serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
                        let hash = v["content_hash"]
                            .as_str()
                            .map(|h| &h[..8.min(h.len())])
                            .unwrap_or("-");
                        let by = v["updated_by"].as_str().unwrap_or("-");
                        let at = v["updated_at"].as_str().unwrap_or("-");
                        out.push_str(&format!("| {ft} | {hash}... | {by} | {at} |\n"));
                    }
                    _ => {
                        out.push_str(&format!("| {ft} | (not synced) | - | - |\n"));
                    }
                }
            }

            tool_ok(out)
        }
        "push" => {
            let ft = match params.file_type {
                Some(ref ft) => ft.clone(),
                None => return tool_error("file_type is required for push action"),
            };
            let content = match params.content {
                Some(ref c) => c.clone(),
                None => {
                    match file_type_to_local_path(&ft, config) {
                        Some(path) => match std::fs::read_to_string(&path) {
                            Ok(c) => c,
                            Err(e) => return tool_error(format!(
                                "No content provided and failed to read local file '{}': {e}",
                                path.display()
                            )),
                        },
                        None => return tool_error(
                            "No content provided and cannot resolve local path for file_type. \
                             Either pass content or ensure workspace_path is configured."
                        ),
                    }
                }
            };
            let wid = params
                .workstation_id
                .as_deref()
                .unwrap_or("mcp-tool")
                .to_string();

            let body = serde_json::json!({
                "project_id": params.project_id,
                "content": content,
                "workstation_id": wid,
            });

            match client
                .post(format!("{}/api/v1/config/{}", base, ft))
                .json(&body)
                .timeout(Duration::from_secs(10))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let body = r.text().await.unwrap_or_default();
                    let v: serde_json::Value =
                        serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
                    tool_ok(format!(
                        "Push result: {} (config version: {})",
                        v["status"].as_str().unwrap_or("?"),
                        v["config_version"].as_str().unwrap_or("?")
                    ))
                }
                Ok(r) => tool_error(format!("push failed: HTTP {}", r.status())),
                Err(e) => tool_error(format!("push failed: {e}")),
            }
        }
        "pull" => {
            let ft = match params.file_type {
                Some(ref ft) => ft.clone(),
                None => return tool_error("file_type is required for pull action"),
            };

            let mut url = format!("{}/api/v1/config/{}", base, ft);
            if let Some(ref pid) = params.project_id {
                url.push_str(&format!("?project_id={}", pid));
            }

            match client
                .get(&url)
                .timeout(Duration::from_secs(5))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let body = r.text().await.unwrap_or_default();
                    let v: serde_json::Value =
                        serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
                    let content = v["content"].as_str().unwrap_or("");
                    let hash = v["content_hash"].as_str().unwrap_or("?");
                    let by = v["updated_by"].as_str().unwrap_or("?");

                    match file_type_to_local_path(&ft, config) {
                        Some(path) => {
                            let local_hash = std::fs::read(&path)
                                .map(|bytes| format!("{:x}", Sha256::digest(&bytes)))
                                .unwrap_or_default();

                            if local_hash == hash {
                                tool_ok(format!(
                                    "## Pulled: {ft}\n\nLocal file already up to date.\n\
                                     Path: `{}`\nHash: {hash}\nUpdated by: {by}",
                                    path.display()
                                ))
                            } else {
                                match write_with_backup(&path, content) {
                                    Ok(()) => tool_ok(format!(
                                        "## Pulled: {ft}\n\nWritten to: `{}`\n\
                                         Hash: {hash}\nUpdated by: {by}\n\
                                         Backup: `{}.bak`",
                                        path.display(),
                                        path.display()
                                    )),
                                    Err(e) => tool_error(format!(
                                        "Fetched {ft} from remote but failed to write: {e}\n\n\
                                         Content (not saved):\n```\n{content}\n```"
                                    )),
                                }
                            }
                        }
                        None => {
                            tool_ok(format!(
                                "## Pulled: {ft}\n\nHash: {hash}\nUpdated by: {by}\n\
                                 **Warning:** Could not resolve local path for '{ft}' \
                                 (workspace_path may not be configured).\n\n```\n{content}\n```"
                            ))
                        }
                    }
                }
                Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => {
                    tool_error(format!("no config found for file_type '{ft}'"))
                }
                Ok(r) => tool_error(format!("pull failed: HTTP {}", r.status())),
                Err(e) => tool_error(format!("pull failed: {e}")),
            }
        }
        _ => tool_error(format!(
            "Unknown action '{}'. Expected: push, pull, status",
            params.action
        )),
    }
}

// ─────────────────────────────────────────────────────────────────
// gaming_tool — Cloud gaming VM orchestration
// ─────────────────────────────────────────────────────────────────

/// Parameters for the `gaming` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GamingParams {
    /// Action: "status", "launch", "stop", "list-gpus".
    pub action: String,
    /// VM name (required for launch/stop).
    #[serde(default)]
    pub vm_name: Option<String>,
}

/// Manage cloud gaming VMs on Thor (Proxmox).
#[instrument(skip(client, config), fields(action = %params.action))]
pub async fn gaming(
    client: &Client,
    config: &McpServerConfig,
    params: GamingParams,
) -> CallToolResult {
    let timeout = Duration::from_secs(config.timeout_secs);
    let url = format!(
        "{}/api/v1/gaming",
        config.odin_url.trim_end_matches('/')
    );

    let body = serde_json::json!({
        "action": params.action,
        "vm_name": params.vm_name
    });

    let resp = match client.post(&url).json(&body).timeout(timeout).send().await {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!("Gaming request timed out after {}s", config.timeout_secs));
        }
        Err(e) => {
            return tool_error(format!("Gaming service unreachable: {e}"));
        }
    };

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.is_success() {
        tool_ok(text)
    } else {
        tool_error(format!("Gaming HTTP {status}: {text}"))
    }
}

// ─────────────────────────────────────────────────────────────────
// deploy_tool — Build and deploy binaries to nodes
// ─────────────────────────────────────────────────────────────────

/// Parameters for the `deploy` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeployParams {
    /// Action: "build", "deploy", "status".
    pub action: String,
    /// Service binary name: "odin", "mimir", "muninn", "huginn", etc.
    pub service: String,
    /// Target node: "munin", "hugin". Omit to deploy to both.
    #[serde(default)]
    pub node: Option<String>,
}

/// Build and deploy Yggdrasil service binaries.
#[instrument(skip(_client, config), fields(action = %params.action, service = %params.service))]
pub async fn deploy(
    _client: &Client,
    config: &McpServerConfig,
    params: DeployParams,
) -> CallToolResult {
    let workspace = match &config.workspace_path {
        Some(p) => p.clone(),
        None => return tool_error("workspace_path not configured — cannot run deploy"),
    };

    match params.action.as_str() {
        "build" => {
            let output = tokio::time::timeout(
                Duration::from_secs(300),
                tokio::process::Command::new("cargo")
                    .args(["build", "--release", "--bin", &params.service])
                    .current_dir(&workspace)
                    .output(),
            )
            .await;

            match output {
                Ok(Ok(o)) if o.status.success() => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    tool_ok(format!("Build successful for `{}`.\n{stdout}", params.service))
                }
                Ok(Ok(o)) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    tool_error(format!("Build failed:\n{stderr}"))
                }
                Ok(Err(e)) => tool_error(format!("Failed to run cargo: {e}")),
                Err(_) => tool_error("Build timed out after 300 seconds"),
            }
        }
        "deploy" => {
            let nodes: Vec<&str> = match params.node.as_deref() {
                Some("munin") => vec!["munin"],
                Some("hugin") => vec!["hugin"],
                _ => vec!["munin", "hugin"],
            };

            let bin_path = format!("{}/target/release/{}", workspace, params.service);
            let mut results = Vec::new();

            for node in &nodes {
                let dest = format!("yggdrasil@{}:/opt/yggdrasil/bin/{}", node, params.service);
                let output = tokio::process::Command::new("rsync")
                    .args(["-az", "--progress", &bin_path, &dest])
                    .output()
                    .await;

                match output {
                    Ok(o) if o.status.success() => {
                        results.push(format!("{node}: deployed successfully"));
                    }
                    Ok(o) => {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        results.push(format!("{node}: rsync failed — {stderr}"));
                    }
                    Err(e) => {
                        results.push(format!("{node}: rsync error — {e}"));
                    }
                }
            }

            tool_ok(results.join("\n"))
        }
        "status" => {
            let bin = format!("{}/target/release/{}", workspace, params.service);
            let exists = std::path::Path::new(&bin).exists();
            if exists {
                let meta = std::fs::metadata(&bin);
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                tool_ok(format!(
                    "Binary `{}` exists ({:.1} MB)",
                    params.service,
                    size as f64 / 1_048_576.0
                ))
            } else {
                tool_ok(format!("Binary `{}` not found — run build first", params.service))
            }
        }
        _ => tool_error(format!(
            "Unknown deploy action '{}'. Expected: build, deploy, status",
            params.action
        )),
    }
}

// ─────────────────────────────────────────────────────────────────
// network_topology_tool — Mesh node and service discovery
// ─────────────────────────────────────────────────────────────────

/// Parameters for the `network_topology` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NetworkTopologyParams {
    /// Action: "nodes" (list mesh nodes), "services" (list services), "health" (check all).
    #[serde(default = "default_topology_action")]
    pub action: Option<String>,
    /// Filter by node name.
    #[serde(default)]
    pub node_name: Option<String>,
}

fn default_topology_action() -> Option<String> {
    Some("nodes".to_string())
}

/// Query the mesh network topology — nodes, services, and health.
#[instrument(skip(client, config))]
pub async fn network_topology(
    client: &Client,
    config: &McpServerConfig,
    params: NetworkTopologyParams,
) -> CallToolResult {
    let timeout = Duration::from_secs(config.timeout_secs);
    let action = params.action.as_deref().unwrap_or("nodes");
    let base = config.odin_url.trim_end_matches('/');

    let url = match action {
        "nodes" => format!("{base}/api/v1/mesh/nodes"),
        "services" => format!("{base}/api/v1/mesh/services"),
        "health" => {
            // Aggregate health from service_health endpoint.
            return service_health(client, config, ServiceHealthParams { services: None }).await;
        }
        _ => {
            return tool_error(format!(
                "Unknown topology action '{action}'. Expected: nodes, services, health"
            ));
        }
    };

    let resp = match client.get(&url).timeout(timeout).send().await {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return tool_error(format!("Topology request timed out after {}s", config.timeout_secs));
        }
        Err(e) => {
            return tool_error(format!("Mesh service unreachable: {e}"));
        }
    };

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.is_success() {
        tool_ok(format!("## Mesh {action}\n\n{text}"))
    } else {
        tool_error(format!("Mesh HTTP {status}: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ha_call_service_rejects_disallowed_domain() {
        let params = HaCallServiceParams {
            domain: "lock".to_string(),
            service: "unlock".to_string(),
            data: serde_json::json!({"entity_id": "lock.front_door"}),
        };
        let result = ha_call_service(None, params).await;
        // Should fail because "lock" is not in the allowlist.
        assert!(result.is_error.unwrap_or(false));
    }

    #[tokio::test]
    async fn ha_call_service_allows_light_domain() {
        // With no HA client configured, this will fail because "not configured",
        // NOT because of domain validation — meaning light domain passed the check.
        let params = HaCallServiceParams {
            domain: "light".to_string(),
            service: "turn_on".to_string(),
            data: serde_json::json!({"entity_id": "light.living_room"}),
        };
        let result = ha_call_service(None, params).await;
        // Should fail with "not configured", not "domain not allowed".
        assert!(result.is_error.unwrap_or(false));
        let text = match &result.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(text.contains("not configured"), "got: {text}");
    }

    #[tokio::test]
    async fn ha_call_service_no_client_returns_error() {
        let params = HaCallServiceParams {
            domain: "switch".to_string(),
            service: "toggle".to_string(),
            data: serde_json::json!({}),
        };
        let result = ha_call_service(None, params).await;
        assert!(result.is_error.unwrap_or(false));
    }

    #[test]
    fn test_parse_file_blocks() {
        let content = r#"Here are the changes:

```src/main.rs
fn main() {
    println!("hello");
}
```

Some explanation text.

```src/lib.rs
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
```
"#;

        let blocks = parse_file_blocks(content);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "src/main.rs");
        assert!(blocks[0].1.contains("println"));
        assert_eq!(blocks[1].0, "src/lib.rs");
        assert!(blocks[1].1.contains("pub fn add"));
    }

    #[test]
    fn test_parse_file_blocks_skips_generic_language() {
        let content = r#"```rust
fn example() {}
```

```src/real.rs
fn real() {}
```
"#;

        let blocks = parse_file_blocks(content);
        // "rust" has no dot, so it's treated as a language tag and skipped
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, "src/real.rs");
    }
}
