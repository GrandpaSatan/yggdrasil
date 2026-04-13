/// Shared application state passed to all Axum handlers via `State<AppState>`.
///
/// `AppState` is a data holder; no business logic lives here.  Construction
/// happens entirely in `main.rs`.
///
/// All fields are either `Clone` directly or wrapped in `Arc` so the state
/// can be cheaply cloned when Axum distributes it to handler tasks.
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};

use dashmap::DashMap;
use ygg_cloud::adapter::{ChatMessage as CloudChatMessage, ChatRequest as CloudChatRequest, CloudAdapter};
use ygg_domain::config::{BackendType, FlowConfig, OdinConfig};
use ygg_ha::HaClient;

use crate::llm_router::LlmRouterClient;
use crate::request_log::RequestLogWriter;
use crate::request_queue::RequestQueue;
use crate::router::SemanticRouter;
use crate::sdr_router::SdrRouter;
use crate::session::SessionStore;
use crate::tool_registry::ToolSpec;

// ─────────────────────────────────────────────────────────────────
// Circuit breaker for tool endpoints
// ─────────────────────────────────────────────────────────────────

/// Per-endpoint circuit breaker state.
///
/// Tracks consecutive failures and trips open after a threshold, returning
/// instant errors to avoid burning the agent loop's time budget on dead services.
///
/// States: Closed (healthy) → Open (tripped) → HalfOpen (probing).
pub struct CircuitBreaker {
    /// Consecutive failure count.
    failures: AtomicU32,
    /// Whether the circuit is currently open (tripped).
    open: AtomicBool,
    /// Timestamp (seconds since UNIX epoch) when the circuit was tripped.
    tripped_at: std::sync::atomic::AtomicU64,
}

impl CircuitBreaker {
    /// Consecutive failures before tripping open.
    const FAILURE_THRESHOLD: u32 = 3;
    /// Seconds to wait in open state before allowing a probe request.
    const COOLDOWN_SECS: u64 = 30;

    pub fn new() -> Self {
        Self {
            failures: AtomicU32::new(0),
            open: AtomicBool::new(false),
            tripped_at: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Check if a request should be allowed through.
    /// Returns `true` if the circuit is closed or half-open (probe allowed).
    pub fn allow_request(&self) -> bool {
        if !self.open.load(Ordering::Relaxed) {
            return true; // Closed — healthy
        }
        // Open — check if cooldown has elapsed (half-open probe).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let tripped = self.tripped_at.load(Ordering::Relaxed);
        now.saturating_sub(tripped) >= Self::COOLDOWN_SECS
    }

    /// Record a successful request. Closes the circuit.
    pub fn record_success(&self) {
        self.failures.store(0, Ordering::Relaxed);
        self.open.store(false, Ordering::Relaxed);
    }

    /// Record a failed request. Trips the circuit after threshold failures.
    pub fn record_failure(&self) {
        let count = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= Self::FAILURE_THRESHOLD && !self.open.load(Ordering::Relaxed) {
            self.open.store(true, Ordering::Relaxed);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            self.tripped_at.store(now, Ordering::Relaxed);
            tracing::warn!(
                failures = count,
                "circuit breaker tripped — endpoint will be short-circuited for {}s",
                Self::COOLDOWN_SECS,
            );
        }
    }

    /// Whether the circuit is currently open (tripped).
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }

    /// Manually set the tripped_at timestamp. Used by integration tests
    /// to simulate cooldown without real-time waits.
    pub fn set_tripped_at(&self, epoch_secs: u64) {
        self.tripped_at.store(epoch_secs, Ordering::Relaxed);
    }
}

/// Thread-safe map of endpoint base URLs to their circuit breaker state.
#[derive(Clone)]
pub struct CircuitBreakerRegistry {
    inner: Arc<DashMap<String, Arc<CircuitBreaker>>>,
}

impl CircuitBreakerRegistry {
    pub fn new() -> Self {
        Self { inner: Arc::new(DashMap::new()) }
    }

    /// Get or create a circuit breaker for the given endpoint URL.
    pub fn get(&self, endpoint: &str) -> Arc<CircuitBreaker> {
        self.inner
            .entry(endpoint.to_string())
            .or_insert_with(|| Arc::new(CircuitBreaker::new()))
            .clone()
    }
}

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
    /// Hot-swappable flow list. Source of truth for `find_by_intent` /
    /// `find_by_modality` reads; mutated by `PUT /api/flows/:id` without
    /// requiring a service restart. Read path is a brief RwLock read + Arc
    /// clone (refcount bump) — cheap enough for the per-request dispatch.
    pub flows: Arc<RwLock<Arc<Vec<FlowConfig>>>>,
    /// Absolute path to the config JSON on disk. Flow CRUD persists mutations
    /// back here via atomic tempfile-rename so changes survive restarts.
    pub config_path: PathBuf,
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
    /// Voice server URL for TTS endpoint (e.g. "http://localhost:9098").
    /// `None` when voice streaming is disabled.
    pub voice_api_url: Option<String>,
    /// LFM-Audio server URL for full audio chat (e.g. "http://localhost:9098").
    /// Handles STT + LLM + TTS in a single call.
    pub omni_url: Option<String>,
    /// Static tool registry for the agent loop.  Built once at startup.
    pub tool_registry: Arc<Vec<ToolSpec>>,
    /// Optional gaming orchestration config.  Present when `GAMING_CONFIG_PATH` is set.
    pub gaming_config: Option<ygg_gaming::config::GamingConfig>,
    /// SDR-based skill cache for instant tool dispatch on repeat voice commands.
    pub skill_cache: Arc<crate::skill_cache::SkillCache>,
    /// SDR-based wake word detection with per-user enrollment.
    pub wake_word_registry: Arc<crate::wake_word::WakeWordRegistry>,
    /// True when MiniCPM-o is processing a voice request. Used to play "busy" presets.
    pub omni_busy: Arc<std::sync::atomic::AtomicBool>,
    /// Broadcast channel for pushing voice alerts to all connected WebSocket sessions.
    /// Sentinel (or any service) POSTs to `/api/v1/voice/alert`, Odin broadcasts to all
    /// active voice clients so they hear the alert via TTS.
    pub voice_alert_tx: tokio::sync::broadcast::Sender<String>,
    /// Optional web search config. Present when `config.web_search` is `Some`.
    pub web_search_config: Option<ygg_domain::config::WebSearchConfig>,
    /// Per-endpoint circuit breakers for tool dispatch resilience.
    pub circuit_breakers: CircuitBreakerRegistry,
    /// SDR-based "System 1" intent classifier (Sprint 052).
    pub sdr_router: Arc<SdrRouter>,
    /// LLM-based "System 2" intent confirmation via Liquid AI on Hugin (Sprint 052).
    /// `None` when the hybrid router is disabled or not configured.
    pub llm_router: Option<LlmRouterClient>,
    /// Priority request queue for LLM classification (Sprint 052).
    /// `None` when the hybrid router is disabled.
    pub router_queue: Option<RequestQueue>,
    /// Append-only JSONL request log (Sprint 052).
    /// `None` when request logging is disabled.
    pub request_log: Option<RequestLogWriter>,
    /// Multi-model flow execution engine (Sprint 055).
    /// Executes configurable pipelines where specialist models collaborate.
    pub flow_engine: Arc<crate::flow::FlowEngine>,
    /// Tracks last user activity for idle-triggered background flows.
    pub activity_tracker: crate::flow_scheduler::ActivityTracker,
    /// Per-camera notification cooldown tracker (Sprint 057).
    pub camera_cooldown: Arc<crate::camera::CooldownTracker>,
}

impl AppState {
    /// Find an alternative backend after a connection failure.
    ///
    /// Returns the first backend (other than `failed_backend`) that has
    /// available semaphore permits.  Prefers backends that list `model`
    /// in their model set, but will fall back to any reachable backend.
    pub fn find_fallback_backend(
        &self,
        failed_backend: &str,
        model: &str,
    ) -> Option<&BackendState> {
        // First pass: same model on a different backend.
        let with_model = self
            .backends
            .iter()
            .find(|b| {
                b.name != failed_backend
                    && b.models.iter().any(|m| m == model)
                    && b.semaphore.available_permits() > 0
            });
        if with_model.is_some() {
            return with_model;
        }

        // Second pass: any other backend with capacity.
        self.backends.iter().find(|b| {
            b.name != failed_backend && b.semaphore.available_permits() > 0
        })
    }
}
