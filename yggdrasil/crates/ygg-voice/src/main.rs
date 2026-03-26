//! Yggdrasil Voice — local audio bridge for the Fergus voice assistant.
//!
//! Captures audio from the local microphone, streams it to Odin's `/v1/voice`
//! WebSocket endpoint, and plays back TTS audio through the speakers. All speech
//! understanding, wake word detection, and conversation management happen in Odin.
//!
//! Usage:
//!   ygg-voice [--config <path>]

pub(crate) mod audio;
mod pipeline;

pub use pipeline::VoiceError;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

/// Voice node configuration.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct VoiceConfig {
    /// Odin URL (used to derive WebSocket URL if odin_ws_url not set).
    pub odin_url: String,

    /// Audio input device name (ALSA device string).
    #[serde(default = "default_audio_device")]
    pub audio_device: String,

    /// Audio sample rate (default 16000).
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,

    /// Odin voice WebSocket URL for local mic pipeline.
    /// If not set, derived from odin_url (http→ws + /v1/voice).
    #[serde(default)]
    pub odin_ws_url: Option<String>,

    /// Health endpoint listen address (default "0.0.0.0:9095").
    #[serde(default)]
    pub listen_addr: Option<String>,
}

fn default_audio_device() -> String {
    "default".to_string()
}

fn default_sample_rate() -> u32 {
    16000
}

#[derive(Debug, Parser)]
#[command(
    name = "ygg-voice",
    version,
    about = "Yggdrasil voice — local audio bridge for Fergus"
)]
struct Args {
    #[arg(
        short,
        long,
        default_value = "configs/voice/config.json",
        env = "YGG_VOICE_CONFIG"
    )]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    info!(config = %args.config.display(), "loading voice configuration");

    let config: VoiceConfig = ygg_config::load_json(&args.config)
        .with_context(|| format!("failed to load config: {}", args.config.display()))?;

    // Derive WebSocket URL from odin_url if not explicitly set.
    let odin_ws_url = config.odin_ws_url.clone().unwrap_or_else(|| {
        let base = config.odin_url.replace("http://", "ws://").replace("https://", "wss://");
        format!("{base}/v1/voice")
    });

    info!(
        odin_ws_url = %odin_ws_url,
        audio_device = %config.audio_device,
        sample_rate = config.sample_rate,
        "ygg-voice starting"
    );

    // Alert channel: external services (Sentinel) push alert text, pipeline speaks them.
    let (alert_tx, alert_rx) = tokio::sync::mpsc::channel::<String>(16);

    // Try to initialize local mic pipeline (optional — may fail on headless servers)
    let pipeline_handle = match audio::AudioCapture::new(&config.audio_device, config.sample_rate, 30) {
        Ok(capture) => {
            info!("audio capture initialized — local mic pipeline active");
            let voice_pipeline = pipeline::VoicePipeline::new(
                capture,
                odin_ws_url,
                config.sample_rate,
                alert_rx,
            );
            Some(tokio::spawn(async move {
                voice_pipeline.run().await;
            }))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "no audio device available — pipeline disabled"
            );
            drop(alert_rx);
            None
        }
    };

    // Notify systemd we're ready
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
    info!("ygg-voice: initialized and ready");

    // Spawn watchdog task for systemd
    tokio::spawn(async {
        loop {
            let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    // Spawn health endpoint
    let health_addr = config
        .listen_addr
        .as_deref()
        .unwrap_or("0.0.0.0:9095");
    let health_listener = tokio::net::TcpListener::bind(health_addr)
        .await
        .with_context(|| format!("failed to bind health server to {health_addr}"))?;
    info!(addr = %health_addr, "health endpoint ready");

    tokio::spawn(async move {
        use axum::{routing::{get, post}, Json, Router};
        let atx = alert_tx;
        let app = Router::new()
            .route(
                "/health",
                get(|| async {
                    Json(serde_json::json!({ "status": "ok", "service": "ygg-voice" }))
                }),
            )
            .route(
                "/api/v1/voice/alert",
                post(move |body: axum::extract::Json<serde_json::Value>| {
                    let tx = atx.clone();
                    async move {
                        if let Some(text) = body.get("text").and_then(|t| t.as_str()) {
                            let _ = tx.send(text.to_string()).await;
                        }
                        Json(serde_json::json!({"status": "ok"}))
                    }
                }),
            );
        let _ = axum::serve(health_listener, app).await;
    });

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    info!("ygg-voice shutting down");

    if let Some(handle) = pipeline_handle {
        handle.abort();
    }

    Ok(())
}
