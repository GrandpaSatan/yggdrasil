/// WebSocket voice handler for real-time STT/TTS streaming.
///
/// Accepts streamed PCM audio (s16le, 16 kHz, mono) from browser clients,
/// performs server-side VAD (energy-based), proxies STT/TTS to ygg-voice's
/// HTTP API, runs transcribed text through the full Odin chat pipeline via
/// `handlers::process_chat_text`, and streams TTS audio back to the client.
///
/// Protocol (JSON text frames):
///   Server -> Client:
///     {"type":"ready","session_id":"..."}   — connection established
///     {"type":"listening"}                  — speech detected
///     {"type":"processing"}                 — silence detected, STT in progress
///     {"type":"transcript","text":"..."}    — STT result
///     {"type":"response","text":"..."}      — LLM response
///     {"type":"audio_start","sample_rate":N} — TTS audio begins
///     {"type":"audio_end"}                  — TTS audio complete
///     {"type":"error","message":"..."}      — error
///
///   Client -> Server:
///     Binary frames: raw PCM s16le audio
///     {"type":"vad_end"}    — client-side VAD triggered end-of-speech
///     {"type":"config"}     — configuration (reserved, currently no-op)
use axum::{
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::{Html, IntoResponse, Response},
};
use uuid::Uuid;

use crate::error::OdinError;
use crate::handlers;
use crate::state::AppState;

/// WebSocket upgrade handler mounted at `GET /v1/voice`.
pub async fn ws_voice_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Result<Response, OdinError> {
    let voice_url = state.voice_api_url.clone().ok_or_else(|| {
        OdinError::BadRequest("voice is not enabled".to_string())
    })?;
    Ok(ws.on_upgrade(move |socket| handle_voice_session(socket, state, voice_url)))
}

// ─────────────────────────────────────────────────────────────────
// VAD state machine
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum VadState {
    /// Waiting for speech energy to exceed the threshold.
    Idle,
    /// Speech detected — accumulating audio.
    Listening,
    /// Silence timeout reached — STT/LLM/TTS pipeline running.
    Processing,
}

// ─────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────

const SAMPLE_RATE: u32 = 16_000;
/// Maximum buffer: 30 seconds of 16 kHz mono audio.
const MAX_SAMPLES: usize = 30 * SAMPLE_RATE as usize;
/// RMS energy threshold for voice activity detection.
const VAD_THRESHOLD: f32 = 0.01;
/// Number of consecutive below-threshold frames before end-of-speech.
/// Each binary WebSocket message counts as one frame for this purpose.
const SILENCE_TIMEOUT_FRAMES: u32 = 15;
/// Window size (in samples) for the RMS energy check — 0.5 seconds.
const VAD_WINDOW_SAMPLES: usize = SAMPLE_RATE as usize / 2;

// ─────────────────────────────────────────────────────────────────
// Per-connection session
// ─────────────────────────────────────────────────────────────────

/// Drive a single voice WebSocket connection through the VAD -> STT -> LLM -> TTS loop.
async fn handle_voice_session(mut socket: WebSocket, state: AppState, voice_api_url: String) {
    let session_id = Uuid::new_v4().to_string();
    let http = state.http_client.clone();

    // Send ready message.
    let ready = serde_json::json!({"type": "ready", "session_id": &session_id});
    if socket
        .send(Message::Text(ready.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Ring buffer for accumulated PCM samples.
    let mut audio_buffer: Vec<i16> = Vec::new();

    // VAD bookkeeping.
    let mut vad_state = VadState::Idle;
    let mut silence_frames: u32 = 0;
    let mut speech_start_sample: usize = 0;

    loop {
        match socket.recv().await {
            // ── Binary: raw PCM s16le audio ──────────────────────
            Some(Ok(Message::Binary(data))) => {
                // Decode little-endian i16 samples.
                let samples: Vec<i16> = data
                    .chunks_exact(2)
                    .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect();

                audio_buffer.extend_from_slice(&samples);

                // Cap buffer at MAX_SAMPLES — drop oldest excess.
                if audio_buffer.len() > MAX_SAMPLES {
                    let excess = audio_buffer.len() - MAX_SAMPLES;
                    audio_buffer.drain(..excess);
                }

                // RMS energy over the trailing window.
                let window_start = audio_buffer.len().saturating_sub(VAD_WINDOW_SAMPLES);
                let energy = rms_energy_i16(&audio_buffer[window_start..]);

                match vad_state {
                    VadState::Idle => {
                        if energy > VAD_THRESHOLD {
                            vad_state = VadState::Listening;
                            silence_frames = 0;
                            // Mark where speech begins (with a small lookback for onset)
                            speech_start_sample = audio_buffer.len().saturating_sub(VAD_WINDOW_SAMPLES);
                            let _ = socket
                                .send(Message::Text(
                                    serde_json::json!({"type": "listening"}).to_string().into(),
                                ))
                                .await;
                        }
                    }
                    VadState::Listening => {
                        if energy <= VAD_THRESHOLD {
                            silence_frames += 1;
                            if silence_frames >= SILENCE_TIMEOUT_FRAMES {
                                // Only send the speech portion, not pre-speech silence.
                                let speech_audio = &audio_buffer[speech_start_sample..];
                                process_utterance(
                                    &mut socket,
                                    &state,
                                    &http,
                                    &voice_api_url,
                                    &session_id,
                                    speech_audio,
                                )
                                .await;

                                // Reset for next utterance.
                                audio_buffer.clear();
                                vad_state = VadState::Idle;
                                silence_frames = 0;
                            }
                        } else {
                            silence_frames = 0;
                        }
                    }
                    VadState::Processing => {
                        // Should not arrive here in practice (single-threaded per connection).
                    }
                }
            }

            // ── Text: JSON control messages ─────────────────────
            Some(Ok(Message::Text(text))) => {
                if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&text) {
                    match msg.get("type").and_then(|t| t.as_str()) {
                        Some("vad_end") => {
                            // Client-side VAD signalled end-of-speech (mic stop or client VAD).
                            // Process if we have audio, regardless of server-side VAD state.
                            if !audio_buffer.is_empty() {
                                let speech_audio = &audio_buffer[speech_start_sample..];
                                process_utterance(
                                    &mut socket,
                                    &state,
                                    &http,
                                    &voice_api_url,
                                    &session_id,
                                    speech_audio,
                                )
                                .await;

                                audio_buffer.clear();
                                vad_state = VadState::Idle;
                                silence_frames = 0;
                            }
                        }
                        Some("config") => { /* reserved — no-op */ }
                        _ => {}
                    }
                }
            }

            // ── Close / disconnect ──────────────────────────────
            Some(Ok(Message::Close(_))) | None => break,
            _ => {}
        }
    }

    tracing::info!(session_id = %session_id, "voice WebSocket session closed");
}

// ─────────────────────────────────────────────────────────────────
// Pipeline: STT -> LLM -> TTS
// ─────────────────────────────────────────────────────────────────

/// Run the full STT -> chat -> TTS pipeline for a completed utterance and send
/// results back over the WebSocket.
async fn process_utterance(
    socket: &mut WebSocket,
    state: &AppState,
    http: &reqwest::Client,
    voice_api_url: &str,
    session_id: &str,
    audio_buffer: &[i16],
) {
    // Notify client that we are processing.
    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "processing"}).to_string().into(),
        ))
        .await;

    // Convert i16 samples to raw PCM bytes for the STT endpoint.
    let pcm_bytes: Vec<u8> = audio_buffer
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();

    // ── STT ──────────────────────────────────────────────────────
    let transcript = match call_stt(http, voice_api_url, &pcm_bytes).await {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => return, // Empty transcript — nothing to do.
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "error", "message": format!("STT failed: {e}")})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };

    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "transcript", "text": &transcript})
                .to_string()
                .into(),
        ))
        .await;

    // ── Chat pipeline ────────────────────────────────────────────
    let response_text = match handlers::process_chat_text(state, &transcript, session_id).await {
        Ok(text) => text,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "error", "message": format!("chat failed: {e}")})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };

    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "response", "text": &response_text})
                .to_string()
                .into(),
        ))
        .await;

    // ── TTS ──────────────────────────────────────────────────────
    match call_tts(http, voice_api_url, &response_text).await {
        Ok((audio_bytes, sample_rate)) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "audio_start", "sample_rate": sample_rate})
                        .to_string()
                        .into(),
                ))
                .await;

            // Stream audio in 8 KB chunks.
            for chunk in audio_bytes.chunks(8192) {
                if socket
                    .send(Message::Binary(chunk.to_vec().into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }

            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "audio_end"}).to_string().into(),
                ))
                .await;
        }
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "error", "message": format!("TTS failed: {e}")})
                        .to_string()
                        .into(),
                ))
                .await;
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// VAD helpers
// ─────────────────────────────────────────────────────────────────

/// Compute RMS energy of i16 PCM samples normalised to [-1.0, 1.0].
fn rms_energy_i16(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|&s| {
            let f = s as f64 / 32768.0;
            f * f
        })
        .sum();
    (sum / samples.len() as f64).sqrt() as f32
}

// ─────────────────────────────────────────────────────────────────
// ygg-voice HTTP client helpers
// ─────────────────────────────────────────────────────────────────

/// Call ygg-voice `POST /api/v1/stt` with raw PCM bytes.
///
/// Returns the transcribed text on success.
async fn call_stt(
    client: &reqwest::Client,
    base_url: &str,
    pcm_bytes: &[u8],
) -> Result<String, String> {
    let resp = client
        .post(format!("{base_url}/api/v1/stt"))
        .header("content-type", "application/octet-stream")
        .body(pcm_bytes.to_vec())
        .send()
        .await
        .map_err(|e| format!("STT request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("STT returned {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("STT parse failed: {e}"))?;

    Ok(body["text"].as_str().unwrap_or("").to_string())
}

/// Call ygg-voice `POST /api/v1/tts` with a JSON text payload.
///
/// Returns `(pcm_bytes, sample_rate)` on success.  The sample rate is read from
/// the `x-sample-rate` response header, defaulting to 24 000 Hz.
async fn call_tts(
    client: &reqwest::Client,
    base_url: &str,
    text: &str,
) -> Result<(Vec<u8>, u32), String> {
    let resp = client
        .post(format!("{base_url}/api/v1/tts"))
        .json(&serde_json::json!({"text": text}))
        .send()
        .await
        .map_err(|e| format!("TTS request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("TTS returned {}", resp.status()));
    }

    let sample_rate: u32 = resp
        .headers()
        .get("x-sample-rate")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(24_000);

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("TTS body failed: {e}"))?;

    Ok((bytes.to_vec(), sample_rate))
}

// ─────────────────────────────────────────────────────────────────
// GET /voice — browser client
// ─────────────────────────────────────────────────────────────────

/// Serve the embedded voice client HTML page.
pub async fn voice_page() -> impl IntoResponse {
    Html(include_str!("voice_ui.html"))
}
