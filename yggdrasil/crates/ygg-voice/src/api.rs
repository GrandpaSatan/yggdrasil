// File: crates/ygg-voice/src/api.rs
//! HTTP API endpoints for remote STT and TTS access.
//!
//! Exposes WhisperStt and KokoroTts over HTTP so Odin can proxy voice
//! requests from remote clients without local NPU hardware.
//!
//! SDR acceleration:
//!   1. Silence rejection — mel SDR fingerprint compared against silence SDR,
//!      skips Whisper entirely if audio is noise/silence (~4μs).
//!   2. Transcript cache — after successful Whisper transcription, the audio's
//!      SDR is cached with its transcript. Future similar audio hits the cache
//!      instead of re-running Whisper (~4μs vs seconds).
//!
//! Endpoints:
//!   POST /api/v1/stt — PCM s16le 16kHz mono → JSON {"text": "..."}
//!   POST /api/v1/tts — JSON {"text": "..."} → PCM s16le 24kHz mono

use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router, extract::State};
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{error, info};

use crate::audio;
use crate::mel::MelSpectrogram;
use crate::stt::WhisperStt;
use crate::tts::KokoroTts;

/// SDR similarity threshold for silence detection.
const SILENCE_SDR_THRESHOLD: f64 = 0.75;
/// SDR similarity threshold for transcript cache hits.
const CACHE_SDR_THRESHOLD: f64 = 0.85;
/// RMS energy below this is definitely silence — skip SDR entirely.
const ENERGY_FLOOR: f32 = 0.005;
/// Maximum cached SDR→transcript entries.
const MAX_CACHE_ENTRIES: usize = 256;

/// A cached SDR→transcript mapping.
pub(crate) struct SdrCacheEntry {
    sdr: ygg_domain::sdr::Sdr,
    transcript: String,
}

/// Shared state for the API endpoints.
#[derive(Clone)]
pub(crate) struct ApiState {
    pub stt: Arc<WhisperStt>,
    pub tts: Arc<KokoroTts>,
    pub mel: Arc<MelSpectrogram>,
    /// Pre-computed SDR of silence (all-zeros audio) for rejection.
    pub silence_sdr: ygg_domain::sdr::Sdr,
    /// LRU-ish cache of audio SDR → transcript for repeat queries.
    pub sdr_cache: Arc<RwLock<Vec<SdrCacheEntry>>>,
}

/// TTS request body.
#[derive(Debug, Deserialize)]
struct TtsRequest {
    text: String,
}

/// POST /api/v1/stt
///
/// Accepts raw bytes (PCM s16le 16kHz mono) and returns transcribed text.
/// Uses SDR fast-path to skip Whisper for silence and repeat utterances.
async fn stt_handler(
    State(state): State<ApiState>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if body.len() < 2 {
        return Err((
            StatusCode::BAD_REQUEST,
            "request body too small for PCM audio".to_string(),
        ));
    }

    if body.len() % 2 != 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "body length must be even (s16le samples are 2 bytes each)".to_string(),
        ));
    }

    let samples_i16: Vec<i16> = body
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    // Convert i16 samples to f32 normalized to [-1.0, 1.0]
    let audio_f32: Vec<f32> = samples_i16
        .iter()
        .map(|&s| s as f32 / 32768.0)
        .collect();

    info!(samples = audio_f32.len(), "STT API: received audio");

    // Debug: save raw audio to /tmp for inspection
    if let Ok(mut f) = std::fs::File::create("/tmp/ygg-voice-debug.raw") {
        use std::io::Write;
        for &s in &samples_i16 {
            let _ = f.write_all(&s.to_le_bytes());
        }
        info!("debug audio saved to /tmp/ygg-voice-debug.raw");
    }

    // ── Gate 1: RMS energy floor ─────────────────────────────────
    let energy = audio::rms_energy(&audio_f32);
    if energy < ENERGY_FLOOR {
        info!(energy, "STT API: below energy floor — skipping (silence)");
        return Ok(Json(serde_json::json!({ "text": "" })));
    }

    // ── Gate 2: SDR silence rejection ────────────────────────────
    let audio_sdr = state.mel.fingerprint_sdr(&audio_f32);
    let silence_sim = ygg_domain::sdr::hamming_similarity(&audio_sdr, &state.silence_sdr);
    if silence_sim > SILENCE_SDR_THRESHOLD {
        info!(
            similarity = format!("{:.3}", silence_sim),
            "STT API: SDR matches silence — skipping"
        );
        return Ok(Json(serde_json::json!({ "text": "" })));
    }

    // ── Gate 3: SDR transcript cache ─────────────────────────────
    {
        let cache = state.sdr_cache.read().await;
        if let Some(entry) = cache.iter().find(|e| {
            ygg_domain::sdr::hamming_similarity(&audio_sdr, &e.sdr) > CACHE_SDR_THRESHOLD
        }) {
            info!(
                text = %entry.transcript,
                "STT API: SDR cache hit — skipping Whisper"
            );
            return Ok(Json(serde_json::json!({ "text": entry.transcript })));
        }
    }

    // ── Full Whisper STT ─────────────────────────────────────────
    let result = state.stt.transcribe_async(audio_f32).await.map_err(|e| {
        error!(error = %e, "STT API: transcription failed");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    info!(text = %result, "STT API: transcription complete");

    // ── Cache the result for future SDR matches ──────────────────
    if !result.is_empty() {
        let mut cache = state.sdr_cache.write().await;
        // Evict oldest if at capacity
        if cache.len() >= MAX_CACHE_ENTRIES {
            cache.remove(0);
        }
        cache.push(SdrCacheEntry {
            sdr: audio_sdr,
            transcript: result.clone(),
        });
    }

    Ok(Json(serde_json::json!({ "text": result })))
}

/// POST /api/v1/tts
///
/// Accepts JSON `{"text": "..."}` and returns raw PCM s16le 24kHz mono bytes.
async fn tts_handler(
    State(state): State<ApiState>,
    Json(req): Json<TtsRequest>,
) -> Result<(StatusCode, HeaderMap, Vec<u8>), (StatusCode, String)> {
    if req.text.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "text field must not be empty".to_string(),
        ));
    }

    info!(text = %req.text, "TTS API: synthesis request");

    let samples = state
        .tts
        .synthesize_async(req.text)
        .await
        .map_err(|e| {
            error!(error = %e, "TTS API: synthesis failed");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    // Convert f32 samples to s16le bytes
    let mut pcm_bytes = Vec::with_capacity(samples.len() * 2);
    for &sample in &samples {
        let clamped = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
        pcm_bytes.extend_from_slice(&clamped.to_le_bytes());
    }

    let sample_rate = state.tts.sample_rate();

    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/octet-stream".parse().unwrap());
    headers.insert("x-sample-rate", sample_rate.to_string().parse().unwrap());

    info!(
        samples = samples.len(),
        bytes = pcm_bytes.len(),
        sample_rate,
        "TTS API: synthesis complete"
    );

    Ok((StatusCode::OK, headers, pcm_bytes))
}

/// Build the API router with STT and TTS endpoints.
pub fn api_routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/stt", post(stt_handler))
        .route("/api/v1/tts", post(tts_handler))
}
