use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use metrics_exporter_prometheus::PrometheusBuilder;
use sd_notify::NotifyState;
use tracing::info;
use tracing_subscriber::EnvFilter;
use ygg_domain::config::HuginnConfig;

use huginn::health::{HealthState, start_health_server};
use huginn::indexer::Indexer;
use huginn::watcher::FileWatcher;

#[derive(Parser)]
#[command(name = "huginn", about = "Yggdrasil knowledge indexer")]
struct Cli {
    /// Path to JSON configuration file.
    #[arg(short, long, default_value = "configs/huginn/config.json")]
    config: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// One-shot index of configured paths. Exits on completion.
    Index {
        /// Re-index all files regardless of stored hash (skip change detection).
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Override the repo root (replaces watch_paths[0] from config).
        #[arg(long)]
        repo_root: Option<String>,
    },
    /// Continuously watch configured paths for changes and re-index incrementally.
    Watch,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cli = Cli::parse();
    info!(config = %cli.config, "huginn starting");

    // Load configuration from JSON.
    let mut config: HuginnConfig =
        ygg_config::load_json(std::path::Path::new(&cli.config))
            .with_context(|| format!("failed to load config: {}", cli.config))?;

    // --- Validate watch_paths ---
    for path in &config.watch_paths {
        let p = std::path::Path::new(path);
        anyhow::ensure!(
            p.is_absolute(),
            "watch_path '{}' must be an absolute path", path
        );
        anyhow::ensure!(
            p.exists(),
            "watch_path '{}' does not exist", path
        );
    }

    // --- Prometheus metrics recorder ---
    // Install the global recorder. The handle is passed into HealthState so
    // that the /metrics endpoint can render text exposition format.
    let prometheus_handle = PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install prometheus recorder")?;

    match cli.command {
        Command::Index { force, repo_root } => {
            // If --repo-root is provided, override watch_paths from config.
            if let Some(root) = repo_root {
                info!(repo_root = %root, "repo_root override — replacing watch_paths");
                config.watch_paths = vec![root];
            }

            let indexer = Indexer::new(config).await?;
            let stats = indexer.index_all(force).await?;

            info!(
                files_scanned = stats.files_scanned,
                files_indexed = stats.files_indexed,
                files_skipped = stats.files_skipped,
                chunks_created = stats.chunks_created,
                elapsed_s = stats.duration.as_secs_f64(),
                "index run complete"
            );
        }

        Command::Watch => {
            // --- Build shared health state ---
            let health_state = Arc::new(HealthState::new(prometheus_handle));

            // Seed the watch_count gauge before entering watch mode.
            health_state.set_watch_count(config.watch_paths.len() as u64);

            let listen_addr = config.listen_addr.clone();

            // --- Spawn health/metrics HTTP listener ---
            // The listener runs for the lifetime of the process alongside the
            // file watcher. It is not joined explicitly; the process exits when
            // the watcher task completes (or errors out).
            let health_state_clone = Arc::clone(&health_state);
            tokio::spawn(async move {
                if let Err(e) = start_health_server(listen_addr, health_state_clone).await {
                    tracing::error!(error = %e, "huginn health server exited with error");
                }
            });

            let indexer = Arc::new(Indexer::new(config.clone()).await?);

            // Run initial full index before entering watch mode.
            info!("running initial index before entering watch mode");
            let stats = indexer.index_all(false).await?;
            info!(
                files_indexed = stats.files_indexed,
                files_skipped = stats.files_skipped,
                chunks_created = stats.chunks_created,
                "initial index complete — entering watch mode"
            );

            // Update health state after initial index.
            // Fire-and-forget: record_file_indexed is async due to RwLock on
            // last_index_at; we approximate here with the aggregate stats.
            {
                let hs = Arc::clone(&health_state);
                tokio::spawn(async move {
                    // Seed indexed_files and code_chunks from initial index stats.
                    for _ in 0..stats.files_indexed {
                        // Use 0 chunks as approximation — actual per-file chunk
                        // counts are tracked incrementally by the watcher path.
                        hs.record_file_indexed(0).await;
                    }
                    // Correct the code_chunks counter with the actual batch total.
                    metrics::gauge!("ygg_code_chunks_total").set(stats.chunks_created as f64);
                });
            }

            // --- systemd ready notification ---
            // Signal after the initial index so that systemd considers the
            // service healthy only once it is actually ready to watch.
            let _ = sd_notify::notify(false, &[NotifyState::Ready]);

            // --- systemd watchdog ---
            // Huginn uses WatchdogSec=60 (send every 30s). Cancelled on shutdown.
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

            let watcher = FileWatcher::new(Arc::clone(&indexer), config.debounce_ms);
            watcher.run(&config.watch_paths).await?;

            let _ = wd_tx.send(true);
            info!("huginn watch mode exiting");
        }
    }

    Ok(())
}
