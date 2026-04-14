//! Sprint 064 P2 — Keep-Warm Injector.
//!
//! Periodically pings configured Ollama models with a no-op generation
//! (`num_predict: 1`) carrying a long `keep_alive` window. Eliminates
//! cold-start latency for any flow that touches the model — most importantly
//! `dream_exploration` / `dream_speculation` (GLM-4.7-flash cold-load = 30–60s
//! on Munin's eGPU).
//!
//! Design notes:
//! - All targets are pinged in parallel each tick (one `tokio::spawn` per
//!   target). Slow models do not block fast ones.
//! - First-tick fires immediately after spawn so cold models warm up at
//!   service start instead of waiting `interval_secs`.
//! - Failures are logged at WARN and counted, but the loop continues
//!   indefinitely. Outages do not crash Odin.
//! - The `keep_alive` window passed to Ollama should be slightly LONGER than
//!   `interval_secs` so the model never falls out of VRAM between pings.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use ygg_domain::config::{KeepWarmConfig, KeepWarmTarget};

/// Spawn the keep-warm loop. Returns the `JoinHandle` so the caller can
/// abort on shutdown if needed (typically the loop runs for the process
/// lifetime).
pub fn spawn(cfg: KeepWarmConfig, http_client: reqwest::Client) -> Option<JoinHandle<()>> {
    if !cfg.enabled {
        tracing::info!("keep_warm disabled in config");
        return None;
    }
    if cfg.targets.is_empty() {
        tracing::warn!("keep_warm enabled but no targets configured");
        return None;
    }

    let interval = Duration::from_secs(cfg.interval_secs);
    let target_count = cfg.targets.len();
    tracing::info!(
        targets = target_count,
        interval_secs = cfg.interval_secs,
        keep_alive = %cfg.keep_alive,
        "keep_warm loop starting"
    );

    Some(tokio::spawn(async move {
        // Fire once immediately at startup so models warm up before the first
        // user request, then settle into the configured interval.
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let success = Arc::new(AtomicU64::new(0));
        let failure = Arc::new(AtomicU64::new(0));

        loop {
            ticker.tick().await;
            for target in &cfg.targets {
                let target = target.clone();
                let client = http_client.clone();
                let keep_alive = cfg.keep_alive.clone();
                let timeout_ms = cfg.timeout_ms;
                let success = success.clone();
                let failure = failure.clone();
                tokio::spawn(async move {
                    match ping(&client, &target, &keep_alive, timeout_ms).await {
                        Ok(()) => {
                            success.fetch_add(1, Ordering::Relaxed);
                            tracing::debug!(
                                model = %target.model,
                                url = %target.url,
                                note = ?target.note,
                                "keep_warm ping ok"
                            );
                        }
                        Err(e) => {
                            failure.fetch_add(1, Ordering::Relaxed);
                            tracing::warn!(
                                model = %target.model,
                                url = %target.url,
                                note = ?target.note,
                                error = %e,
                                "keep_warm ping failed"
                            );
                        }
                    }
                });
            }
        }
    }))
}

/// Single keep-alive ping. `num_predict: 1` keeps generation cost negligible;
/// the real purpose is the `keep_alive` parameter, which tells Ollama to pin
/// the model in VRAM for the specified window.
async fn ping(
    client: &reqwest::Client,
    target: &KeepWarmTarget,
    keep_alive: &str,
    timeout_ms: u64,
) -> Result<(), String> {
    let url = format!("{}/api/generate", target.url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": target.model,
        "prompt": " ",
        "stream": false,
        "keep_alive": keep_alive,
        "options": { "num_predict": 1, "temperature": 0.0 }
    });

    let resp = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        client.post(&url).json(&body).send(),
    )
    .await
    .map_err(|_| format!("timed out after {timeout_ms}ms"))?
    .map_err(|e| format!("http error: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(url: &str, model: &str) -> KeepWarmTarget {
        KeepWarmTarget {
            url: url.to_string(),
            model: model.to_string(),
            note: None,
        }
    }

    #[test]
    fn spawn_returns_none_when_disabled() {
        let cfg = KeepWarmConfig {
            enabled: false,
            interval_secs: 60,
            keep_alive: "10m".to_string(),
            timeout_ms: 30000,
            targets: vec![target("http://localhost:11434", "x")],
        };
        let handle = spawn(cfg, reqwest::Client::new());
        assert!(handle.is_none());
    }

    #[test]
    fn spawn_returns_none_when_no_targets() {
        let cfg = KeepWarmConfig {
            enabled: true,
            interval_secs: 60,
            keep_alive: "10m".to_string(),
            timeout_ms: 30000,
            targets: vec![],
        };
        let handle = spawn(cfg, reqwest::Client::new());
        assert!(handle.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ping_propagates_http_errors() {
        // 127.0.0.1:1 is reserved and refuses connections — exercises the http error path.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let t = target("http://127.0.0.1:1", "any");
        let result = ping(&client, &t, "10m", 500).await;
        assert!(result.is_err(), "expected error, got {result:?}");
    }
}
