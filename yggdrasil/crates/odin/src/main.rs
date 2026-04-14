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
use tower_http::cors::CorsLayer;
use ygg_domain::config::OdinConfig;
use ygg_ha::HaClient;

use odin::{
    handlers,
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
    // ── Telemetry (tracing + Prometheus recorder) ─────────────────
    let prometheus_handle = ygg_server::init::telemetry();

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
    let omni_url = voice_cfg.and_then(|v| v.omni_url.clone());

    if let Some(ref url) = voice_api_url {
        tracing::info!(voice_api_url = %url, "voice streaming enabled");
    } else {
        tracing::info!("voice streaming disabled (no voice config or not enabled)");
    }
    if let Some(ref url) = omni_url {
        tracing::info!(omni_url = %url, "LFM-Audio voice endpoint configured");
    }

    // ── Hybrid SDR + LLM router (Sprint 052) ──────────────────────
    let sdr_router = Arc::new(odin::sdr_router::SdrRouter::with_defaults());
    let mut llm_router_client = None;
    let mut router_queue_handle = None;
    let mut request_log_writer = None;

    if let Some(ref lr_config) = config.llm_router {
        if lr_config.enabled {
            // SDR prototypes: load from disk or bootstrap later.
            let proto_path = std::path::Path::new(&lr_config.prototypes_path);
            if proto_path.exists() {
                match sdr_router.load_from_file(proto_path).await {
                    Ok(n) => tracing::info!(count = n, "loaded SDR intent prototypes from disk"),
                    Err(e) => tracing::warn!(error = %e, "failed to load SDR prototypes — will bootstrap"),
                }
            }

            // LLM router client.
            let client = odin::llm_router::LlmRouterClient::new(
                http_client.clone(),
                lr_config.ollama_url.clone(),
                lr_config.model.clone(),
                lr_config.timeout_ms,
                lr_config.min_confidence,
                lr_config.max_concurrent,
            );

            // Request queue + workers.
            let (queue, receivers) = odin::request_queue::RequestQueue::new(lr_config.queue_size);
            let depth = Arc::new([
                std::sync::atomic::AtomicUsize::new(0),
                std::sync::atomic::AtomicUsize::new(0),
                std::sync::atomic::AtomicUsize::new(0),
            ]);
            let _worker_handles = odin::request_queue::spawn_workers(
                lr_config.workers,
                receivers,
                client.clone(),
                depth,
            );

            // Request log writer.
            let log_path = std::path::Path::new(&lr_config.request_log_path);
            match odin::request_log::RequestLogWriter::open(log_path).await {
                Ok(writer) => {
                    request_log_writer = Some(writer);
                    tracing::info!(path = %log_path.display(), "request log opened");
                }
                Err(e) => tracing::warn!(error = %e, "failed to open request log — logging disabled"),
            }

            llm_router_client = Some(client);
            router_queue_handle = Some(queue);
            tracing::info!(
                model = %lr_config.model,
                url = %lr_config.ollama_url,
                "hybrid SDR + LLM router enabled"
            );
        }
    }

    // ── AppState ──────────────────────────────────────────────────
    let tool_registry = Arc::new(odin::tool_registry::build_registry());
    tracing::info!(tools = tool_registry.len(), "agent tool registry built");

    // Broadcast channel for voice alerts (Sentinel → all connected WebSocket voice clients).
    let (voice_alert_tx, _) = tokio::sync::broadcast::channel::<String>(16);

    let flow_engine = Arc::new(odin::flow::FlowEngine::new(
        http_client.clone(),
        Arc::new(backends.clone()),
    ));
    let activity_tracker = odin::flow_scheduler::ActivityTracker::new();
    tracing::info!(flows = config.flows.len(), "flow engine initialized");

    // Sprint 064 P2: keep-warm injector. Pre-loads configured Ollama models
    // so cold-start (GLM-4.7 ~30–60s, etc.) never bites the first user
    // request after a quiet period.
    if let Some(keep_warm_cfg) = config.keep_warm.clone() {
        let _keep_warm_handle = odin::keep_warm::spawn(keep_warm_cfg, http_client.clone());
    }

    // Hot-swappable flows (mutated by PUT /api/flows/:id at runtime).
    let flows_hot = Arc::new(std::sync::RwLock::new(Arc::new(config.flows.clone())));
    let config_path = std::path::PathBuf::from(&cli.config);

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
        omni_url,
        web_search_config: config.web_search.clone(),
        config,
        tool_registry,
        gaming_config,
        skill_cache: Arc::new(odin::skill_cache::SkillCache::new()),
        wake_word_registry: Arc::new(odin::wake_word::WakeWordRegistry::new(
            Some(std::path::PathBuf::from("/var/lib/yggdrasil/wake-words.json")),
        )),
        omni_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        voice_alert_tx,
        circuit_breakers: odin::state::CircuitBreakerRegistry::new(),
        sdr_router,
        llm_router: llm_router_client,
        router_queue: router_queue_handle,
        request_log: request_log_writer,
        flow_engine,
        activity_tracker: activity_tracker.clone(),
        camera_cooldown: Arc::new(odin::camera::CooldownTracker::new()),
        flows: flows_hot,
        config_path,
    };

    // ── Axum router ───────────────────────────────────────────────
    // Metrics route handler captures the PrometheusHandle by value.
    // OPTIMIZATION: PrometheusHandle::render() is lock-free and safe for
    // concurrent scrapes. The clone here is cheap (it's an Arc internally).
    let prom_handle_clone = prometheus_handle.clone();
    let app = Router::new()
        // OpenAI-compatible endpoints.
        .route("/v1/chat/completions", post(handlers::chat_handler))
        .route("/v1/agent/stream", post(handlers::agent_stream_handler))
        .route("/v1/models", get(handlers::models_handler))
        .route("/internal/activity", get(handlers::internal_activity))
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
        // Vault proxy endpoint (Mimir).
        .route("/api/v1/vault", post(handlers::proxy_vault))
        // Muninn transparent proxy endpoints (AST analysis).
        .route("/api/v1/symbols", post(handlers::proxy_symbols))
        .route("/api/v1/references", post(handlers::proxy_references))
        // Gaming VM orchestration endpoint.
        .route("/api/v1/gaming", post(handlers::gaming_handler))
        // Build check endpoint (local cargo commands).
        .route("/api/v1/build_check", post(handlers::build_check_handler))
        // Deploy endpoint (cargo build + rsync).
        .route("/api/v1/deploy", post(handlers::deploy_handler))
        // Web search endpoint (Brave Search API).
        .route("/api/v1/web_search", post(handlers::web_search_handler))
        // Notification and webhook endpoints (HA integration).
        .route("/api/v1/notify", post(handlers::notify_handler))
        .route("/api/v1/webhook", post(handlers::webhook_handler))
        // Sprint 064 P8 — daily E2E hit counter (cron wrapper pings this).
        .route("/api/v1/e2e/hit", post(handlers::e2e_hit_handler))
        .route("/api/v1/e2e/hit", get(handlers::e2e_hit_handler))
        // Voice WebSocket endpoint (STT/TTS streaming via ygg-voice).
        .route("/v1/voice", get(voice_ws::ws_voice_handler))
        .route("/voice", get(voice_ws::voice_page))
        // Voice alert injection (Sentinel pushes anomaly alerts to browser clients).
        .route("/api/v1/voice/alert", post(voice_ws::voice_alert_handler))
        // Wake word enrollment (multi-user SDR calibration).
        .route("/api/v1/voice/enroll", get(handlers::wake_word_list))
        .route("/api/v1/voice/enroll/{user_id}", post(handlers::wake_word_enroll).delete(handlers::wake_word_remove))
        // Mesh topology endpoints (consumed by network_topology MCP tool).
        .route("/api/v1/mesh/nodes", get(handlers::mesh_nodes_handler))
        .route("/api/v1/mesh/services", get(handlers::mesh_services_handler))
        // Sprint 052: Request feedback and log query endpoints.
        .route("/api/v1/request/feedback", post(handlers::request_feedback_handler))
        .route("/api/v1/request/log", get(handlers::request_log_query_handler))
        // Flow CRUD (Sprint 059) — consumed by the VS Code extension Settings → Flows editor.
        .route("/api/flows", get(handlers::flows_list_handler))
        .route("/api/flows/{id}", get(handlers::flow_get_handler).put(handlers::flow_save_handler))
        .route("/api/backends", get(handlers::backends_handler))
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
        .layer(middleware::from_fn(ygg_server::metrics::http_metrics("odin")))
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

    // ── Nightly SDR prototype self-tuning (Sprint 052) ─────────────
    if let Some(ref lr_config) = state.config.llm_router {
        if lr_config.enabled {
            let sdr = state.sdr_router.clone();
            let proto_path = std::path::PathBuf::from(&lr_config.prototypes_path);
            tokio::spawn(async move {
                use chrono::Timelike;
                loop {
                    // Sleep until 3AM local time.
                    let now = chrono::Local::now();
                    let next_3am = if now.hour() >= 3 {
                        (now + chrono::Duration::days(1)).date_naive().and_hms_opt(3, 0, 0).unwrap()
                    } else {
                        now.date_naive().and_hms_opt(3, 0, 0).unwrap()
                    };
                    let next_3am = next_3am.and_local_timezone(chrono::Local).unwrap();
                    let sleep_dur = (next_3am - now).to_std().unwrap_or(std::time::Duration::from_secs(3600));
                    tracing::info!(
                        sleep_secs = sleep_dur.as_secs(),
                        "nightly SDR self-tune scheduled"
                    );
                    tokio::time::sleep(sleep_dur).await;

                    // Save current prototypes to disk.
                    match sdr.save_to_file(&proto_path).await {
                        Ok(()) => {
                            let n = sdr.len().await;
                            tracing::info!(prototypes = n, "nightly SDR self-tune: prototypes saved");
                        }
                        Err(e) => tracing::warn!(error = %e, "nightly SDR self-tune: save failed"),
                    }
                }
            });
        }
    }

    // ── Omni keepalive — prevent GPU idle clock-down ─────────────
    if let Some(ref omni) = state.omni_url {
        let client = state.http_client.clone();
        let url = format!("{omni}/keepalive");
        tracing::info!(url = %url, "omni keepalive task started (60s interval)");
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                match client
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        tracing::debug!("omni keepalive OK");
                    }
                    Ok(resp) => {
                        tracing::warn!(status = %resp.status(), "omni keepalive non-200");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "omni keepalive failed");
                    }
                }
            }
        });
    }

    // ── systemd ready notification ────────────────────────────────
    ygg_server::init::sd_ready();

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
                        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
                    }
                    _ = wd_rx.changed() => break,
                }
            }
        });
    }

    // ── Serve with graceful shutdown ──────────────────────────────
    axum::serve(listener, app)
        .with_graceful_shutdown(ygg_server::shutdown::signal())
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

    // Save SDR intent prototypes to disk (Sprint 052).
    if let Some(ref lr_config) = state.config.llm_router {
        if lr_config.enabled {
            let proto_path = std::path::Path::new(&lr_config.prototypes_path);
            match state.sdr_router.save_to_file(proto_path).await {
                Ok(()) => tracing::info!("SDR intent prototypes saved on shutdown"),
                Err(e) => tracing::warn!(error = %e, "failed to save SDR prototypes on shutdown"),
            }
        }
    }

    let _ = shutdown_tx.send(true); // Signal task worker to stop.
    let _ = wd_tx.send(true);
    tracing::info!("odin shutdown complete");
    Ok(())
}

