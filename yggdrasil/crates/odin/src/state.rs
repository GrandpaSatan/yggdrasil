/// Shared application state passed to all Axum handlers via `State<AppState>`.
///
/// `AppState` is a data holder; no business logic lives here.  Construction
/// happens entirely in `main.rs`.
///
/// All fields are either `Clone` directly or wrapped in `Arc` so the state
/// can be cheaply cloned when Axum distributes it to handler tasks.
use std::sync::Arc;

use ygg_cloud::adapter::{ChatMessage as CloudChatMessage, ChatRequest as CloudChatRequest, CloudAdapter};
use ygg_domain::config::{BackendType, OdinConfig};
use ygg_ha::HaClient;

use crate::router::SemanticRouter;
use crate::session::SessionStore;
use crate::tool_registry::ToolSpec;

/// Per-backend runtime state: URL, declared model list, and concurrency gate.
#[derive(Clone)]
pub struct BackendState {
    pub name: String,
    pub url: String,
    pub backend_type: BackendType,
    /// Model names listed in the config for this backend.
    pub models: Vec<String>,
    /// Semaphore limiting the number of in-flight requests.
    ///
    /// Permits are acquired via `try_acquire()` which returns 503 immediately
    /// if no permit is available, rather than blocking the Axum task.
    pub semaphore: Arc<tokio::sync::Semaphore>,
    /// Total context window size in tokens for this backend.
    pub context_window: usize,
}

/// Pool of cloud provider adapters for fallback routing.
#[derive(Clone)]
pub struct CloudPool {
    pub adapters: Arc<Vec<Box<dyn CloudAdapter>>>,
    pub fallback_enabled: bool,
}

impl CloudPool {
    /// Try each cloud adapter in order until one succeeds.
    pub async fn fallback_chat(
        &self,
        messages: Vec<CloudChatMessage>,
        model_hint: Option<&str>,
    ) -> Option<String> {
        if !self.fallback_enabled {
            return None;
        }

        for adapter in self.adapters.iter() {
            let model = model_hint
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}-default", adapter.provider()));

            let req = CloudChatRequest {
                model,
                messages: messages.clone(),
                temperature: Some(0.7),
                max_tokens: Some(4096),
                stream: false,
            };

            match adapter.chat_completion(req).await {
                Ok(resp) => {
                    tracing::info!(
                        provider = %adapter.provider(),
                        model = %resp.model,
                        tokens = resp.usage.total_tokens,
                        "cloud fallback succeeded"
                    );
                    return Some(resp.content);
                }
                Err(e) => {
                    tracing::warn!(
                        provider = %adapter.provider(),
                        error = %e,
                        "cloud fallback failed — trying next provider"
                    );
                }
            }
        }

        None
    }
}

/// Shared state injected into every Axum handler.
#[derive(Clone)]
pub struct AppState {
    /// Single shared HTTP client (connection-pooled via hyper/reqwest).
    pub http_client: reqwest::Client,
    /// Keyword-based semantic router for intent classification.
    pub router: SemanticRouter,
    /// All configured Ollama backends with their semaphores.
    pub backends: Vec<BackendState>,
    /// Mimir service base URL, e.g. `http://localhost:9090`.
    pub mimir_url: String,
    /// Muninn service base URL, e.g. `http://<hugin-ip>:9091`.
    pub muninn_url: String,
    /// Full resolved configuration (kept for per-handler access to limits).
    pub config: OdinConfig,
    /// Optional Home Assistant client.  Present when `config.ha` is `Some`.
    pub ha_client: Option<HaClient>,
    /// 60-second cache for the HA domain summary used in the system prompt.
    ///
    /// Protected by a `RwLock` so multiple concurrent readers can share the
    /// cached value while a single writer refreshes it.
    ///
    /// Cache entry: `(instant_of_last_refresh, summary_string)`.
    pub ha_context_cache: Arc<tokio::sync::RwLock<Option<(tokio::time::Instant, String)>>>,
    /// In-memory conversation session store.
    pub session_store: SessionStore,
    /// Cloud provider pool for fallback routing when local backends are at capacity.
    pub cloud_pool: Option<CloudPool>,
    /// ygg-voice HTTP API URL for STT/TTS proxying (e.g. "http://localhost:9095").
    /// `None` when voice streaming is disabled.
    pub voice_api_url: Option<String>,
    /// Dedicated STT service URL (e.g. "http://localhost:9097" for Qwen3-ASR).
    /// When `None`, STT calls go to `voice_api_url`.
    pub stt_url: Option<String>,
    /// Static tool registry for the agent loop.  Built once at startup.
    pub tool_registry: Arc<Vec<ToolSpec>>,
    /// Optional gaming orchestration config.  Present when `GAMING_CONFIG_PATH` is set.
    pub gaming_config: Option<ygg_gaming::config::GamingConfig>,
}
