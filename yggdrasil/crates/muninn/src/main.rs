use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
use clap::Parser;
use tower_http::cors::CorsLayer;
use ygg_store::{Store, qdrant::VectorStore};
use ygg_embed::OnnxEmbedder;

use muninn::{
    handlers::{
        find_references_handler, health_handler, search_handler, stats_handler,
        symbol_lookup_handler,
    },
    state::AppState,
};

#[derive(Parser)]
#[command(name = "muninn", about = "Yggdrasil retrieval engine")]
struct Cli {
    /// Path to JSON configuration file.
    #[arg(short, long, default_value = "configs/muninn/config.json")]
    config: String,

    /// Listen address override (also accepts MUNINN_LISTEN_ADDR env var).
    #[arg(long, env = "MUNINN_LISTEN_ADDR")]
    listen_addr: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Tracing + Prometheus ---
    let prometheus_handle = ygg_server::init::telemetry();

    let cli = Cli::parse();
    tracing::info!(config = %cli.config, "muninn starting");

    // --- Load configuration ---
    let config: ygg_domain::config::MuninnConfig =
        ygg_config::load_json(std::path::Path::new(&cli.config))
            .map_err(|e| anyhow::anyhow!("failed to load config '{}': {}", cli.config, e))?;

    // Determine listen address: CLI flag overrides config file.
    let listen_addr = cli
        .listen_addr
        .clone()
        .unwrap_or_else(|| config.listen_addr.clone());

    // --- Connect to PostgreSQL ---
    tracing::info!(url = %config.database_url, "connecting to postgresql");
    let store = Store::connect(&config.database_url)
        .await
        .map_err(|e| anyhow::anyhow!("postgresql connection failed: {e}"))?;

    // Run pending migrations.
    tracing::info!("running migrations from ./migrations");
    store
        .migrate("./migrations")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed: {e}"))?;
    tracing::info!("migrations applied");

    // --- Connect to Qdrant ---
    tracing::info!(url = %config.qdrant_url, "connecting to qdrant");
    let vectors = VectorStore::connect(&config.qdrant_url)
        .await
        .map_err(|e| anyhow::anyhow!("qdrant connection failed: {e}"))?;

    // Ensure the code_chunks collection exists (created by Huginn; this verifies it).
    vectors
        .ensure_collection("code_chunks")
        .await
        .map_err(|e| anyhow::anyhow!("qdrant collection setup failed: {e}"))?;
    tracing::info!("qdrant collection 'code_chunks' ready");

    // --- Load ONNX embedding model ---
    tracing::info!(model_dir = %config.embed.model_dir, "loading ONNX embedder");
    let embedder = OnnxEmbedder::load(std::path::Path::new(&config.embed.model_dir))
        .map_err(|e| anyhow::anyhow!("failed to load ONNX embedder: {e}"))?;
    tracing::info!("ONNX embedder ready");

    // --- Build shared application state ---
    let state = AppState {
        pool: store.pool().clone(),
        vectors,
        embedder,
        search_config: config.search,
    };

    // --- Build Axum router ---
    // CORS: permissive policy — no auth in MVP, private LAN only.
    let prom_handle_clone = prometheus_handle.clone();
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/api/v1/search", post(search_handler))
        .route("/api/v1/symbols", post(symbol_lookup_handler))
        .route("/api/v1/references", post(find_references_handler))
        .route("/api/v1/stats", get(stats_handler))
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
        .layer(middleware::from_fn(ygg_server::metrics::http_metrics("muninn")))
        .layer(CorsLayer::permissive())
        // Cap request body at 2MB to prevent abuse.
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        // Concurrency limit: max 64 in-flight requests to prevent resource exhaustion.
        .layer(tower::limit::ConcurrencyLimitLayer::new(64))
        .with_state(state);

    // --- Bind and serve ---
    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .map_err(|e| anyhow::anyhow!("failed to bind to {listen_addr}: {e}"))?;

    tracing::info!("muninn ready on {listen_addr}");

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
                        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
                    }
                    _ = wd_rx.changed() => break,
                }
            }
        });
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(ygg_server::shutdown::signal())
        .await
        .map_err(|e| anyhow::anyhow!("server error: {e}"))?;

    let _ = wd_tx.send(true);
    tracing::info!("muninn shut down gracefully");
    Ok(())
}
