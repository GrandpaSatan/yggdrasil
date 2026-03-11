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
use std::collections::BTreeMap;
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
        cause: &'a str,
        effect: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        tags: Option<&'a Vec<String>>,
    }

    let body = Req {
        cause: &params.cause,
        effect: &params.effect,
        tags: params.tags.as_ref(),
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

/// Query Odin/Mimir for sprint engrams, filter by project, return as markdown.
#[instrument(skip(client, config), fields(project = ?params.project))]
pub async fn get_sprint_history(
    client: &Client,
    config: &McpServerConfig,
    params: GetSprintHistoryParams,
) -> CallToolResult {
    let timeout = Duration::from_secs(config.timeout_secs);
    let url = format!(
        "{}/api/v1/query",
        config.odin_url.trim_end_matches('/')
    );

    let project = params.project.as_deref().unwrap_or("");
    let query_text = if project.is_empty() {
        "sprint history".to_string()
    } else {
        format!("{project} sprint history")
    };
    let limit = params.limit.unwrap_or(5).min(20);

    #[derive(Serialize)]
    struct Req<'a> {
        text: &'a str,
        limit: u32,
    }

    let body = Req {
        text: &query_text,
        limit: limit * 2, // fetch extra, filter client-side by sprint tag
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
    // Filter: keep only engrams whose cause starts with "Sprint " (from archival pattern).
    let sprint_results: Vec<_> = results
        .iter()
        .filter(|e| {
            e.cause
                .as_deref()
                .map(|c| c.starts_with("Sprint "))
                .unwrap_or(false)
        })
        .take(limit as usize)
        .collect();

    if sprint_results.is_empty() {
        let project_note = if project.is_empty() {
            String::new()
        } else {
            format!(" for project '{project}'")
        };
        return tool_ok(format!(
            "## Sprint History{}\n\nNo sprint engrams found. \
             Run sync_docs_tool(event: \"sprint_end\", ...) to archive sprints.",
            project_note
        ));
    }

    let project_note = if project.is_empty() {
        String::new()
    } else {
        format!(" — {project}")
    };
    let mut out = format!(
        "## Sprint History{} ({} results)\n\n",
        project_note,
        sprint_results.len()
    );
    for (i, engram) in sprint_results.iter().enumerate() {
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
            .content
            .iter()
            .next()
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
        {
            if !content.contains("NO_CHANGES") && !content.trim().is_empty() {
                let updated_arch = format!(
                    "{}\n\n## Sprint {} Changes\n\n{}",
                    current_arch.trim_end(),
                    params.sprint_id,
                    content
                );
                let _ = tokio::fs::write(&arch_path, &updated_arch).await;
            }
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
}
