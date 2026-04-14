//! Parameter structs for all MCP tools in the Yggdrasil ecosystem.
//!
//! This is the **single source of truth** for tool parameter schemas.
//! Both `odin::tool_registry` (via `schema_for_tool()`) and `ygg-mcp::server`
//! consume these types.  Moving them here from `ygg-mcp/src/tools.rs`
//! eliminates schema duplication and ensures compile-time consistency.

use schemars::JsonSchema;
use serde::Deserialize;

// ── Default value functions ─────────────────────────────────────────

fn default_search_limit() -> Option<u32> { Some(10) }
fn default_query_limit() -> Option<u32> { Some(5) }
fn default_sprint_history_limit() -> Option<u32> { Some(5) }
fn default_sdr_operation() -> String { "and".to_string() }
fn default_intersect_limit() -> Option<u32> { Some(5) }
fn default_build_mode() -> Option<String> { Some("check".to_string()) }
fn default_timeline_limit() -> Option<u32> { Some(10) }
fn default_delegate_max_tokens() -> Option<u64> { Some(8192) }
fn default_review_focus() -> Option<String> { Some("all".to_string()) }
fn default_ast_limit() -> Option<u32> { Some(20) }
fn default_impact_limit() -> Option<u32> { Some(20) }
fn default_task_queue_limit() -> Option<u32> { Some(20) }
fn default_graph_tool_limit() -> Option<u32> { Some(20) }
fn default_topology_action() -> Option<String> { Some("nodes".to_string()) }
fn default_web_search_count() -> Option<u32> { Some(5) }
fn default_true() -> bool { true }

// ── Parameter structs ───────────────────────────────────────────────

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

/// Parameters for the `query_memory` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryMemoryParams {
    /// Query text to search engram memory.
    pub text: String,
    /// Maximum number of engrams to return (default 5, max 20).
    #[serde(default = "default_query_limit")]
    pub limit: Option<u32>,
    /// Optional tag filter — only return engrams matching ALL specified tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// Parameters for the `store_memory` tool.
///
/// Sprint 063 P3b: `tags`, `id`, and `force` carry `#[serde(default)]` so
/// the generated JSON schema marks them as NOT required. Without this,
/// schemars emits a schema that requires the fields to be present (even
/// as JSON `null`), causing strict MCP clients to reject calls like
/// `store_memory(cause="x", effect="y")` — and, perversely, also to
/// reject calls that supply `tags=["a","b"]` when the serialized form
/// omits `id`/`force`. Matching the pattern used by `QueryMemoryParams`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StoreMemoryParams {
    /// The trigger or question (what happened).
    pub cause: String,
    /// The outcome or answer (what resulted).
    pub effect: String,
    /// Optional tags for categorization.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Optional engram UUID for update-by-ID.
    #[serde(default)]
    pub id: Option<String>,
    /// Set to true to bypass the novelty gate.
    #[serde(default)]
    pub force: Option<bool>,
}

/// Parameters for the `generate` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GenerateParams {
    /// The prompt or question to send to the LLM.
    pub prompt: String,
    /// Model name. If omitted, uses Odin's default routing.
    pub model: Option<String>,
    /// Maximum tokens to generate (optional, default 4096).
    pub max_tokens: Option<u64>,
}

/// Parameters for the `get_sprint_history` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetSprintHistoryParams {
    /// Project name to filter by. Optional — returns all sprint engrams if omitted.
    pub project: Option<String>,
    /// Maximum number of sprint summaries to return (default 5).
    #[serde(default = "default_sprint_history_limit")]
    pub limit: Option<u32>,
}

/// Parameters for the `sync_docs` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncDocsParams {
    /// Lifecycle event: "sprint_start", "sprint_end", or "setup".
    pub event: String,
    /// Sprint identifier, e.g. "027".
    #[serde(default)]
    pub sprint_id: String,
    /// Full content of the sprint document.
    #[serde(default)]
    pub sprint_content: String,
    /// Workspace root path. Overrides config.workspace_path.
    #[serde(default)]
    pub workspace_path: Option<String>,
}

/// Parameters for the `memory_intersect` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryIntersectParams {
    /// Two or more texts to embed and combine.
    pub texts: Vec<String>,
    /// SDR set operation: "and", "or", or "xor".
    #[serde(default = "default_sdr_operation")]
    pub operation: String,
    /// Maximum number of matching engrams (default 5, max 20).
    #[serde(default = "default_intersect_limit")]
    pub limit: Option<u32>,
}

/// Parameters for the `screenshot` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenshotParams {
    /// URL to capture.
    pub url: String,
    /// Optional CSS selector to wait for before capture.
    #[serde(default)]
    pub selector: Option<String>,
    /// Capture the full scrollable page (default: false).
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
    /// Optional: only check specific services.
    #[serde(default)]
    pub services: Option<Vec<String>>,
}

/// Parameters for the `build_check` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuildCheckParams {
    /// Check mode: "check" (default), "clippy", or "test".
    #[serde(default = "default_build_mode")]
    pub mode: Option<String>,
    /// Optional: specific crate to check. Default: whole workspace.
    #[serde(default)]
    pub crate_name: Option<String>,
}

/// Parameters for the `memory_timeline` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryTimelineParams {
    /// Optional semantic search text.
    #[serde(default)]
    pub text: Option<String>,
    /// ISO 8601 datetime — only engrams after this time.
    #[serde(default)]
    pub after: Option<String>,
    /// ISO 8601 datetime — only engrams before this time.
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

/// Parameters for the `context_offload` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextOffloadParams {
    /// Action: "store", "retrieve", or "list".
    pub action: String,
    /// For "store": the content to offload.
    #[serde(default)]
    pub content: Option<String>,
    /// For "store": optional label.
    #[serde(default)]
    pub label: Option<String>,
    /// For "retrieve": the handle ID to fetch.
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
    /// Optional: constraints.
    #[serde(default)]
    pub constraints: Option<Vec<String>>,
    /// Optional: target language (default: "rust").
    #[serde(default)]
    pub language: Option<String>,
    /// Optional: model override.
    #[serde(default)]
    pub model: Option<String>,
    /// Max tokens for response (default: 8192).
    #[serde(default = "default_delegate_max_tokens")]
    pub max_tokens: Option<u64>,
}

/// Inline file content for the delegate tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileContext {
    /// File path (for display/context).
    pub path: String,
    /// File content.
    pub content: String,
}

/// Parameters for the unified `delegate` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DelegateParams {
    /// Agent type: "executor", "docs", "qa", "review", "general".
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
    /// Inline file content to include as context.
    #[serde(default)]
    pub file_context: Option<Vec<FileContext>>,
    /// Reference pattern code to follow.
    #[serde(default)]
    pub reference_pattern: Option<String>,
    /// Whether to parse output as file blocks.
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
    /// Enable agentic tool-use mode.
    #[serde(default)]
    pub agentic: Option<bool>,
    /// Tool allowlist for agentic mode.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// Enable post-generation code review pass (Sprint 054 multi-agent pipeline).
    /// When true, generated code is sent to the review specialist model for
    /// convention checking before being returned.
    #[serde(default)]
    pub review: Option<bool>,
    /// Enable post-generation test generation (Sprint 054 multi-agent pipeline).
    /// When true, a test specialist model generates tests for the output code.
    #[serde(default)]
    pub generate_tests: Option<bool>,
    /// Override model for the review pass. Uses Odin routing default if absent.
    #[serde(default)]
    pub review_model: Option<String>,
    /// Override model for test generation. Uses Odin routing default if absent.
    #[serde(default)]
    pub test_model: Option<String>,
}

/// Parameters for the `diff_review` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiffReviewParams {
    /// Git diff text, or file content to review.
    pub content: String,
    /// Review focus: "security", "performance", "architecture", "bugs", "all".
    #[serde(default = "default_review_focus")]
    pub focus: Option<String>,
    /// Description of the change's intent.
    #[serde(default)]
    pub description: Option<String>,
}

/// Parameters for the `context_bridge` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextBridgeParams {
    /// Action: "export" or "import".
    pub action: String,
    /// For "export": optional label. For "import": context snapshot ID.
    #[serde(default)]
    pub context_id: Option<String>,
    /// Workspace identifier for scoping bridge operations.
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// Parameters for the `ast_analyze` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AstAnalyzeParams {
    /// Symbol name to look up.
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

/// Parameters for the `impact_analysis` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImpactAnalysisParams {
    /// Symbol name to find references for.
    pub symbol: String,
    /// Optional language filter.
    #[serde(default)]
    pub language: Option<String>,
    /// Optional: UUID of the symbol's definition chunk to exclude.
    #[serde(default)]
    pub exclude_id: Option<String>,
    /// Max results (default 20, max 50).
    #[serde(default = "default_impact_limit")]
    pub limit: Option<u32>,
}

/// Parameters for the `task_queue` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskQueueParams {
    /// Action: "push", "pop", "complete", "cancel", or "list".
    pub action: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub success: Option<bool>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default = "default_task_queue_limit")]
    pub limit: Option<u32>,
}

/// Parameters for the `memory_graph` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryGraphParams {
    /// Action: "link", "unlink", "neighbors", or "traverse".
    pub action: String,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub relation: Option<String>,
    #[serde(default)]
    pub weight: Option<f32>,
    #[serde(default)]
    pub engram_id: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub start_id: Option<String>,
    #[serde(default)]
    pub max_depth: Option<u32>,
    #[serde(default = "default_graph_tool_limit")]
    pub limit: Option<u32>,
}

/// Parameters for the `config_version` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigVersionParams {
    /// Action: "check", "bump", or "info".
    pub action: String,
    #[serde(default)]
    pub bump_type: Option<String>,
    #[serde(default)]
    pub component: Option<String>,
}

/// Parameters for the `config_sync` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigSyncParams {
    /// Action: "push", "pull", or "status".
    pub action: String,
    #[serde(default)]
    pub file_type: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub workstation_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Parameters for the `vault` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VaultParams {
    /// Action: "get", "set", "list", "delete".
    pub action: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// Parameters for the `ha_get_states` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HaGetStatesParams {
    /// If true (default), return a compact markdown summary grouped by domain.
    #[serde(default = "default_true")]
    pub summary: bool,
}

/// Parameters for the `ha_list_entities` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HaListEntitiesParams {
    /// HA domain to filter by (e.g., "light", "switch", "sensor").
    pub domain: Option<String>,
}

/// Parameters for the `ha_call_service` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HaCallServiceParams {
    /// HA service domain (e.g., "light", "switch").
    pub domain: String,
    /// Service name (e.g., "turn_on", "turn_off").
    pub service: String,
    /// Service call data.
    pub data: serde_json::Value,
}

/// Parameters for the `ha_generate_automation` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HaGenerateAutomationParams {
    /// Natural language description of the desired automation.
    pub description: String,
}

/// Parameters for the `gaming` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GamingParams {
    /// Action: "status", "launch", "stop", "list-gpus", "pair".
    pub action: String,
    #[serde(default)]
    pub vm_name: Option<String>,
    #[serde(default)]
    pub pin: Option<String>,
}

/// Parameters for the `deploy` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeployParams {
    /// Action: "build", "deploy", "status".
    pub action: String,
    /// Service binary name.
    pub service: String,
    /// Target node. Omit to deploy to both.
    #[serde(default)]
    pub node: Option<String>,
}

/// Parameters for the `network_topology` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NetworkTopologyParams {
    /// Action: "nodes", "services", "health".
    #[serde(default = "default_topology_action")]
    pub action: Option<String>,
    #[serde(default)]
    pub node_name: Option<String>,
}

/// Parameters for the `web_search` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WebSearchParams {
    /// Search query.
    pub query: String,
    /// Number of results to return (default 5, max 10).
    #[serde(default = "default_web_search_count")]
    pub count: Option<u32>,
}

fn default_doc_search_limit() -> Option<u32> { Some(10) }

/// Parameters for the `search_documents` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchDocumentsParams {
    /// Search query (semantic + keyword).
    pub query: String,
    /// Filter by document type: "pdf", "markdown", "text", "transcript".
    #[serde(default)]
    pub doc_types: Option<Vec<String>>,
    /// Filter by project name.
    #[serde(default)]
    pub project: Option<String>,
    /// Maximum number of chunks to return (default 10, max 50).
    #[serde(default = "default_doc_search_limit")]
    pub limit: Option<u32>,
}

/// Parameters for the `ingest_document` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IngestDocumentParams {
    /// Source URI or file path identifying the document.
    pub source_uri: String,
    /// Full text content of the document.
    pub content: String,
    /// Document type: "markdown", "text", "pdf_text", "transcript".
    pub doc_type: String,
    /// Optional document title.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional project scope.
    #[serde(default)]
    pub project: Option<String>,
}

/// Parameters for the `research_report` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResearchReportParams {
    /// The original research query.
    pub query: String,
    /// Synthesized findings to store.
    pub findings: String,
    /// Tags for the research engram (auto-includes "research").
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

// ── Schema generation ───────────────────────────────────────────────

/// Generate the JSON Schema for a tool's parameter type by name.
///
/// Returns `None` only for tools that have no known param struct (should not
/// happen — every tool in `ALL_TOOLS` should have a corresponding entry here).
pub fn schema_for_tool(name: &str) -> Option<serde_json::Value> {
    let schema = match name {
        "search_code" => schemars::schema_for!(SearchCodeParams),
        "query_memory" => schemars::schema_for!(QueryMemoryParams),
        "store_memory" => schemars::schema_for!(StoreMemoryParams),
        "generate" => schemars::schema_for!(GenerateParams),
        "get_sprint_history" => schemars::schema_for!(GetSprintHistoryParams),
        "sync_docs" => schemars::schema_for!(SyncDocsParams),
        "memory_intersect" => schemars::schema_for!(MemoryIntersectParams),
        "screenshot" => schemars::schema_for!(ScreenshotParams),
        "service_health" => schemars::schema_for!(ServiceHealthParams),
        "build_check" => schemars::schema_for!(BuildCheckParams),
        "memory_timeline" => schemars::schema_for!(MemoryTimelineParams),
        "context_offload" => schemars::schema_for!(ContextOffloadParams),
        "task_delegate" => schemars::schema_for!(TaskDelegateParams),
        "delegate" => schemars::schema_for!(DelegateParams),
        "diff_review" => schemars::schema_for!(DiffReviewParams),
        "context_bridge" => schemars::schema_for!(ContextBridgeParams),
        "ast_analyze" => schemars::schema_for!(AstAnalyzeParams),
        "impact_analysis" => schemars::schema_for!(ImpactAnalysisParams),
        "task_queue" => schemars::schema_for!(TaskQueueParams),
        "memory_graph" => schemars::schema_for!(MemoryGraphParams),
        "config_version" => schemars::schema_for!(ConfigVersionParams),
        "config_sync" => schemars::schema_for!(ConfigSyncParams),
        "vault" => schemars::schema_for!(VaultParams),
        "ha_get_states" => schemars::schema_for!(HaGetStatesParams),
        "ha_list_entities" => schemars::schema_for!(HaListEntitiesParams),
        "ha_call_service" => schemars::schema_for!(HaCallServiceParams),
        "ha_generate_automation" => schemars::schema_for!(HaGenerateAutomationParams),
        "gaming" => schemars::schema_for!(GamingParams),
        "deploy" => schemars::schema_for!(DeployParams),
        "network_topology" => schemars::schema_for!(NetworkTopologyParams),
        "web_search" => schemars::schema_for!(WebSearchParams),
        "search_documents" => schemars::schema_for!(SearchDocumentsParams),
        "ingest_document" => schemars::schema_for!(IngestDocumentParams),
        "research_report" => schemars::schema_for!(ResearchReportParams),
        "list_models" => return Some(serde_json::json!({"type": "object", "properties": {}})),
        _ => return None,
    };
    serde_json::to_value(schema).ok()
}
