use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
use clap::Parser;
use sd_notify::NotifyState;
use tower_http::cors::CorsLayer;

use mimir::{
    handlers::{
        auto_ingest, context_list, context_retrieve, context_store, embed_text,
        get_core_engrams_handler, get_engram_by_id, get_stats, graph_link, graph_neighbors,
        graph_traverse, graph_unlink, health, promote_engram, list_sprints, query_engrams,
        recall_engrams, sdr_operations, store_engram, task_cancel, task_complete, task_list,
        task_pop, task_push, timeline,
    },
    state::{AppState, load_sdr_rows},
    summarization::SummarizationService,
};

#[derive(Parser)]
#[command(name = "mimir", about = "Yggdrasil engram memory service")]
struct Cli {
    /// Path to JSON configuration file.
    #[arg(short, long, default_value = "configs/mimir/config.json")]
    config: String,

    /// Database URL override (also accepts MIMIR_DATABASE_URL env var).
    #[arg(long, env = "MIMIR_DATABASE_URL")]
    database_url: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Tracing + Prometheus setup ---
    let prometheus_handle = ygg_server::init::telemetry();

    let cli = Cli::parse();
    tracing::info!(config = %cli.config, "mimir starting");

    // --- Load configuration ---
    let mut config: ygg_domain::config::MimirConfig =
        ygg_config::load_json(std::path::Path::new(&cli.config)).map_err(|e| {
            anyhow::anyhow!("failed to load config '{}': {}", cli.config, e)
        })?;

    // CLI / env override for database_url.
    if let Some(db_url) = cli.database_url {
        tracing::info!("database_url overridden via CLI/env");
        config.database_url = db_url;
    }

    // --- Build application state (connects to all external services) ---
    let state = AppState::new(config)
        .await
        .map_err(|e| anyhow::anyhow!("failed to initialise app state: {e}"))?;

    let listen_addr = state.config.listen_addr.clone();
    let shared_state = Arc::new(state);

    // --- Background summarization service ---
    // Compresses aging Recall engrams into Archival summaries via Odin LLM calls.
    let summarization_rx = shared_state.shutdown_tx.subscribe();
    let summarization = SummarizationService::new(
        shared_state.store.clone(),
        shared_state.vectors.clone(),
        shared_state.embedder.clone(),
        shared_state.config.tiers.clone(),
        summarization_rx,
    );
    let _summarization_handle = summarization.start();

    // --- Build Axum router ---
    // CORS: permissive policy — no auth in MVP, private LAN only.
    let prom_handle_clone = prometheus_handle.clone();
    let router = Router::new()
        .route("/health", get(health))
        .route("/api/v1/store", post(store_engram))
        .route("/api/v1/sprints/list", post(list_sprints))
        .route("/api/v1/query", post(query_engrams))
        .route("/api/v1/stats", get(get_stats))
        .route("/api/v1/engrams/{id}", get(get_engram_by_id))
        .route("/api/v1/embed", post(embed_text))
        .route("/api/v1/promote", post(promote_engram))
        .route("/api/v1/core", get(get_core_engrams_handler))
        .route("/api/v1/recall", post(recall_engrams))
        .route("/api/v1/auto-ingest", post(auto_ingest))
        .route("/api/v1/sdr/operations", post(sdr_operations))
        .route("/api/v1/timeline", post(timeline))
        .route("/api/v1/context", post(context_store))
        .route("/api/v1/context", get(context_list))
        .route("/api/v1/context/{handle}", get(context_retrieve))
        // Task queue endpoints.
        .route("/api/v1/tasks/push", post(task_push))
        .route("/api/v1/tasks/pop", post(task_pop))
        .route("/api/v1/tasks/complete", post(task_complete))
        .route("/api/v1/tasks/cancel", post(task_cancel))
        .route("/api/v1/tasks/list", post(task_list))
        // Graph endpoints.
        .route("/api/v1/graph/link", post(graph_link))
        .route("/api/v1/graph/unlink", post(graph_unlink))
        .route("/api/v1/graph/neighbors", post(graph_neighbors))
        .route("/api/v1/graph/traverse", post(graph_traverse))
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
        .layer(middleware::from_fn(ygg_server::metrics::http_metrics("mimir")))
        .layer(CorsLayer::permissive())
        // Cap request body at 2MB to prevent abuse.
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        // Concurrency limit: max 64 in-flight requests to prevent resource exhaustion.
        .layer(tower::limit::ConcurrencyLimitLayer::new(64))
        .with_state(shared_state.clone());

    // --- Bind and serve ---
    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .map_err(|e| anyhow::anyhow!("failed to bind to {listen_addr}: {e}"))?;

    tracing::info!("mimir listening on {listen_addr}");

    // --- Background SDR health metrics ---
    // Refresh SDR distribution stats every 60s for Prometheus/Grafana.
    {
        let state = shared_state.clone();
        let mut shutdown_rx = state.shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let stats = state.sdr_index.stats();
                        mimir::metrics::record_sdr_health(
                            stats.count as f64,
                            stats.avg_popcount,
                            stats.concept_coverage as f64,
                            stats.similarity_p50,
                            stats.similarity_p90,
                        );
                    }
                    _ = shutdown_rx.changed() => break,
                }
            }
        });
    }

    // --- Background SDR index backfill ---
    // Spawn after the listener is bound so the server can accept requests immediately.
    // Queries arriving before backfill completes still work via Qdrant (System 2 path).
    {
        let state = shared_state.clone();
        tokio::spawn(async move {
            match load_sdr_rows(state.store.pool()).await {
                Ok(rows) => {
                    let row_count = rows.len();
                    state.sdr_index.load_from_rows(&rows);
                    tracing::info!(
                        rows = row_count,
                        "sdr index backfill complete"
                    );
                }
                Err(e) => {
                    tracing::warn!("sdr index backfill failed (non-fatal): {e}");
                }
            }
        });
    }

    // --- Background insight template loading (Sprint 044) ---
    // Query PG for engrams tagged "insight_template", embed each cause text,
    // binarize to SDR, and store in AppState.template_sdrs for fast in-memory
    // Hamming matching on the auto-ingest path. Non-fatal: if this fails,
    // auto-ingest will always return below_threshold until templates are seeded.
    {
        let state = shared_state.clone();
        tokio::spawn(async move {
            match load_template_sdrs(&state).await {
                Ok(count) => {
                    tracing::info!(templates = count, "insight template SDRs loaded");
                }
                Err(e) => {
                    tracing::warn!("insight template loading failed (non-fatal): {e}");
                }
            }
        });
    }

    // --- systemd ready notification ---
    ygg_server::init::sd_ready();

    // --- systemd watchdog ---
    // Send WATCHDOG=1 at half the WatchdogSec interval. Cancelled on shutdown.
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

    // Graceful shutdown on SIGTERM or SIGINT.
    axum::serve(listener, router)
        .with_graceful_shutdown(ygg_server::shutdown::signal())
        .await
        .map_err(|e| anyhow::anyhow!("server error: {e}"))?;

    // Signal the background summarization task and watchdog to stop.
    let _ = shared_state.shutdown_tx.send(true);
    let _ = wd_tx.send(true);

    tracing::info!("mimir shut down gracefully");
    Ok(())
}

/// Load insight templates from PostgreSQL into AppState (both SDR and dense embeddings).
///
/// Queries for engrams with tag "insight_template", embeds each cause text via ONNX,
/// and stores both:
///   - `(category_name, sdr)` pairs in `template_sdrs` (for legacy/dedup paths)
///   - `(category_name, Vec<f32>)` pairs in `template_embeddings` (for cosine classification)
///
/// The category name is extracted from the second tag (e.g. "bug_fix" from
/// tags ["insight_template", "bug_fix"]).
///
/// Called once at startup after the SDR backfill. Non-fatal: if no templates are seeded,
/// auto-ingest will return "below_threshold" for all requests until templates are added
/// via `deploy/workstation/seed-insight-templates.sh`.
async fn load_template_sdrs(state: &std::sync::Arc<AppState>) -> anyhow::Result<usize> {
    use sqlx::Row as _;

    let rows = sqlx::query(
        r#"
        SELECT cause, tags
        FROM yggdrasil.engrams
        WHERE 'insight_template' = ANY(tags)
        ORDER BY created_at ASC
        "#,
    )
    .fetch_all(state.store.pool())
    .await
    .map_err(|e| anyhow::anyhow!("failed to fetch insight templates: {e}"))?;

    let mut sdr_templates: Vec<(String, mimir::sdr::Sdr)> = Vec::with_capacity(rows.len());
    let mut dense_templates: Vec<(String, Vec<f32>)> = Vec::with_capacity(rows.len());

    for row in &rows {
        let cause: String = row.get("cause");
        let tags: Vec<String> = row.get("tags");

        // Extract category: second tag after "insight_template"
        let category = tags
            .iter()
            .find(|t| t.as_str() != "insight_template")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        // Embed cause text — produces L2-normalized 384-dim vector
        let embedder = state.embedder.clone();
        let cause_clone = cause.clone();
        let embedding: Vec<f32> =
            tokio::task::spawn_blocking(move || embedder.embed(&cause_clone))
                .await
                .map_err(|e| anyhow::anyhow!("embed task panicked: {e}"))??;

        let sdr = mimir::sdr::binarize(&embedding[..mimir::sdr::SDR_BITS]);
        dense_templates.push((category.clone(), embedding));
        sdr_templates.push((category, sdr));
    }

    let count = sdr_templates.len();

    // Write both template sets into their RwLocks.
    *state
        .template_sdrs
        .write()
        .map_err(|e| anyhow::anyhow!("template_sdrs lock poisoned: {e}"))? = sdr_templates;
    *state
        .template_embeddings
        .write()
        .map_err(|e| anyhow::anyhow!("template_embeddings lock poisoned: {e}"))? = dense_templates;

    Ok(count)
}

