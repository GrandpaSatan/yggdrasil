/// Odin process entry point.
///
/// Responsibilities:
///   1. Parse CLI flags (`--config`, `--listen-addr`).
///   2. Load `OdinConfig` from JSON.
///   3. Install Prometheus metrics recorder.
///   4. Build `AppState` (reqwest client, semantic router, backend semaphores).
///   5. Mount Axum router with all endpoints, CORS middleware, and metrics layer.
///   6. Bind TCP listener and serve with graceful shutdown.
///   7. Signal systemd ready (sd-notify) and spawn watchdog task.
///
/// Shutdown: SIGINT (Ctrl-C) or SIGTERM triggers graceful shutdown.  In-flight
/// requests are allowed to complete before the process exits.
use std::sync::Arc;

use anyhow::Context;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
use clap::Parser;
use metrics_exporter_prometheus::PrometheusBuilder;
use sd_notify::NotifyState;
use tower_http::cors::CorsLayer;
use tracing_subscriber::EnvFilter;
use ygg_domain::config::OdinConfig;
use ygg_ha::HaClient;

use odin::{
    handlers,
    metrics::metrics_middleware,
    router::SemanticRouter,
    session::{self, SessionStore},
    state::{AppState, BackendState, CloudPool},
    voice_ws,
};

// ─────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "odin", about = "Yggdrasil LLM orchestrator and semantic router")]
struct Cli {
    /// Path to the JSON configuration file.
    #[arg(short, long, default_value = "configs/odin/config.json")]
    config: String,

    /// Listen address override (e.g. "0.0.0.0:9000").
    /// Overrides the value in the config file.
    #[arg(long, env = "ODIN_LISTEN_ADDR")]
    listen_addr: Option<String>,
}

// ─────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── Logging ───────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // ── CLI ───────────────────────────────────────────────────────
    let cli = Cli::parse();
    tracing::info!(config = %cli.config, "loading configuration");

    // ── Config ────────────────────────────────────────────────────
    let config: OdinConfig =
        ygg_config::load_json(std::path::Path::new(&cli.config))
            .with_context(|| format!("failed to load config: {}", cli.config))?;

    let listen_addr = cli
        .listen_addr
        .unwrap_or_else(|| config.listen_addr.clone());

    tracing::info!(
        node = %config.node_name,
        addr = %listen_addr,
        backends = config.backends.len(),
        "configuration loaded"
    );

    // ── Prometheus metrics recorder ───────────────────────────────
    // Install the global recorder. The returned handle is moved into the
    // /metrics route closure so it can render the text exposition format on
    // each scrape request.
    let prometheus_handle = PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install prometheus recorder")?;

    // ── reqwest client ────────────────────────────────────────────
    // A single shared client for all outbound HTTP calls.
    // The 120-second timeout applies to the entire request/response cycle.
    // RAG calls use their own per-call timeouts (3s via tokio::time::timeout),
    // so the client-level timeout only gates very long Ollama inference runs.
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("failed to build reqwest client")?;

    // ── Semantic router ───────────────────────────────────────────
    let router = SemanticRouter::new(&config.routing, &config.backends);
    tracing::info!(rules = config.routing.rules.len(), "semantic router built");

    // ── Backend states ────────────────────────────────────────────
    let backends: Vec<BackendState> = config
        .backends
        .iter()
        .map(|b| {
            tracing::info!(
                name = %b.name,
                url = %b.url,
                backend_type = %format!("{:?}", b.backend_type),
                max_concurrent = b.max_concurrent,
                "registering backend"
            );
            BackendState {
                name: b.name.clone(),
                url: b.url.clone(),
                backend_type: b.backend_type.clone(),
                models: b.models.clone(),
                semaphore: Arc::new(tokio::sync::Semaphore::new(b.max_concurrent)),
                context_window: b.context_window,
            }
        })
        .collect();

    // ── HA client ─────────────────────────────────────────────────
    // Constructed only when `config.ha` is `Some`.  The token is consumed
    // from the config struct and never logged.
    let ha_client: Option<HaClient> = config.ha.as_ref().map(|ha_cfg| {
        tracing::info!(ha_url = %ha_cfg.url, "HA integration enabled");
        HaClient::from_config(ha_cfg)
    });

    if ha_client.is_none() {
        tracing::info!("HA integration disabled (no ha config)");
    }

    // ── Gaming config ─────────────────────────────────────────────
    let gaming_config = std::env::var("GAMING_CONFIG_PATH").ok().and_then(|path| {
        match ygg_gaming::config::load_config(std::path::Path::new(&path)) {
            Ok(cfg) => {
                tracing::info!(path = %path, "gaming orchestration enabled");
                Some(cfg)
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %path, "failed to load gaming config — gaming disabled");
                None
            }
        }
    });

    // ── Cloud fallback pool ────────────────────────────────────────
    let cloud_pool = config.cloud.as_ref().map(|cloud_cfg| {
        use ygg_cloud::adapter::CloudAdapter;
        let mut adapters: Vec<Box<dyn CloudAdapter>> = Vec::new();

        if let Some(ref openai) = cloud_cfg.openai {
            match ygg_cloud::providers::openai::OpenAiAdapter::new(
                openai.api_key.clone(),
                openai.requests_per_minute,
            ) {
                Ok(adapter) => {
                    tracing::info!("cloud fallback: OpenAI enabled");
                    adapters.push(Box::new(adapter));
                }
                Err(e) => tracing::warn!(error = %e, "failed to init OpenAI cloud adapter"),
            }
        }

        if let Some(ref claude) = cloud_cfg.claude {
            match ygg_cloud::providers::claude::ClaudeAdapter::new(
                claude.api_key.clone(),
                claude.requests_per_minute,
            ) {
                Ok(adapter) => {
                    tracing::info!("cloud fallback: Claude enabled");
                    adapters.push(Box::new(adapter));
                }
                Err(e) => tracing::warn!(error = %e, "failed to init Claude cloud adapter"),
            }
        }

        if let Some(ref gemini) = cloud_cfg.gemini {
            match ygg_cloud::providers::gemini::GeminiAdapter::new(
                gemini.api_key.clone(),
                gemini.requests_per_minute,
            ) {
                Ok(adapter) => {
                    tracing::info!("cloud fallback: Gemini enabled");
                    adapters.push(Box::new(adapter));
                }
                Err(e) => tracing::warn!(error = %e, "failed to init Gemini cloud adapter"),
            }
        }

        tracing::info!(
            providers = adapters.len(),
            fallback = cloud_cfg.fallback_enabled,
            "cloud pool initialized"
        );

        CloudPool {
            adapters: Arc::new(adapters),
            fallback_enabled: cloud_cfg.fallback_enabled,
        }
    });

    if cloud_pool.is_none() {
        tracing::info!("cloud fallback disabled (no cloud config)");
    }

    // ── Session store ──────────────────────────────────────────────
    let session_store = SessionStore::new(config.session.clone());

    // Try to restore sessions from previous run.
    let sessions_file = std::path::PathBuf::from("/var/lib/yggdrasil/odin-sessions.json");
    if sessions_file.exists() {
        match session_store.load_from_file(&sessions_file) {
            Ok(n) => tracing::info!(loaded = n, "restored sessions from disk"),
            Err(e) => tracing::warn!(error = %e, "failed to load sessions — starting fresh"),
        }
    }
    tracing::info!(
        max_sessions = config.session.max_sessions,
        ttl_secs = config.session.session_ttl_secs,
        context_budget = config.session.context_budget_tokens,
        "session store initialised"
    );

    // ── Voice streaming ─────────────────────────────────────────────
    let voice_cfg = config.voice.as_ref().filter(|v| v.enabled);
    let voice_api_url = voice_cfg.map(|v| v.voice_api_url.clone());
    let stt_url = voice_cfg.and_then(|v| v.stt_url.clone());
    let omni_url = voice_cfg.and_then(|v| v.omni_url.clone());

    if let Some(ref url) = voice_api_url {
        tracing::info!(voice_api_url = %url, "voice streaming enabled");
    } else {
        tracing::info!("voice streaming disabled (no voice config or not enabled)");
    }
    if let Some(ref url) = stt_url {
        tracing::info!(stt_url = %url, "dedicated STT endpoint configured");
    }
    if let Some(ref url) = omni_url {
        tracing::info!(omni_url = %url, "MiniCPM-o omni endpoint configured");
    }

    // ── AppState ──────────────────────────────────────────────────
    let tool_registry = Arc::new(odin::tool_registry::build_registry());
    tracing::info!(tools = tool_registry.len(), "agent tool registry built");

    // Broadcast channel for voice alerts (Sentinel → all connected WebSocket voice clients).
    let (voice_alert_tx, _) = tokio::sync::broadcast::channel::<String>(16);

    let state = AppState {
        http_client,
        router,
        backends,
        mimir_url: config.mimir.url.clone(),
        muninn_url: config.muninn.url.clone(),
        ha_client,
        ha_context_cache: Arc::new(tokio::sync::RwLock::new(None)),
        session_store: session_store.clone(),
        cloud_pool,
        voice_api_url,
        stt_url,
        omni_url,
        config,
        tool_registry,
        gaming_config,
        skill_cache: Arc::new(odin::skill_cache::SkillCache::new()),
        voice_alert_tx,
    };

    // ── Axum router ───────────────────────────────────────────────
    // Metrics route handler captures the PrometheusHandle by value.
    // OPTIMIZATION: PrometheusHandle::render() is lock-free and safe for
    // concurrent scrapes. The clone here is cheap (it's an Arc internally).
    let prom_handle_clone = prometheus_handle.clone();
    let app = Router::new()
        // OpenAI-compatible endpoints.
        .route("/v1/chat/completions", post(handlers::chat_handler))
        .route("/v1/models", get(handlers::models_handler))
        // Mimir transparent proxy endpoints (Fergus client compatibility).
        .route("/api/v1/query", post(handlers::proxy_query))
        .route("/api/v1/store", post(handlers::proxy_store))
        .route("/api/v1/sdr/operations", post(handlers::proxy_sdr_operations))
        .route("/api/v1/timeline", post(handlers::proxy_timeline))
        .route("/api/v1/sprints/list", post(handlers::proxy_sprint_list))
        .route("/api/v1/context", post(handlers::proxy_context_store))
        .route("/api/v1/context", get(handlers::proxy_context_list))
        .route("/api/v1/context/{handle}", get(handlers::proxy_context_retrieve))
        // Embedding proxy (Mimir).
        .route("/api/v1/embed", post(handlers::proxy_embed))
        // Engram by ID proxy (Mimir).
        .route("/api/v1/engrams/{id}", get(handlers::proxy_engram_by_id))
        // Task queue proxy endpoints (Mimir).
        .route("/api/v1/tasks/push", post(handlers::proxy_task_push))
        .route("/api/v1/tasks/pop", post(handlers::proxy_task_pop))
        .route("/api/v1/tasks/complete", post(handlers::proxy_task_complete))
        .route("/api/v1/tasks/cancel", post(handlers::proxy_task_cancel))
        .route("/api/v1/tasks/list", post(handlers::proxy_task_list))
        // Graph proxy endpoints (Mimir).
        .route("/api/v1/graph/link", post(handlers::proxy_graph_link))
        .route("/api/v1/graph/unlink", post(handlers::proxy_graph_unlink))
        .route("/api/v1/graph/neighbors", post(handlers::proxy_graph_neighbors))
        .route("/api/v1/graph/traverse", post(handlers::proxy_graph_traverse))
        // Muninn transparent proxy endpoints (AST analysis).
        .route("/api/v1/symbols", post(handlers::proxy_symbols))
        .route("/api/v1/references", post(handlers::proxy_references))
        // Gaming VM orchestration endpoint.
        .route("/api/v1/gaming", post(handlers::gaming_handler))
        // Notification and webhook endpoints (HA integration).
        .route("/api/v1/notify", post(handlers::notify_handler))
        .route("/api/v1/webhook", post(ygg_ha::webhook::handle_webhook))
        // Voice WebSocket endpoint (STT/TTS streaming via ygg-voice).
        .route("/v1/voice", get(voice_ws::ws_voice_handler))
        .route("/voice", get(voice_ws::voice_page))
        // Voice alert injection (Sentinel pushes anomaly alerts to browser clients).
        .route("/api/v1/voice/alert", post(voice_ws::voice_alert_handler))
        // Odin health endpoint.
        .route("/health", get(handlers::health_handler))
        // Prometheus scrape endpoint.
        .route(
            "/metrics",
            get(move || {
                let h = prom_handle_clone.clone();
                async move {
                    (
                        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
                        h.render(),
                    )
                }
            }),
        )
        // Metrics middleware: records request count and duration for all routes.
        .layer(middleware::from_fn(metrics_middleware))
        // CORS middleware — permissive for private LAN deployment.
        .layer(CorsLayer::permissive())
        // Cap request body at 2MB to prevent abuse.
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        // Concurrency limit: max 64 in-flight requests to prevent resource exhaustion.
        .layer(tower::limit::ConcurrencyLimitLayer::new(64))
        .with_state(state.clone());

    // ── TCP listener ──────────────────────────────────────────────
    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .with_context(|| format!("failed to bind to {listen_addr}"))?;

    tracing::info!(addr = %listen_addr, "odin ready");

    // ── Session reaper ────────────────────────────────────────────
    let _reaper_handle = session::spawn_reaper(session_store.clone());

    // ── Task worker (autonomous background task execution) ──────
    let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);

    if let Some(ref tw_config) = state.config.task_worker {
        if tw_config.enabled {
            let worker = odin::task_worker::TaskWorker::new(
                state.clone(),
                tw_config.clone(),
                shutdown_tx.subscribe(),
            );
            tokio::spawn(worker.run());
        } else {
            tracing::info!("task worker disabled in config");
        }
    } else {
        tracing::info!("task worker not configured");
    }

    // ── systemd ready notification ────────────────────────────────
    // Signal systemd that the service is ready. This unblocks any unit that
    // lists odin in its `After=` or `Requires=` clauses.
    // No-ops gracefully when NOTIFY_SOCKET is not set (non-systemd envs).
    let _ = sd_notify::notify(false, &[NotifyState::Ready]);

    // ── systemd watchdog ──────────────────────────────────────────
    // If WatchdogSec is configured, send WATCHDOG=1 at half the interval.
    // The task is cancelled via a watch channel when the server shuts down.
    let (wd_tx, mut wd_rx) = tokio::sync::watch::channel(false);
    let mut watchdog_usec = 0u64;
    if sd_notify::watchdog_enabled(false, &mut watchdog_usec) {
        let half = std::time::Duration::from_micros(watchdog_usec / 2);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(half);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        let _ = sd_notify::notify(false, &[NotifyState::Watchdog]);
                    }
                    _ = wd_rx.changed() => break,
                }
            }
        });
    }

    // ── Serve with graceful shutdown ──────────────────────────────
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    // Save sessions to disk before exiting.
    let sessions_file = std::path::PathBuf::from("/var/lib/yggdrasil/odin-sessions.json");
    if let Err(e) = std::fs::create_dir_all(sessions_file.parent().unwrap_or(std::path::Path::new("/"))) {
        tracing::warn!(error = %e, "failed to create sessions directory");
    }
    if let Err(e) = session_store.save_to_file(&sessions_file) {
        tracing::warn!(error = %e, "failed to save sessions on shutdown");
    }

    let _ = shutdown_tx.send(true); // Signal task worker to stop.
    let _ = wd_tx.send(true);
    tracing::info!("odin shutdown complete");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
// Graceful shutdown signal
// ─────────────────────────────────────────────────────────────────

/// Wait for SIGINT (Ctrl-C) or SIGTERM to trigger graceful shutdown.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => { sig.recv().await; }
            Err(e) => {
                tracing::error!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("received SIGINT — shutting down");
        }
        _ = terminate => {
            tracing::info!("received SIGTERM — shutting down");
        }
    }
}
