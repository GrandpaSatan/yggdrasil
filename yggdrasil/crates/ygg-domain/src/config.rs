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
    /// Background task worker configuration.
    /// When enabled, Odin polls the Mimir task queue and executes tasks autonomously.
    #[serde(default)]
    pub task_worker: Option<TaskWorkerConfig>,
    /// Optional web search configuration (Brave Search API).
    /// When present, enables the `web_search` tool in the agent loop.
    #[serde(default)]
    pub web_search: Option<WebSearchConfig>,
    /// Hybrid SDR + LLM router configuration (Sprint 052).
    /// When present and enabled, Odin uses a Liquid AI LFM model on Hugin for
    /// intelligent intent classification, with SDR prototypes as a fast prior.
    #[serde(default)]
    pub llm_router: Option<LlmRouterConfig>,
    /// Multi-model flow pipelines (Sprint 055).
    /// When configured, flows take priority over single-model dispatch for matching intents/modalities.
    #[serde(default)]
    pub flows: Vec<FlowConfig>,
    /// Camera watch configuration (Sprint 057).
    /// When present, enables motion-triggered vision analysis via Wyze cameras.
    #[serde(default)]
    pub cameras: Option<CameraConfig>,
}

// ─── Camera Watch (Sprint 057) ──────────────────────────────────────

/// Configuration for motion-triggered camera vision analysis.
///
/// Wyze cameras detect motion → HA webhook → Odin fetches RTSP snapshot →
/// Gemma 4 E4B analyzes → HA notification if important.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraConfig {
    /// Base URL for wyze-bridge snapshot endpoint (e.g. "http://10.0.45.28:5000/snapshot").
    pub snapshot_base_url: String,
    /// List of cameras to monitor.
    pub cameras: Vec<CameraEntry>,
    /// HA notification entity (e.g. "notify.mobile_app_pixel_10_pro_fold").
    pub notify_entity: String,
    /// Minimum seconds between notifications per camera to prevent spam.
    #[serde(default = "default_camera_cooldown")]
    pub cooldown_secs: u64,
}

/// A single camera entry in the watch list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraEntry {
    /// Camera name as known to wyze-bridge (used in snapshot URL).
    pub name: String,
    /// Human-readable label for notifications (e.g. "Front Door").
    pub label: String,
}

fn default_camera_cooldown() -> u64 { 60 }

/// Configuration for Odin's autonomous background task worker.
///
/// Polls Mimir's task queue at a fixed interval, claims pending tasks, interprets
/// them via a lightweight LLM, and executes tool calls autonomously.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskWorkerConfig {
    /// Whether the task worker is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Poll interval in seconds (default 30).
    #[serde(default = "default_tw_poll_interval")]
    pub poll_interval_secs: u64,
    /// Agent name used when claiming tasks (default "fergus").
    #[serde(default = "default_tw_agent_name")]
    pub agent_name: String,
    /// Optional project scope filter for task pop.
    #[serde(default)]
    pub project: Option<String>,
    /// Model to use for task interpretation.
    /// Default: LFM2-24B-A2B (updated Sprint 054 — qwen3.5:4b was removed in Sprint 053).
    #[serde(default = "default_tw_model")]
    pub model: String,
}

fn default_tw_poll_interval() -> u64 { 30 }
fn default_tw_agent_name() -> String { "fergus".to_string() }
fn default_tw_model() -> String { "hf.co/LiquidAI/LFM2-24B-A2B-GGUF:Q4_K_M".to_string() }

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

/// Voice streaming configuration for WebSocket-based voice interaction.
///
/// Uses a single LFM2.5-Audio server that handles STT + LLM + TTS in one model.
/// `voice_api_url` points to the TTS-only endpoint on the same server.
/// `omni_url` points to the full audio-in/audio-out chat endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceStreamConfig {
    /// Whether voice streaming is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Base URL for the voice server's TTS endpoint (e.g. "http://localhost:9098").
    #[serde(default = "default_voice_api_url")]
    pub voice_api_url: String,
    /// Base URL for the LFM-Audio server's chat endpoint (e.g. "http://localhost:9098").
    /// Handles audio-in → text + audio-out in a single call.
    #[serde(default)]
    pub omni_url: Option<String>,
    /// Model to use for voice interactions. When set, voice requests bypass
    /// the semantic router's model selection and use this model instead.
    #[serde(default)]
    pub model: Option<String>,
    /// Tool names to load for voice requests. When set, only these tools
    /// are included in the agent loop context (reduces token overhead).
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

fn default_voice_api_url() -> String {
    "http://localhost:9098".to_string()
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
    /// Temperature for LLM calls during the agent loop (lower = more precise tool use).
    #[serde(default = "default_agent_temperature")]
    pub temperature: f64,
    /// Maximum characters of tool output before truncation.
    #[serde(default = "default_agent_tool_output_max_chars")]
    pub tool_output_max_chars: usize,
    /// Whether to enable thinking/reasoning mode for the LLM during agent loop.
    #[serde(default)]
    pub enable_thinking: bool,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: default_agent_max_iterations(),
            max_tool_calls_total: default_agent_max_tool_calls(),
            tool_timeout_secs: default_agent_tool_timeout(),
            total_timeout_secs: default_agent_total_timeout(),
            default_tiers: default_agent_tiers(),
            temperature: default_agent_temperature(),
            tool_output_max_chars: default_agent_tool_output_max_chars(),
            enable_thinking: false,
        }
    }
}

fn default_agent_max_iterations() -> usize { 10 }
fn default_agent_max_tool_calls() -> usize { 30 }
fn default_agent_tool_timeout() -> u64 { 30 }
fn default_agent_total_timeout() -> u64 { 300 }
fn default_agent_tiers() -> Vec<String> { vec!["safe".to_string()] }
fn default_agent_temperature() -> f64 { 0.3 }
fn default_agent_tool_output_max_chars() -> usize { 8000 }

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

/// Autonomous memory ingest configuration for Mimir (Sprint 044).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AutoIngestConfig {
    /// Whether the auto-ingest endpoint is enabled (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minimum cosine similarity against a template embedding to store an engram (default: 0.35).
    /// Uses dense 384-dim dot product (L2-normalized → cosine). Range: [-1.0, 1.0] where
    /// 0.0 = unrelated, 1.0 = identical. 0.35 is selective — random texts score near 0.0.
    #[serde(default = "default_template_threshold")]
    pub template_threshold: f64,
    /// Maximum content length in chars before truncation (default: 4096).
    #[serde(default = "default_max_content_length")]
    pub max_content_length: usize,
    /// Per-workstation cooldown in seconds — suppress duplicate bursts (default: 5).
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    /// Content-hash dedup window in seconds (default: 300).
    #[serde(default = "default_dedup_window_secs")]
    pub dedup_window_secs: u64,
    /// Saga async enrichment configuration. When present and enabled, auto-ingest
    /// spawns a background task to verify/correct classification and extract
    /// structured cause/effect via the local Saga LLM.
    #[serde(default)]
    pub saga: Option<SagaEnrichConfig>,
}

/// Configuration for Saga async enrichment of auto-ingested engrams.
///
/// Saga (LFM2.5-1.2B-Instruct or saga-350m specialist) runs via OpenAI-compatible
/// `/v1/chat/completions` endpoint (llama-server or Odin).
/// After the fast cosine gate stores an engram, a fire-and-forget task calls
/// Saga to verify should_store and distill structured cause/effect/tags.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SagaEnrichConfig {
    /// Whether Saga enrichment is enabled (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// LLM base URL for smart-ingest classification (default: Odin at "http://127.0.0.1:8080").
    #[serde(default = "default_saga_url", alias = "ollama_url")]
    pub llm_url: String,
    /// Model name for OpenAI-compatible `/v1/chat/completions` requests.
    #[serde(default = "default_saga_model")]
    pub model: String,
    /// Timeout per Saga inference call in seconds (default: 10).
    #[serde(default = "default_saga_timeout")]
    pub timeout_secs: u64,
}

fn default_saga_url() -> String { "http://127.0.0.1:11434".to_string() }
fn default_saga_model() -> String { "saga-350m".to_string() }
fn default_saga_timeout() -> u64 { 10 }

fn default_template_threshold() -> f64 { 0.35 }
fn default_max_content_length() -> usize { 4096 }
fn default_cooldown_secs() -> u64 { 5 }
fn default_dedup_window_secs() -> u64 { 300 }

/// Mimir memory service configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MimirConfig {
    pub listen_addr: String,
    pub database_url: String,
    pub qdrant_url: String,
    pub sdr: SdrConfig,
    pub tiers: TierConfig,
    /// Auto-ingest pipeline configuration. Uses defaults when absent.
    #[serde(default)]
    pub auto_ingest: Option<AutoIngestConfig>,
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

/// Database pool configuration for per-service connection tuning.
///
/// Default of 10 connections per service keeps total pool usage manageable
/// when multiple services (Odin, Mimir, Muninn, Huginn, MCP-remote) share
/// the same PostgreSQL instance. 5 services × 10 = 50, well within PG's
/// default `max_connections = 100`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
    #[serde(default = "default_db_max_connections")]
    pub max_connections: u32,
    #[serde(default = "default_db_acquire_timeout_secs")]
    pub acquire_timeout_secs: u64,
    #[serde(default = "default_db_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
}

impl DatabaseConfig {
    /// Convenience constructor from a bare URL using all defaults.
    pub fn from_url(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            max_connections: default_db_max_connections(),
            acquire_timeout_secs: default_db_acquire_timeout_secs(),
            idle_timeout_secs: default_db_idle_timeout_secs(),
        }
    }
}

fn default_db_max_connections() -> u32 {
    10
}
fn default_db_acquire_timeout_secs() -> u64 {
    10
}
fn default_db_idle_timeout_secs() -> u64 {
    600
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
    /// Model to use for HA automation YAML generation (Sprint 054).
    /// When absent, falls back to the coding-intent model via Odin routing.
    #[serde(default)]
    pub automation_model: Option<String>,
}

fn default_ha_timeout() -> u64 {
    10
}

/// Web search configuration (Brave Search API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchConfig {
    /// Brave Search API key (use `${BRAVE_SEARCH_API_KEY}` expansion).
    pub api_key: String,
    /// Maximum results per query (default: 5).
    #[serde(default = "default_web_search_max_results")]
    pub max_results: usize,
}

fn default_web_search_max_results() -> usize {
    5
}

/// Hybrid SDR + LLM router configuration (Sprint 052).
///
/// Combines a fast SDR-based "System 1" classifier with an LLM-based "System 2"
/// confirmation step. The SDR prototype scan runs in ~4μs (Hamming distance);
/// the LLM classification runs in <500ms on a lightweight Liquid AI model.
///
/// When both agree, confidence is high. When they disagree, the LLM wins and
/// the disagreement is logged as training data for nightly self-tuning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRouterConfig {
    /// Whether the hybrid router is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Ollama base URL for the router model (e.g. "http://localhost:11434").
    pub ollama_url: String,
    /// Model name in Ollama (e.g. "LFM2.5-1.2B-Instruct").
    pub model: String,
    /// Timeout in milliseconds for the LLM classification call (default: 500).
    #[serde(default = "default_llm_router_timeout_ms")]
    pub timeout_ms: u64,
    /// Minimum LLM confidence score (0.0–1.0) to accept the classification.
    /// Below this threshold, falls back to keyword router (default: 0.6).
    #[serde(default = "default_llm_router_min_confidence")]
    pub min_confidence: f64,
    /// Hamming similarity threshold for SDR prototype matching (default: 0.70).
    /// Lower than skill_cache's 0.85 because intent classification is broader.
    #[serde(default = "default_llm_router_sdr_threshold")]
    pub sdr_threshold: f64,
    /// Maximum concurrent LLM classification requests to Hugin (default: 4).
    #[serde(default = "default_llm_router_max_concurrent")]
    pub max_concurrent: usize,
    /// Number of queue worker tasks consuming classification requests (default: 2).
    #[serde(default = "default_llm_router_workers")]
    pub workers: usize,
    /// Maximum queued requests per priority before back-pressure (default: 16).
    #[serde(default = "default_llm_router_queue_size")]
    pub queue_size: usize,
    /// Path to persist SDR intent prototypes (default: "/var/lib/yggdrasil/odin-sdr-prototypes.json").
    #[serde(default = "default_llm_router_prototypes_path")]
    pub prototypes_path: String,
    /// Path for the JSONL request log (default: "/var/lib/yggdrasil/odin-request-log.jsonl").
    #[serde(default = "default_llm_router_request_log_path")]
    pub request_log_path: String,
}

fn default_llm_router_timeout_ms() -> u64 { 500 }
fn default_llm_router_min_confidence() -> f64 { 0.6 }
fn default_llm_router_sdr_threshold() -> f64 { 0.70 }
fn default_llm_router_max_concurrent() -> usize { 4 }
fn default_llm_router_workers() -> usize { 2 }
fn default_llm_router_queue_size() -> usize { 16 }
fn default_llm_router_prototypes_path() -> String {
    "/var/lib/yggdrasil/odin-sdr-prototypes.json".to_string()
}
fn default_llm_router_request_log_path() -> String {
    "/var/lib/yggdrasil/odin-request-log.jsonl".to_string()
}

// ─── Flow Engine (Sprint 055) ───────────────────────────────────────

/// A multi-model pipeline that routes a request through specialist models sequentially.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowConfig {
    /// Flow name, e.g. "code_review", "voice_assist".
    pub name: String,
    /// What activates this flow.
    pub trigger: FlowTrigger,
    /// Ordered pipeline steps.
    pub steps: Vec<FlowStep>,
    /// Total flow timeout in seconds (default: 120).
    #[serde(default = "default_flow_timeout")]
    pub timeout_secs: u64,
    /// Max chars per step output for defensive truncation (default: 8000).
    #[serde(default = "default_flow_max_output")]
    pub max_step_output_chars: usize,
    /// Optional loop configuration for convergence-based iteration.
    /// When present, the flow repeats a subset of steps until a convergence
    /// pattern is matched or max_iterations is reached.
    #[serde(default)]
    pub loop_config: Option<LoopConfig>,
}

/// Convergence-based loop configuration for iterative flows.
///
/// Example: code review flow loops generate→review→refine until
/// the reviewer outputs "LGTM" or max iterations are reached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopConfig {
    /// Max loop iterations before forced exit (default: 5).
    #[serde(default = "default_loop_max_iterations")]
    pub max_iterations: usize,
    /// Regex pattern that signals convergence (e.g. "LGTM|CONVERGED|PASS").
    /// Matched against the output of `check_step`.
    pub convergence_pattern: String,
    /// Name of the step whose output is checked for convergence.
    pub check_step: String,
    /// Name of the step where the loop restarts from.
    /// Steps before this are only run once (e.g. initial generation).
    pub restart_from_step: String,
    /// Key of the step output that feeds back as input on loop restart.
    pub feedback_key: String,
}

fn default_loop_max_iterations() -> usize { 5 }

/// What triggers a flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowTrigger {
    /// Triggered by routing intent match (e.g. "coding").
    Intent(String),
    /// Triggered by input modality (e.g. "voice", "vision").
    Modality(String),
    /// Only triggered by explicit API parameter.
    Manual,
    /// Background flow triggered on cron schedule (e.g. "0 */4 * * *" = every 4 hours).
    Cron { schedule: String },
    /// Background flow triggered after N seconds of no incoming requests.
    Idle { min_idle_secs: u64 },
}

/// A single step in a flow pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowStep {
    /// Step name, used as key for output references.
    pub name: String,
    /// Backend name (must match a BackendConfig.name).
    pub backend: String,
    /// Model name on the backend.
    pub model: String,
    /// Override system prompt for this step. None = no system prompt.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Where this step gets its input.
    pub input: FlowInput,
    /// Key to store this step's output (referenced by downstream steps).
    pub output_key: String,
    /// Max tokens for this step's generation.
    #[serde(default = "default_flow_step_max_tokens")]
    pub max_tokens: usize,
    /// Temperature for this step.
    #[serde(default = "default_flow_step_temperature")]
    pub temperature: f64,
    /// Optional list of tool names this step can use.
    /// When present, this step runs a mini agent loop (tool-calling cycle)
    /// instead of a single-turn chat completion.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// Override thinking mode (Some(false) to disable for Gemma 4 / Qwen 3.5).
    #[serde(default)]
    pub think: Option<bool>,
    /// Agent loop configuration for tool-enabled steps.
    /// When `tools` is `Some`, controls iteration limits, timeouts, and tiers.
    /// Falls back to sensible defaults when omitted.
    #[serde(default)]
    pub agent_config: Option<AgentLoopConfig>,
}

/// Input source for a flow step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowInput {
    /// The original user message text.
    UserMessage,
    /// Raw audio input (for voice flows).
    AudioInput,
    /// Image data (for vision flows).
    ImageInput,
    /// Output from a previous step, referenced by output_key.
    StepOutput { key: String },
    /// Format string with placeholders: {user_message}, {step_name.output}.
    Template { template: String },
    /// Concatenate outputs from multiple prior steps.
    Accumulated { keys: Vec<String>, separator: String },
}

fn default_flow_timeout() -> u64 { 120 }
fn default_flow_max_output() -> usize { 8000 }
fn default_flow_step_max_tokens() -> usize { 2048 }
fn default_flow_step_temperature() -> f64 { 0.3 }

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
    /// SSH-accessible base for rsync memory sync.
    /// Format: "user@host:/path/to/claude-config"
    /// Example: "user@munin:/opt/yggdrasil/claude-config"
    /// When set, the local server syncs and merges memory files at startup.
    #[serde(default)]
    pub remote_ssh: Option<String>,
    /// PostgreSQL database URL for session persistence (optional).
    /// When set, MCP remote server persists session metadata to PG.
    #[serde(default)]
    pub database_url: Option<String>,
    /// Path to SQL migrations directory (default: "migrations").
    #[serde(default = "default_migrations_path")]
    pub migrations_path: Option<String>,
    /// Unique workspace identifier for session isolation (e.g. "yggdrasil:window-1").
    /// When set, sessions are scoped to this workspace. Multiple IDE windows can
    /// use different workspace_ids to prevent context bleed.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// URL for the Antigravity IDE backend (optional).
    /// When set, enables IDE-specific integration features.
    #[serde(default)]
    pub antigravity_url: Option<String>,
    /// IDE type: "vscode", "antigravity", "cursor" etc.
    /// Used for IDE-specific behavior in context_bridge and event hooks.
    #[serde(default)]
    pub ide_type: Option<String>,
    /// Path to JSONL event file for VS Code extension integration.
    /// When set, tool executions emit events for the extension's status bar and dashboard.
    /// Example: "/tmp/ygg-hooks/memory-events.jsonl"
    #[serde(default)]
    pub events_file: Option<String>,
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
