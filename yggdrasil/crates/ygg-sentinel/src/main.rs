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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

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
    /// ygg-voice URL for spoken alerts (e.g. "http://127.0.0.1:9095").
    /// When set, anomaly alerts are also spoken aloud via Fergus.
    #[serde(default)]
    pub voice_url: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct MonitoredNode {
    pub name: String,
    /// Health check URL (e.g. "http://<munin-ip>:8080/health").
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

/// Ask Odin to rephrase a raw alert in Fergus's persona for spoken delivery.
/// Falls back to the raw alert text if Odin is unavailable.
async fn format_voice_alert(
    client: &reqwest::Client,
    odin_url: &str,
    raw_alert: &str,
) -> String {
    let payload = serde_json::json!({
        "model": "default",
        "messages": [
            {
                "role": "system",
                "content": "You are Fergus, a household AI with dry British wit. \
                            Rephrase this system alert as a brief spoken notification \
                            (1-2 sentences). Address the user as 'sir'."
            },
            {
                "role": "user",
                "content": raw_alert
            }
        ]
    });

    let resp = match client
        .post(format!("{}/v1/chat/completions", odin_url))
        .json(&payload)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "failed to format voice alert via Odin — using raw text");
            return raw_alert.to_string();
        }
    };

    let body: serde_json::Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return raw_alert.to_string(),
    };

    body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or(raw_alert)
        .to_string()
}

/// POST an alert to ygg-voice (local speaker) and Odin (browser WebSocket clients).
async fn send_voice_alert(
    client: &reqwest::Client,
    voice_url: &str,
    odin_url: &str,
    text: &str,
) {
    let payload = serde_json::json!({"text": text});
    let timeout = std::time::Duration::from_secs(5);

    // Push to ygg-voice for local speaker playback.
    let voice_fut = client
        .post(format!("{voice_url}/api/v1/alert"))
        .json(&payload)
        .timeout(timeout)
        .send();

    // Push to Odin for browser WebSocket clients.
    let odin_fut = client
        .post(format!("{odin_url}/api/v1/voice/alert"))
        .json(&payload)
        .timeout(timeout)
        .send();

    // Fire both in parallel — neither blocks the other.
    let (voice_res, odin_res) = tokio::join!(voice_fut, odin_fut);

    match voice_res {
        Ok(r) if r.status().is_success() || r.status().as_u16() == 202 => {
            info!("voice alert queued (local speaker)");
        }
        Ok(r) => warn!(status = %r.status(), "ygg-voice alert returned unexpected status"),
        Err(e) => warn!(error = %e, "failed to send voice alert to ygg-voice"),
    }

    match odin_res {
        Ok(r) if r.status().is_success() || r.status().as_u16() == 202 => {
            info!("voice alert broadcast (browser clients)");
        }
        Ok(r) => warn!(status = %r.status(), "Odin voice alert returned unexpected status"),
        Err(e) => warn!(error = %e, "failed to send voice alert to Odin"),
    }
}

/// Voice alert cooldown — 60 seconds per node/key.
const VOICE_ALERT_COOLDOWN_SECS: u64 = 60;

/// Check if a voice alert is allowed (cooldown elapsed) and update the map.
fn should_voice_alert(cooldowns: &mut HashMap<String, Instant>, key: &str) -> bool {
    let now = Instant::now();
    if let Some(last) = cooldowns.get(key) {
        if now.duration_since(*last).as_secs() < VOICE_ALERT_COOLDOWN_SECS {
            return false;
        }
    }
    cooldowns.insert(key.to_string(), now);
    true
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

    // HTTP client for Odin fix suggestions + voice alerts
    let odin_http = reqwest::Client::new();
    let odin_url = config.odin_url.clone();
    let voice_url = config.voice_url.clone();

    // Per-node cooldown map for voice alerts (prevents repeating every 30s).
    let mut voice_cooldowns: HashMap<String, Instant> = HashMap::new();

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

        let listener = match tokio::net::TcpListener::bind(&health_addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(addr = %health_addr, error = %e, "sentinel health server failed to bind");
                return;
            }
        };
        info!(addr = %health_addr, "sentinel health server listening");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "sentinel health server exited with error");
        }
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

                // Send voice alert via Fergus (with 60s cooldown per node)
                if let Some(vurl) = &voice_url {
                    if should_voice_alert(&mut voice_cooldowns, &result.node) {
                        let raw = format!(
                            "Node {} is unhealthy: {}",
                            result.node,
                            result.error.as_deref().unwrap_or("check failed")
                        );
                        let spoken = format_voice_alert(&odin_http, &odin_url, &raw).await;
                        send_voice_alert(&odin_http, vurl, &odin_url, &spoken).await;
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

                // Send voice alert for SDR anomaly (cooldown key: "sdr_anomaly")
                if let Some(vurl) = &voice_url {
                    if should_voice_alert(&mut voice_cooldowns, "sdr_anomaly") {
                        let raw = match &fix_suggestion {
                            Some(fix) => format!(
                                "SDR anomaly detected in cluster health. Suggested fix: {fix}"
                            ),
                            None => "SDR anomaly detected in cluster health window.".to_string(),
                        };
                        let spoken = format_voice_alert(&odin_http, &odin_url, &raw).await;
                        send_voice_alert(&odin_http, vurl, &odin_url, &spoken).await;
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
