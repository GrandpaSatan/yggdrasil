/// Shared application state passed to all Axum handlers via `State<AppState>`.
///
/// `AppState` is a data holder; no business logic lives here.  Construction
/// happens entirely in `main.rs`.
///
/// All fields are either `Clone` directly or wrapped in `Arc` so the state
/// can be cheaply cloned when Axum distributes it to handler tasks.
use std::sync::Arc;

use ygg_domain::config::{BackendType, OdinConfig};
use ygg_ha::HaClient;

use crate::router::SemanticRouter;
use crate::session::SessionStore;

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
    /// Muninn service base URL, e.g. `http://REDACTED_HUGIN_IP:9091`.
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
}
