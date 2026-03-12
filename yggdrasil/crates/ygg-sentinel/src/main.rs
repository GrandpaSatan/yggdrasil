//! Yggdrasil Sentinel — distributed log monitoring with SDR anomaly detection.
//!
//! Collects logs from mesh nodes, encodes log windows to SDR fingerprints,
//! compares against rolling baseline, and triggers alerts when anomalies
//! are detected.
//!
//! Usage:
//!   ygg-sentinel [--config <path>]

mod collector;
mod detector;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use metrics_exporter_prometheus::PrometheusBuilder;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt};
use ygg_domain::config::HaConfig;
use ygg_ha::HaClient;

use crate::collector::LogCollector;
use crate::detector::AnomalyDetector;

/// Sentinel configuration (loaded from JSON).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SentinelConfig {
    /// Listen address for health/metrics endpoint.
    #[serde(default = "default_listen")]
    pub listen_addr: String,
    /// Odin URL for SDR encoding requests.
    pub odin_url: String,
    /// Nodes to monitor (name + SSH address).
    pub nodes: Vec<MonitoredNode>,
    /// SDR similarity threshold for anomaly detection (default 0.70).
    #[serde(default = "default_threshold")]
    pub anomaly_threshold: f64,
    /// Log window size in seconds (default 60).
    #[serde(default = "default_window")]
    pub window_secs: u64,
    /// Check interval in seconds (default 30).
    #[serde(default = "default_interval")]
    pub check_interval_secs: u64,
    /// HA config for sending notifications on anomaly.
    #[serde(default)]
    pub ha: Option<HaConfig>,
    /// HA notification target (e.g. "mobile_app_pixel_8").
    #[serde(default)]
    pub notify_target: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct MonitoredNode {
    pub name: String,
    /// Health check URL (e.g. "http://REDACTED_MUNIN_IP:8080/health").
    pub health_url: String,
    /// Services to monitor on this node.
    #[serde(default)]
    pub services: Vec<String>,
}

fn default_listen() -> String {
    "0.0.0.0:9094".to_string()
}
fn default_threshold() -> f64 {
    0.70
}
fn default_window() -> u64 {
    60
}
fn default_interval() -> u64 {
    30
}

#[derive(Debug, Parser)]
#[command(name = "ygg-sentinel", version, about = "Yggdrasil SDR log monitor")]
struct Args {
    #[arg(
        short,
        long,
        default_value = "configs/sentinel/config.json",
        env = "YGG_SENTINEL_CONFIG"
    )]
    config: std::path::PathBuf,
}

/// Ask Odin for a fix suggestion given anomaly text.
async fn get_fix_suggestion(
    client: &reqwest::Client,
    odin_url: &str,
    anomaly_text: &str,
) -> Option<String> {
    let payload = serde_json::json!({
        "model": "default",
        "messages": [
            {
                "role": "system",
                "content": "You are a systems administrator. Analyze this anomaly and suggest a fix in 1-2 sentences."
            },
            {
                "role": "user",
                "content": anomaly_text
            }
        ]
    });

    let resp = client
        .post(format!("{}/v1/chat/completions", odin_url))
        .json(&payload)
        .send()
        .await
        .ok()?;

    let body: serde_json::Value = resp.json().await.ok()?;
    body["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    info!(config = %args.config.display(), "loading sentinel configuration");

    let config: SentinelConfig = ygg_config::load_json(&args.config)
        .with_context(|| format!("failed to load config: {}", args.config.display()))?;

    // Prometheus metrics
    let prometheus_handle = PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install prometheus recorder")?;

    // HA client for notifications
    let ha_client = config.ha.as_ref().map(HaClient::from_config);
    let notify_target = config.notify_target.clone();

    let collector = Arc::new(LogCollector::new(config.nodes.clone()));
    let detector = Arc::new(AnomalyDetector::new(
        config.odin_url.clone(),
        config.anomaly_threshold,
    ));

    // HTTP client for Odin fix suggestions
    let odin_http = reqwest::Client::new();
    let odin_url = config.odin_url.clone();

    // Health/metrics endpoint
    let prom = prometheus_handle.clone();
    let health_addr = config.listen_addr.clone();
    tokio::spawn(async move {
        let app = axum::Router::new()
            .route(
                "/health",
                axum::routing::get(|| async {
                    axum::Json(serde_json::json!({"status": "ok", "service": "ygg-sentinel"}))
                }),
            )
            .route(
                "/metrics",
                axum::routing::get(move || {
                    let h = prom.clone();
                    async move {
                        (
                            [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
                            h.render(),
                        )
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind(&health_addr).await.unwrap();
        info!(addr = %health_addr, "sentinel health server listening");
        axum::serve(listener, app).await.unwrap();
    });

    info!(
        nodes = config.nodes.len(),
        threshold = config.anomaly_threshold,
        interval_secs = config.check_interval_secs,
        "sentinel starting monitoring loop"
    );

    // Main monitoring loop
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(config.check_interval_secs));

    loop {
        interval.tick().await;

        // Phase 1: Check health of all monitored nodes
        let results = collector.check_all().await;

        for result in &results {
            metrics::counter!("ygg_sentinel_checks_total", "node" => result.node.clone())
                .increment(1);

            if !result.healthy {
                metrics::counter!(
                    "ygg_sentinel_anomalies_total",
                    "node" => result.node.clone()
                )
                .increment(1);

                info!(
                    node = %result.node,
                    error = %result.error.as_deref().unwrap_or("unhealthy"),
                    "anomaly detected"
                );

                // Send HA notification if configured
                if let (Some(client), Some(target)) = (&ha_client, &notify_target) {
                    let msg = format!(
                        "Node {} is unhealthy: {}",
                        result.node,
                        result.error.as_deref().unwrap_or("check failed")
                    );
                    if let Err(e) = client.notify_simple(target, "Yggdrasil Alert", &msg).await {
                        warn!(error = %e, "failed to send HA notification");
                    }
                }
            }
        }

        // Phase 2: SDR anomaly detection on aggregated health window
        let log_text: String = results
            .iter()
            .map(|r| {
                format!(
                    "{} status={} response_ms={} error={}",
                    r.node,
                    if r.healthy { "ok" } else { "unhealthy" },
                    r.response_ms,
                    r.error.as_deref().unwrap_or("none"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        match detector.check_anomaly(&log_text).await {
            Ok(true) => {
                info!("SDR anomaly detected in health window");

                metrics::counter!("ygg_sentinel_sdr_anomalies_total").increment(1);

                // Ask Odin for a fix suggestion
                let fix_suggestion =
                    get_fix_suggestion(&odin_http, &odin_url, &log_text).await;

                // Send enriched HA notification with fix suggestion
                if let (Some(client), Some(target)) = (&ha_client, &notify_target) {
                    let mut msg = "SDR anomaly detected in cluster health window.".to_string();
                    if let Some(fix) = &fix_suggestion {
                        msg.push_str(&format!("\n\nSuggested fix: {}", fix));
                    }
                    if let Err(e) = client
                        .notify_simple(target, "Yggdrasil SDR Anomaly", &msg)
                        .await
                    {
                        warn!(error = %e, "failed to send SDR anomaly HA notification");
                    }
                }
            }
            Ok(false) => {
                // Normal — no anomaly detected
            }
            Err(e) => {
                warn!(error = %e, "SDR anomaly check failed");
            }
        }
    }
}
