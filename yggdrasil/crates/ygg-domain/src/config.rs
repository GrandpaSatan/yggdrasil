use serde::{Deserialize, Serialize};

/// Odin orchestrator configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdinConfig {
    pub node_name: String,
    pub listen_addr: String,
    pub backends: Vec<BackendConfig>,
    pub routing: RoutingConfig,
    pub mimir: MimirClientConfig,
    pub muninn: MuninnClientConfig,
    /// Optional Home Assistant integration. If absent, HA features are disabled.
    #[serde(default)]
    pub ha: Option<HaConfig>,
    /// Session state configuration. Uses defaults when absent.
    #[serde(default)]
    pub session: SessionConfig,
    /// Optional cloud provider backends for fallback routing.
    /// When local backends are at capacity, Odin falls back to these cloud APIs.
    #[serde(default)]
    pub cloud: Option<CloudProvidersConfig>,
    /// Optional voice streaming configuration. When present and enabled, Odin
    /// exposes a WebSocket endpoint at `/v1/voice` that proxies STT/TTS through
    /// ygg-voice's HTTP API.
    #[serde(default)]
    pub voice: Option<VoiceStreamConfig>,
    /// Agent loop configuration for autonomous LLM tool-use.
    /// When present, Odin can run an agent loop where local LLMs call MCP tools.
    #[serde(default)]
    pub agent: Option<AgentLoopConfig>,
}

/// Cloud provider configuration for fallback routing through ygg-cloud.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudProvidersConfig {
    /// Enable cloud fallback when local backends return 503.
    #[serde(default)]
    pub fallback_enabled: bool,
    /// OpenAI API configuration.
    #[serde(default)]
    pub openai: Option<CloudProviderEntry>,
    /// Anthropic Claude API configuration.
    #[serde(default)]
    pub claude: Option<CloudProviderEntry>,
    /// Google Gemini API configuration.
    #[serde(default)]
    pub gemini: Option<CloudProviderEntry>,
}

/// A single cloud provider's credentials and limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudProviderEntry {
    /// API key (use ${ENV_VAR} expansion for secrets).
    pub api_key: String,
    /// Default model to use for this provider.
    pub default_model: String,
    /// Max requests per minute (rate limiting).
    #[serde(default = "default_cloud_rpm")]
    pub requests_per_minute: u32,
}

fn default_cloud_rpm() -> u32 {
    60
}

/// Voice streaming configuration for WebSocket-based STT/TTS proxying.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceStreamConfig {
    /// Whether voice streaming is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Base URL for the TTS HTTP API (e.g. "http://localhost:9095").
    #[serde(default = "default_voice_api_url")]
    pub voice_api_url: String,
    /// Base URL for a dedicated STT service (e.g. "http://localhost:9097").
    /// When absent, STT calls go to `voice_api_url` (ygg-voice serves both).
    #[serde(default)]
    pub stt_url: Option<String>,
}

fn default_voice_api_url() -> String {
    "http://localhost:9095".to_string()
}

/// Configuration for Odin's autonomous agent loop.
///
/// When a `/v1/chat/completions` request includes a `tools` array, Odin enters
/// an agent loop: send to Ollama with tool definitions, execute tool calls,
/// feed results back, repeat until the model produces text or limits are hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLoopConfig {
    /// Maximum reasoning iterations before forcing a text response.
    #[serde(default = "default_agent_max_iterations")]
    pub max_iterations: usize,
    /// Absolute cap on tool calls across all iterations.
    #[serde(default = "default_agent_max_tool_calls")]
    pub max_tool_calls_total: usize,
    /// Timeout in seconds for each individual tool HTTP call.
    #[serde(default = "default_agent_tool_timeout")]
    pub tool_timeout_secs: u64,
    /// Total timeout in seconds for the entire agent loop.
    #[serde(default = "default_agent_total_timeout")]
    pub total_timeout_secs: u64,
    /// Default tool tiers allowed: "safe", "restricted". Blocked is never allowed.
    #[serde(default = "default_agent_tiers")]
    pub default_tiers: Vec<String>,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: default_agent_max_iterations(),
            max_tool_calls_total: default_agent_max_tool_calls(),
            tool_timeout_secs: default_agent_tool_timeout(),
            total_timeout_secs: default_agent_total_timeout(),
            default_tiers: default_agent_tiers(),
        }
    }
}

fn default_agent_max_iterations() -> usize { 10 }
fn default_agent_max_tool_calls() -> usize { 30 }
fn default_agent_tool_timeout() -> u64 { 30 }
fn default_agent_total_timeout() -> u64 { 300 }
fn default_agent_tiers() -> Vec<String> { vec!["safe".to_string()] }

/// Session state configuration for Odin's in-memory conversation store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Maximum number of concurrent sessions before eviction.
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    /// Session TTL in seconds. Sessions idle longer than this are evicted.
    #[serde(default = "default_session_ttl_secs")]
    pub session_ttl_secs: u64,
    /// Token budget for context packing (must fit within Ollama context window).
    #[serde(default = "default_context_budget_tokens")]
    pub context_budget_tokens: usize,
    /// Tokens reserved for model generation output.
    #[serde(default = "default_generation_reserve")]
    pub generation_reserve: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_sessions: default_max_sessions(),
            session_ttl_secs: default_session_ttl_secs(),
            context_budget_tokens: default_context_budget_tokens(),
            generation_reserve: default_generation_reserve(),
        }
    }
}

fn default_max_sessions() -> usize {
    256
}

fn default_session_ttl_secs() -> u64 {
    3600
}

fn default_context_budget_tokens() -> usize {
    14000
}

fn default_generation_reserve() -> usize {
    2048
}

/// Backend protocol type.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackendType {
    #[default]
    Ollama,
    Openai,
}

/// A backend node (Ollama or OpenAI-compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub backend_type: BackendType,
    pub models: Vec<String>,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Total context window size in tokens for this backend's model.
    /// Used to compute per-backend context budgets and set `num_ctx`.
    #[serde(default = "default_context_window")]
    pub context_window: usize,
}

fn default_max_concurrent() -> usize {
    2
}

fn default_context_window() -> usize {
    16384
}

/// Routing rules for the semantic router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    pub default_model: String,
    /// Explicit default backend name. When set, used for unmatched intents
    /// instead of inferring from `default_model`'s backend.
    #[serde(default)]
    pub default_backend: Option<String>,
    pub rules: Vec<RoutingRule>,
}

/// A single routing rule mapping intent to model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    pub intent: String,
    pub model: String,
    pub backend: String,
}

/// Client config for connecting to Mimir.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MimirClientConfig {
    pub url: String,
    #[serde(default = "default_query_limit")]
    pub query_limit: usize,
    #[serde(default = "default_true")]
    pub store_on_completion: bool,
}

fn default_query_limit() -> usize {
    5
}

fn default_true() -> bool {
    true
}

/// Client config for connecting to Muninn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuninnClientConfig {
    pub url: String,
    #[serde(default = "default_max_context_chunks")]
    pub max_context_chunks: usize,
}

fn default_max_context_chunks() -> usize {
    10
}

/// Mimir memory service configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MimirConfig {
    pub listen_addr: String,
    pub database_url: String,
    pub qdrant_url: String,
    pub sdr: SdrConfig,
    pub tiers: TierConfig,
}

/// Embedding configuration — ONNX in-process model directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedConfig {
    /// Path to the ONNX model directory (contains model.onnx + tokenizer.json).
    pub model_dir: String,
}

/// SDR (Sparse Distributed Representation) configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdrConfig {
    /// Number of bits in the SDR (default 256).
    #[serde(default = "default_sdr_dim_bits")]
    pub dim_bits: usize,
    /// Path to the ONNX model directory (contains model.onnx + tokenizer.json).
    pub model_dir: String,
    /// Hamming similarity threshold for semantic dedup on store (default 0.85).
    /// Set to 1.0 to disable (only exact SDR matches rejected).
    #[serde(default = "default_dedup_threshold")]
    pub dedup_threshold: f64,
}

fn default_dedup_threshold() -> f64 {
    0.85
}

fn default_sdr_dim_bits() -> usize {
    256
}

/// Memory tier capacity configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierConfig {
    /// Maximum number of Recall tier engrams before summarization triggers.
    #[serde(default = "default_recall_capacity")]
    pub recall_capacity: usize,
    /// Number of engrams to batch for each summarization cycle.
    #[serde(default = "default_summarization_batch")]
    pub summarization_batch_size: usize,
    /// How often to check Recall tier capacity (seconds).
    #[serde(default = "default_check_interval")]
    pub check_interval_secs: u64,
    /// Minimum age (seconds) before a Recall engram is eligible for summarization.
    /// Prevents summarizing very recent engrams that may still be actively accessed.
    #[serde(default = "default_min_age_secs")]
    pub min_age_secs: u64,
    /// Odin URL for summarization LLM calls.
    #[serde(default = "default_summarization_odin_url")]
    pub odin_url: String,
}

fn default_recall_capacity() -> usize {
    1000
}

fn default_summarization_batch() -> usize {
    100
}

fn default_check_interval() -> u64 {
    300 // 5 minutes
}

fn default_min_age_secs() -> u64 {
    86400 // 24 hours
}

fn default_summarization_odin_url() -> String {
    "http://localhost:8080".to_string()
}

/// Huginn indexer configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HuginnConfig {
    pub watch_paths: Vec<String>,
    pub database_url: String,
    pub qdrant_url: String,
    pub embed: EmbedConfig,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    /// Address for health/metrics HTTP listener (default "0.0.0.0:9092").
    #[serde(default = "default_huginn_listen_addr")]
    pub listen_addr: String,
}

fn default_debounce_ms() -> u64 {
    500
}

fn default_huginn_listen_addr() -> String {
    "0.0.0.0:9092".to_string()
}

/// Muninn retrieval engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuninnConfig {
    pub listen_addr: String,
    pub database_url: String,
    pub qdrant_url: String,
    pub embed: EmbedConfig,
    pub search: SearchConfig,
}

/// Search tuning parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_rrf_k")]
    pub rrf_k: f64,
    #[serde(default = "default_context_token_budget")]
    pub context_token_budget: usize,
    #[serde(default = "default_context_fill_ratio")]
    pub context_fill_ratio: f64,
}

fn default_rrf_k() -> f64 {
    60.0
}

fn default_context_token_budget() -> usize {
    32000
}

fn default_context_fill_ratio() -> f64 {
    0.8
}

/// Home Assistant integration configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HaConfig {
    pub url: String,
    pub token: String,
    /// HTTP timeout for HA REST API calls in seconds (default: 10).
    #[serde(default = "default_ha_timeout")]
    pub timeout_secs: u64,
}

fn default_ha_timeout() -> u64 {
    10
}

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Base URL for Odin (default: http://localhost:8080)
    #[serde(default = "default_odin_url")]
    pub odin_url: String,
    /// Optional direct URL for Muninn (bypasses Odin proxy). If unset, uses odin_url.
    #[serde(default)]
    pub muninn_url: Option<String>,
    /// HTTP request timeout in seconds (default: 30)
    #[serde(default = "default_mcp_timeout")]
    pub timeout_secs: u64,
    /// Token generation rate (tok/s) for dynamic generate timeout. Default: 15.0
    #[serde(default = "default_generate_tok_per_sec")]
    pub generate_tok_per_sec: f64,
    /// Optional Home Assistant integration. If present, HA MCP tools are registered.
    #[serde(default)]
    pub ha: Option<HaConfig>,
    /// Query used to prefetch session context at startup (default: "active sprint yggdrasil").
    #[serde(default = "default_prefetch_query")]
    pub prefetch_query: String,
    /// Project name for project-scoped session history (e.g. "yggdrasil").
    /// When set, generate_tool passes this to Odin to enable cross-window context continuity.
    #[serde(default)]
    pub project: Option<String>,
    /// Absolute path to the project workspace root (e.g. "/home/your-user/yggdrasil").
    /// Required by sync_docs_tool for reading/writing local files.
    #[serde(default)]
    pub workspace_path: Option<String>,
    /// URL of the remote MCP server for version check + config sync.
    /// Only used by the local server binary. Example: "http://<munin-ip>:9093"
    #[serde(default)]
    pub remote_url: Option<String>,
    /// PostgreSQL database URL for session persistence (optional).
    /// When set, MCP remote server persists session metadata to PG.
    #[serde(default)]
    pub database_url: Option<String>,
    /// Path to SQL migrations directory (default: "migrations").
    #[serde(default = "default_migrations_path")]
    pub migrations_path: Option<String>,
}

fn default_migrations_path() -> Option<String> {
    None
}

fn default_prefetch_query() -> String {
    "active sprint yggdrasil".to_string()
}

fn default_odin_url() -> String {
    "http://localhost:8080".to_string()
}

fn default_mcp_timeout() -> u64 {
    30
}

fn default_generate_tok_per_sec() -> f64 {
    15.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_type_deserializes_from_json() {
        let json = r#"[
            {"name":"munin","url":"http://localhost:11434","backend_type":"ollama","models":[]},
            {"name":"hugin-vllm","url":"http://127.0.0.2:8000","backend_type":"openai","models":["Qwen/QwQ-32B-AWQ"]},
            {"name":"hugin","url":"http://127.0.0.2:11434","models":["qwen3-embedding"]}
        ]"#;
        let backends: Vec<BackendConfig> = serde_json::from_str(json).unwrap();
        assert_eq!(backends[0].backend_type, BackendType::Ollama);
        assert_eq!(backends[1].backend_type, BackendType::Openai);
        assert_eq!(backends[2].backend_type, BackendType::Ollama); // default
    }
}
