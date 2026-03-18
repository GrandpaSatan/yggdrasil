/// WebSocket voice handler for real-time voice interaction.
///
/// Accepts streamed PCM audio (s16le, 16 kHz, mono) from browser clients,
/// performs server-side VAD (energy-based), transcribes via Qwen3-ASR,
/// runs text through the Odin chat pipeline, and streams Kokoro TTS
/// audio back to the client.
///
/// Protocol (JSON text frames):
///   Server -> Client:
///     {"type":"ready","session_id":"..."}   — connection established
///     {"type":"resumed","session_id":"..."}  — session resumed from prior connection
///     {"type":"listening"}                  — speech detected
///     {"type":"processing"}                 — silence detected, pipeline running
///     {"type":"transcript","text":"..."}    — STT result
///     {"type":"response","text":"..."}      — LLM response
///     {"type":"audio_start","sample_rate":N} — TTS audio begins
///     {"type":"audio_end"}                  — TTS audio complete
///     {"type":"error","message":"..."}      — error
///
///   Client -> Server:
///     Binary frames: raw PCM s16le audio
///     {"type":"resume","session_id":"..."}  — resume prior session (must be first msg)
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
    // Use dedicated STT URL if configured, otherwise STT goes to the same voice_api_url.
    let stt_url = state.stt_url.clone().unwrap_or_else(|| voice_url.clone());
    let omni_url = state.omni_url.clone();
    Ok(ws.on_upgrade(move |socket| handle_voice_session(socket, state, voice_url, stt_url, omni_url)))
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
    /// Silence timeout reached — pipeline running.
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
async fn handle_voice_session(mut socket: WebSocket, state: AppState, voice_api_url: String, stt_url: String, omni_url: Option<String>) {
    let mut session_id = Uuid::new_v4().to_string();
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
    // Session resume: allow the first text message to swap session_id.
    let mut negotiated = false;

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
                                    &stt_url,
                                    &voice_api_url,
                                    &session_id,
                                    speech_audio,
                                    omni_url.as_deref(),
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
                        Some("resume") if !negotiated => {
                            if let Some(id) = msg.get("session_id").and_then(|v| v.as_str()) {
                                if state.session_store.get_session(id).is_some() {
                                    session_id = id.to_string();
                                    tracing::info!(session_id = %session_id, "voice session resumed");
                                    let _ = socket
                                        .send(Message::Text(
                                            serde_json::json!({"type": "resumed", "session_id": &session_id})
                                                .to_string()
                                                .into(),
                                        ))
                                        .await;
                                }
                            }
                        }
                        Some("vad_end") => {
                            // Client-side VAD signalled end-of-speech (mic stop or client VAD).
                            // Process if we have audio, regardless of server-side VAD state.
                            if !audio_buffer.is_empty() {
                                let speech_audio = &audio_buffer[speech_start_sample..];
                                process_utterance(
                                    &mut socket,
                                    &state,
                                    &http,
                                    &stt_url,
                                    &voice_api_url,
                                    &session_id,
                                    speech_audio,
                                    omni_url.as_deref(),
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
                    negotiated = true;
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
    stt_url: &str,
    tts_url: &str,
    session_id: &str,
    audio_buffer: &[i16],
    omni_url: Option<&str>,
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

    // ── Omni path: MiniCPM-o handles STT + LLM in one call ──────
    // Falls back to legacy STT → Ollama agent loop if omni is unavailable.
    let response_text = if let Some(omni) = omni_url {
        match call_omni_chat(http, omni, &pcm_bytes, session_id).await {
            Ok((transcript, response)) => {
                // Send transcript to client.
                let _ = socket
                    .send(Message::Text(
                        serde_json::json!({"type": "transcript", "text": &transcript})
                            .to_string()
                            .into(),
                    ))
                    .await;

                // Update session store with both messages.
                state.session_store.append_messages(
                    session_id,
                    &[
                        crate::session::CompactMessage::new("user", &transcript),
                        crate::session::CompactMessage::new("assistant", &response),
                    ],
                );

                response
            }
            Err(e) => {
                tracing::warn!(error = %e, "omni chat failed, falling back to legacy pipeline");
                // Fall through to legacy path below.
                match legacy_stt_chat(http, state, stt_url, socket, &pcm_bytes, session_id).await {
                    Some(text) => text,
                    None => return,
                }
            }
        }
    } else {
        // Legacy path: separate STT → agent loop.
        match legacy_stt_chat(http, state, stt_url, socket, &pcm_bytes, session_id).await {
            Some(text) => text,
            None => return,
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
    match call_tts(http, tts_url, &response_text).await {
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

/// Strip `<think>...</think>` blocks that Qwen models emit when thinking is enabled.
/// Also trims leading/trailing whitespace from the result.
fn strip_think_tags(text: &str) -> String {
    let mut result = text.to_string();
    while let Some(start) = result.find("<think>") {
        if let Some(end) = result.find("</think>") {
            result.replace_range(start..end + "</think>".len(), "");
        } else {
            // Unclosed <think> — strip from tag to end.
            result.truncate(start);
            break;
        }
    }
    result.trim().to_string()
}

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
// Omni + legacy pipeline helpers
// ─────────────────────────────────────────────────────────────────

/// Call MiniCPM-o omni server — sends raw PCM audio directly to the chat
/// endpoint. The model understands speech natively and responds as Fergus.
/// No separate transcription step needed.
///
/// Returns `(transcript_placeholder, response_text)` on success.
async fn call_omni_chat(
    client: &reqwest::Client,
    omni_url: &str,
    pcm_bytes: &[u8],
    _session_id: &str,
) -> Result<(String, String), String> {
    use base64::Engine;

    // Encode PCM as WAV in memory, then base64 for the chat endpoint.
    let wav_bytes = pcm_to_wav(pcm_bytes, 16000);
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&wav_bytes);

    let chat_url = format!("{omni_url}/api/v1/chat");
    let body = serde_json::json!({
        "audio_b64": audio_b64,
        "generate_audio": false,
    });

    let resp = client
        .post(&chat_url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("omni chat request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("omni chat returned {}", resp.status()));
    }

    let result: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("omni chat parse failed: {e}"))?;

    let response_text = result["text"]
        .as_str()
        .unwrap_or("")
        .to_string();

    tracing::info!(response = %response_text, "omni response");
    Ok(("[audio input]".to_string(), strip_think_tags(&response_text)))
}

/// Convert raw PCM s16le bytes to a minimal WAV file in memory.
fn pcm_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let file_len = 36 + data_len;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes());  // PCM format
    wav.extend_from_slice(&1u16.to_le_bytes());  // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes());  // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// Legacy STT → agent loop path. Sends STT result and transcript to the client,
/// then runs through `process_chat_text` (Ollama agent loop with tools).
/// Returns `Some(response_text)` on success, `None` if pipeline should abort.
async fn legacy_stt_chat(
    http: &reqwest::Client,
    state: &AppState,
    stt_url: &str,
    socket: &mut WebSocket,
    pcm_bytes: &[u8],
    session_id: &str,
) -> Option<String> {
    let transcript = match call_stt(http, stt_url, pcm_bytes).await {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => return None,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "error", "message": format!("STT failed: {e}")})
                        .to_string()
                        .into(),
                ))
                .await;
            return None;
        }
    };

    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "transcript", "text": &transcript})
                .to_string()
                .into(),
        ))
        .await;

    match handlers::process_chat_text(state, &transcript, session_id).await {
        Ok(text) => Some(strip_think_tags(&text)),
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "error", "message": format!("chat failed: {e}")})
                        .to_string()
                        .into(),
                ))
                .await;
            None
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// STT / TTS HTTP client helpers
// ─────────────────────────────────────────────────────────────────

/// Call the STT endpoint (`POST /api/v1/stt`) with raw PCM bytes.
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
        .json(&serde_json::json!({"text": text, "voice": "bm_george"}))
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
