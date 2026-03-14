use std::sync::Arc;

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

use mimir::{
    handlers::{
        context_list, context_retrieve, context_store, embed_text, get_core_engrams_handler, get_engram_by_id, get_stats,
        graph_link, graph_neighbors, graph_traverse, graph_unlink, health, promote_engram,
        list_sprints, query_engrams, recall_engrams, sdr_operations, store_engram,
        task_cancel, task_complete, task_list, task_pop, task_push, timeline,
    },
    metrics::metrics_middleware,
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
    // --- Tracing setup ---
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

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

    // --- Prometheus metrics recorder ---
    // Install the global recorder before building state so that any metrics
    // emitted during startup are captured.
    let prometheus_handle = PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| anyhow::anyhow!("failed to install prometheus recorder: {e}"))?;

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
        .layer(middleware::from_fn(metrics_middleware))
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

    // --- systemd ready notification ---
    // Signal systemd that the service is ready. No-ops when NOTIFY_SOCKET is
    // not set (non-systemd environments).
    let _ = sd_notify::notify(false, &[NotifyState::Ready]);

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
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| anyhow::anyhow!("server error: {e}"))?;

    // Signal the background summarization task and watchdog to stop.
    let _ = shared_state.shutdown_tx.send(true);
    let _ = wd_tx.send(true);

    tracing::info!("mimir shut down gracefully");
    Ok(())
}

/// Wait for SIGTERM or SIGINT and return so that `axum::serve` can finish in-flight requests.
async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install CTRL+C signal handler");
        }
    };

    #[cfg(unix)]
    let sigterm = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => { sig.recv().await; }
            Err(e) => {
                tracing::error!(error = %e, "failed to install SIGTERM signal handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { tracing::info!("received SIGINT, shutting down"); }
        _ = sigterm => { tracing::info!("received SIGTERM, shutting down"); }
    }
}
