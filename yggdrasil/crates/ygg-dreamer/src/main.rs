//! ygg-dreamer binary entrypoint.
//!
//! Spawns three long-lived tokio tasks:
//! 1. `health_server` — axum HTTP server on `listen_addr` for `/health` + `/metrics`.
//! 2. `activity_poller` — polls Odin `/internal/activity` every `poll_interval_secs`.
//! 3. `warmup_loop` — when idle_duration > min_idle_secs, fires each configured
//!    warmup prefix sequentially (spaced by `warmup_interval_secs`).
//!
//! Sprint 065 C·P5 — systemd integration via `deploy/systemd/yggdrasil-dreamer.service`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::{Json, Router, routing::get};
use clap::Parser;
use reqwest::Client;
use serde_json::json;

use ygg_dreamer::config::DreamerConfig;
use ygg_dreamer::{flow_runner, warmup};

#[derive(Parser, Debug)]
#[command(name = "ygg-dreamer", about = "Yggdrasil dream-mode daemon")]
struct Cli {
    #[arg(long, default_value = "/opt/yggdrasil/config/dreamer.config.json")]
    config: PathBuf,
}

#[derive(Default)]
struct DreamerState {
    idle_secs: AtomicU64,
    warmup_fires: AtomicU64,
    dream_fires: AtomicU64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,ygg_dreamer=debug".into()),
        )
        .init();

    let cli = Cli::parse();
    let cfg = DreamerConfig::load(&cli.config)?;
    tracing::info!(
        config = %cli.config.display(),
        odin_url = %cfg.odin_url,
        min_idle_secs = cfg.min_idle_secs,
        warmup_prefixes = cfg.warmup_prefixes.len(),
        dream_flows = cfg.dream_flows.len(),
        "ygg-dreamer starting"
    );

    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;

    let state = Arc::new(DreamerState::default());

    // --- Health server ---
    let health_state = state.clone();
    let listen_addr = cfg.listen_addr.clone();
    let health_handle = tokio::spawn(async move {
        let app = Router::new()
            .route(
                "/health",
                get(move || {
                    let s = health_state.clone();
                    async move {
                        Json(json!({
                            "status": "ok",
                            "service": "ygg-dreamer",
                            "idle_secs": s.idle_secs.load(Ordering::Relaxed),
                            "warmup_fires": s.warmup_fires.load(Ordering::Relaxed),
                            "dream_fires": s.dream_fires.load(Ordering::Relaxed),
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind(&listen_addr).await
            .expect("bind health server");
        tracing::info!(addr = %listen_addr, "dreamer health server listening");
        axum::serve(listener, app).await.ok();
    });

    // --- Activity poller + warmup loop (single task) ---
    let odin_url = cfg.odin_url.clone();
    let mimir_url = cfg.mimir_url.clone();
    let poll_interval = Duration::from_secs(cfg.poll_interval_secs);
    let warmup_interval = Duration::from_secs(cfg.warmup_interval_secs);
    let min_idle = cfg.min_idle_secs;
    let warmup_prefixes = cfg.warmup_prefixes.clone();
    let dream_flows = cfg.dream_flows.clone();
    let sprint_tag = cfg.sprint_tag.clone();
    let dream_client = client.clone();
    let dream_state = state.clone();

    let dream_handle = tokio::spawn(async move {
        let mut last_warmup = tokio::time::Instant::now() - warmup_interval;

        loop {
            tokio::time::sleep(poll_interval).await;

            // Poll Odin activity.
            let idle = match dream_client
                .get(format!("{}/internal/activity", odin_url.trim_end_matches('/')))
                .timeout(Duration::from_secs(5))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<serde_json::Value>().await {
                        Ok(v) => v.get("idle_secs").and_then(|x| x.as_u64()).unwrap_or(0),
                        Err(e) => {
                            tracing::warn!(error = %e, "dreamer: activity body parse failed");
                            0
                        }
                    }
                }
                Ok(resp) => {
                    tracing::warn!(status = %resp.status(), "dreamer: activity non-success");
                    0
                }
                Err(e) => {
                    tracing::warn!(error = %e, "dreamer: activity fetch failed");
                    0
                }
            };
            dream_state.idle_secs.store(idle, Ordering::Relaxed);

            if idle < min_idle {
                continue;
            }

            // Warmup pass — gated by warmup_interval to avoid burning GPU.
            if last_warmup.elapsed() >= warmup_interval {
                for prefix in &warmup_prefixes {
                    match warmup::fire_one(&dream_client, prefix).await {
                        Ok(()) => {
                            tracing::info!(prefix = %prefix.name, "warmup fired");
                            dream_state.warmup_fires.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            tracing::warn!(prefix = %prefix.name, error = %e, "warmup failed");
                        }
                    }
                }
                last_warmup = tokio::time::Instant::now();
            }

            // Dream flow pass — one per idle window, rotate through configured flows.
            for flow in &dream_flows {
                match flow_runner::run_dream(
                    &dream_client,
                    &odin_url,
                    &mimir_url,
                    flow,
                    &sprint_tag,
                )
                .await
                {
                    Ok(text) => {
                        tracing::info!(
                            flow = %flow.name,
                            chars = text.len(),
                            "dream flow completed, engram stored"
                        );
                        dream_state.dream_fires.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::warn!(flow = %flow.name, error = %e, "dream flow failed");
                    }
                }
            }
        }
    });

    tokio::select! {
        _ = health_handle => tracing::warn!("dreamer: health task exited"),
        _ = dream_handle => tracing::warn!("dreamer: dream task exited"),
    }

    Ok(())
}
