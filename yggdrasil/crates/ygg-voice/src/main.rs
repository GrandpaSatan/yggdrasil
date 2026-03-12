//! Yggdrasil Voice — NPU-accelerated speech-to-text and text-to-speech.
//!
//! Runs on Munin (Intel Core Ultra 185H) using the AI Boost NPU for
//! Whisper STT and Kokoro-82M TTS inference via ONNX Runtime + OpenVINO EP.
//!
//! Two-tier pipeline:
//!   1. SDR fast path: mel fingerprint → Hamming match (~4μs)
//!   2. Full STT/TTS: Whisper → Odin → Kokoro (~1-3s on NPU)
//!
//! Usage:
//!   ygg-voice [--config <path>]

mod audio;
mod mel;
mod pipeline;
mod sdr_commands;
mod stt;
mod tts;

pub use pipeline::VoiceError;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

/// Voice node configuration.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct VoiceConfig {
    /// Odin URL for intent routing.
    pub odin_url: String,

    /// Path to Whisper ONNX model directory.
    pub whisper_model_dir: String,

    /// Path to Kokoro ONNX model file.
    pub kokoro_model_path: String,

    /// Path to Kokoro voice styles binary.
    pub kokoro_voices_path: String,

    /// Kokoro voice name (e.g., "af_heart").
    #[serde(default = "default_kokoro_voice")]
    pub kokoro_voice: String,

    /// Kokoro speech speed multiplier.
    #[serde(default = "default_kokoro_speed")]
    pub kokoro_speed: f32,

    /// Audio input device name (ALSA device string).
    #[serde(default = "default_audio_device")]
    pub audio_device: String,

    /// Audio sample rate (default 16000 for Whisper).
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,

    /// Wake word to trigger voice processing (default: "yggdrasil").
    #[serde(default = "default_wake_word")]
    pub wake_word: String,

    /// Preferred inference device (NPU, GPU, CPU).
    #[serde(default = "default_inference_device")]
    pub inference_device: String,

    /// Fallback inference device if preferred is unavailable.
    #[serde(default = "default_fallback_device")]
    pub fallback_device: String,

    /// Path to a "busy/processing" WAV sound file.
    #[serde(default)]
    pub busy_sound_path: Option<String>,

    /// SDR fast-path command configuration.
    #[serde(default)]
    pub sdr_commands: sdr_commands::SdrCommandsConfig,

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

fn default_wake_word() -> String {
    "yggdrasil".to_string()
}

fn default_inference_device() -> String {
    "NPU".to_string()
}

fn default_fallback_device() -> String {
    "CPU".to_string()
}

fn default_kokoro_voice() -> String {
    "af_heart".to_string()
}

fn default_kokoro_speed() -> f32 {
    1.0
}

#[derive(Debug, Parser)]
#[command(
    name = "ygg-voice",
    version,
    about = "Yggdrasil voice control — NPU-accelerated STT/TTS"
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

    info!(
        odin_url = %config.odin_url,
        wake_word = %config.wake_word,
        inference_device = %config.inference_device,
        whisper_model = %config.whisper_model_dir,
        kokoro_model = %config.kokoro_model_path,
        "ygg-voice starting"
    );

    // Initialize mel spectrogram engine (shared across STT + SDR)
    let mel_arc = Arc::new(mel::MelSpectrogram::new());
    info!("mel spectrogram engine initialized");

    // Load Whisper STT model
    info!("loading Whisper STT model...");
    let stt_engine = stt::WhisperStt::load(
        std::path::Path::new(&config.whisper_model_dir),
        Arc::clone(&mel_arc),
        &config.inference_device,
        &config.fallback_device,
    )
    .context("failed to load Whisper STT model")?;
    info!("Whisper STT model loaded");

    // Load Kokoro TTS model
    info!("loading Kokoro TTS model...");
    let tts_engine = tts::KokoroTts::load(
        std::path::Path::new(&config.kokoro_model_path),
        std::path::Path::new(&config.kokoro_voices_path),
        &config.kokoro_voice,
        config.kokoro_speed,
        &config.inference_device,
        &config.fallback_device,
    )
    .context("failed to load Kokoro TTS model")?;
    info!("Kokoro TTS model loaded");

    // Load SDR command registry
    let mel_for_sdr = mel::MelSpectrogram::new();
    let sdr_registry =
        sdr_commands::SdrCommandRegistry::load_from_config(&config.sdr_commands, &mel_for_sdr)
            .context("failed to load SDR command registry")?;

    // Initialize audio capture
    info!("initializing audio capture...");
    let capture = audio::AudioCapture::new(&config.audio_device, config.sample_rate, 30)
        .context("failed to initialize audio capture")?;

    // Initialize audio player
    let player = audio::AudioPlayer::new(tts_engine.sample_rate())
        .context("failed to initialize audio player")?;

    // Build pipeline
    let busy_sound = config.busy_sound_path.map(PathBuf::from);
    let voice_pipeline = pipeline::VoicePipeline::new(
        capture,
        player,
        stt_engine,
        tts_engine,
        sdr_registry,
        mel::MelSpectrogram::new(),
        config.odin_url.clone(),
        busy_sound,
        config.wake_word.clone(),
        config.sample_rate,
    );

    // Notify systemd we're ready
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
    info!("ygg-voice: pipeline initialized and ready");

    // Spawn watchdog task for systemd
    tokio::spawn(async {
        loop {
            let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    // Spawn minimal health HTTP server for sentinel monitoring (port 9095)
    let health_addr = config
        .listen_addr
        .as_deref()
        .unwrap_or("0.0.0.0:9095");
    let health_listener = tokio::net::TcpListener::bind(health_addr)
        .await
        .with_context(|| format!("failed to bind health server to {health_addr}"))?;
    info!(addr = %health_addr, "health endpoint ready");

    tokio::spawn(async move {
        use axum::{routing::get, Json, Router};
        let app = Router::new().route(
            "/health",
            get(|| async {
                Json(serde_json::json!({ "status": "ok", "service": "ygg-voice" }))
            }),
        );
        let _ = axum::serve(health_listener, app).await;
    });

    // Run pipeline in a spawned task so ctrl_c can still shut us down
    let pipeline_handle = tokio::spawn(async move {
        voice_pipeline.run().await;
    });

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    info!("ygg-voice shutting down");

    pipeline_handle.abort();

    Ok(())
}
