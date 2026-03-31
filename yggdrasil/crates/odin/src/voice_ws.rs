/// WebSocket voice handler for real-time voice interaction.
///
/// Accepts streamed PCM audio (s16le, 16 kHz, mono) from browser clients,
/// performs server-side VAD (energy-based), and sends audio to
/// LFM2.5-Audio-1.5B which handles STT + LLM + TTS in a single model.
/// Audio response is streamed back to the client.
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
    Json,
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::OdinError;
use crate::state::AppState;

/// WebSocket upgrade handler mounted at `GET /v1/voice`.
pub async fn ws_voice_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Result<Response, OdinError> {
    // Reject early if voice is not configured.
    state.voice_api_url.as_ref().ok_or_else(|| {
        OdinError::BadRequest("voice is not enabled".to_string())
    })?;
    Ok(ws.on_upgrade(move |socket| handle_voice_session(socket, state)))
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
/// RMS energy threshold for speech onset (voice activity detection).
const VAD_THRESHOLD: f32 = 0.02;
/// RMS energy threshold for silence detection (hysteresis — lower than onset).
/// Laptop mics with AGC can have a noise floor around 0.01, so silence must be
/// clearly below the onset threshold to avoid the VAD getting stuck in Listening.
const SILENCE_THRESHOLD: f32 = 0.008;
/// Number of consecutive below-threshold frames before end-of-speech.
/// Each binary WebSocket message counts as one frame for this purpose.
const SILENCE_TIMEOUT_FRAMES: u32 = 15;
/// Window size (in samples) for the RMS energy check — 0.5 seconds.
const VAD_WINDOW_SAMPLES: usize = SAMPLE_RATE as usize / 2;
/// Maximum time in Listening state before forcing processing (in samples).
/// Prevents VAD from getting stuck when background noise hovers near threshold.
const MAX_UTTERANCE_SAMPLES: usize = 10 * SAMPLE_RATE as usize;

// ─────────────────────────────────────────────────────────────────
// Conversation state
// ─────────────────────────────────────────────────────────────────

/// Result of processing an utterance — drives the conversation state machine.
#[derive(Debug)]
enum ProcessResult {
    /// Audio wasn't addressed to Fergus.
    NotAddressed,
    /// Greeting or question — stay in conversation, keep listening.
    Continue(String),
    /// Action completed — offer follow-up, shorter timeout.
    Done(String),
    /// User dismissed Fergus — return to idle.
    Dismiss(String),
    /// System is busy processing another request.
    Busy,
}

/// Path to pre-rendered personality audio presets.
const SOUNDS_DIR: &str = "/opt/yggdrasil/sounds";

/// Send a pre-rendered PCM preset file over the WebSocket.
async fn send_preset(socket: &mut WebSocket, name: &str) {
    let path = format!("{SOUNDS_DIR}/{name}.pcm");
    let Ok(data) = tokio::fs::read(&path).await else {
        tracing::warn!(path, "preset file not found");
        return;
    };
    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "audio_start", "sample_rate": 24000})
                .to_string().into(),
        ))
        .await;
    for chunk in data.chunks(8192) {
        if socket.send(Message::Binary(chunk.to_vec().into())).await.is_err() {
            return;
        }
    }
    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "audio_end"}).to_string().into(),
        ))
        .await;
}

/// Parse a conversation flow tag from the end of a response.
/// Returns (cleaned_text, tag) where tag is one of CONTINUE/DONE/DISMISS/NOT_ADDRESSED.
fn parse_flow_tag(text: &str) -> (String, &'static str) {
    let trimmed = text.trim();
    for tag in &["[CONTINUE]", "[DONE]", "[DISMISS]", "[NOT_ADDRESSED]"] {
        if trimmed.ends_with(tag) {
            let cleaned = trimmed[..trimmed.len() - tag.len()].trim().to_string();
            return (cleaned, tag);
        }
    }
    // No tag found — default to CONTINUE (keep conversation open).
    (trimmed.to_string(), "[CONTINUE]")
}

/// Conversation timeout (seconds of silence before returning to idle).
const CONVERSATION_TIMEOUT_SECS: u64 = 20;
/// Shorter timeout after a completed action.
const CONVERSATION_DONE_TIMEOUT_SECS: u64 = 5;

// ─────────────────────────────────────────────────────────────────
// Per-connection session
// ─────────────────────────────────────────────────────────────────

/// Drive a single voice WebSocket connection through the VAD -> STT -> LLM -> TTS loop.
async fn handle_voice_session(mut socket: WebSocket, state: AppState) {
    let mut session_id = Uuid::new_v4().to_string();

    // Subscribe to voice alert broadcast channel.
    let mut alert_rx = state.voice_alert_tx.subscribe();

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
    // Only honour the first *successful* session resume (valid session_id found in store).
    // A failed resume attempt (unknown session_id) leaves this false so the client can retry.
    let mut seen_resume = false;

    // ── Dynamic VAD calibration ──────────────────────────────────
    // Measure ambient noise for the first 2 seconds to set thresholds
    // adaptively. Until calibration completes, use conservative defaults.
    const CALIBRATION_SAMPLES: usize = 2 * SAMPLE_RATE as usize; // 2 seconds
    let mut calibration_buf: Vec<i16> = Vec::with_capacity(CALIBRATION_SAMPLES);
    let mut calibrated = false;
    let mut dyn_vad_threshold = VAD_THRESHOLD;
    let mut dyn_silence_threshold = SILENCE_THRESHOLD;

    // Conversation state — when active, wake word is not required.
    let mut conversation_active = false;
    let mut conversation_timeout = std::time::Instant::now();
    let mut idle_since = std::time::Instant::now();
    let mut conv_timeout_secs = CONVERSATION_TIMEOUT_SECS;
    let mut last_response = String::new(); // Echo cancellation: last Fergus spoken text

    loop {
        // Select between WebSocket messages and broadcast alerts.
        let ws_msg = tokio::select! {
            msg = socket.recv() => msg,
            alert = alert_rx.recv() => {
                // Voice alert from Sentinel — send as alert message + TTS.
                if let Ok(text) = alert {
                    let _ = socket
                        .send(Message::Text(
                            serde_json::json!({"type": "alert", "text": &text})
                                .to_string().into(),
                        ))
                        .await;
                    let voice_api_url = state.voice_api_url.as_deref().unwrap_or_default();
                    send_tts(&mut socket, &state.http_client, voice_api_url, &text).await;
                }
                continue;
            }
        };

        match ws_msg {
            // ── Binary: raw PCM s16le audio ──────────────────────
            Some(Ok(Message::Binary(data))) => {
                // ── Echo suppression: drop audio while TTS is playing ──
                if state.omni_busy.load(std::sync::atomic::Ordering::Relaxed) {
                    continue;
                }

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
                        // ── Calibration phase: measure noise floor ──
                        if !calibrated {
                            calibration_buf.extend_from_slice(&samples);
                            if calibration_buf.len() >= CALIBRATION_SAMPLES {
                                let noise_floor = rms_energy_i16(&calibration_buf);
                                dyn_vad_threshold = (noise_floor * 3.0_f32).max(VAD_THRESHOLD);
                                dyn_silence_threshold = (noise_floor * 1.5_f32).max(SILENCE_THRESHOLD);
                                calibrated = true;
                                tracing::info!(
                                    noise_floor = %format!("{noise_floor:.6}"),
                                    vad_threshold = %format!("{dyn_vad_threshold:.6}"),
                                    silence_threshold = %format!("{dyn_silence_threshold:.6}"),
                                    "voice: VAD calibrated to ambient noise"
                                );
                                // Free calibration buffer
                                calibration_buf = Vec::new();
                            }
                            // Don't process speech during calibration
                        } else if energy > dyn_vad_threshold {
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
                        let speech_len = audio_buffer.len().saturating_sub(speech_start_sample);
                        let utterance_timeout = speech_len >= MAX_UTTERANCE_SAMPLES;

                        if energy <= dyn_silence_threshold {
                            silence_frames += 1;
                        } else {
                            silence_frames = 0;
                        }

                        if silence_frames >= SILENCE_TIMEOUT_FRAMES || utterance_timeout {
                            if utterance_timeout {
                                tracing::info!(speech_secs = speech_len / SAMPLE_RATE as usize, "voice: max utterance timeout — forcing processing");
                            }

                            // Pick the right wake preset based on idle duration.
                            let wake_preset = if !conversation_active {
                                let idle_secs = idle_since.elapsed().as_secs();
                                Some(if idle_secs > 1800 {
                                    "wake_drowsy"
                                } else if idle_secs > 300 {
                                    "wake_normal"
                                } else {
                                    "wake_fresh"
                                })
                            } else {
                                None // Already in conversation, no wake preset.
                            };

                            // Only send the speech portion, not pre-speech silence.
                            let speech_audio = &audio_buffer[speech_start_sample..];

                            // ── Reject noise: too short (<300ms) or too quiet ──
                            let speech_duration_ms = speech_audio.len() as f32 / SAMPLE_RATE as f32 * 1000.0;
                            let speech_rms = rms_energy_i16(speech_audio);
                            if speech_duration_ms < 300.0 || speech_rms < VAD_THRESHOLD * 0.5 {
                                tracing::debug!(
                                    duration_ms = speech_duration_ms,
                                    rms = speech_rms,
                                    "voice: rejecting short/quiet utterance"
                                );
                                audio_buffer.clear();
                                speech_start_sample = 0;
                                vad_state = VadState::Idle;
                                silence_frames = 0;
                                continue;
                            }

                            let result = process_utterance(
                                &mut socket,
                                &state,
                                &session_id,
                                speech_audio,
                                conversation_active,
                                &last_response,
                            )
                            .await;

                            // Update echo cancellation state from result.
                            match &result {
                                ProcessResult::Continue(text) | ProcessResult::Done(text) | ProcessResult::Dismiss(text) => {
                                    last_response = text.clone();
                                }
                                _ => {}
                            }

                            // Drive conversation state machine based on result.
                            match result {
                                ProcessResult::NotAddressed => {
                                    // Not for us — but don't let noise-processing
                                    // time eat the conversation timer.
                                    if conversation_active {
                                        conversation_timeout = std::time::Instant::now();
                                    }
                                }
                                ProcessResult::Continue(_) => {
                                    if !conversation_active {
                                        // First activation — play wake preset.
                                        if let Some(preset) = wake_preset {
                                            // Don't play preset — omni already responded.
                                            tracing::info!(preset, "voice: conversation started");
                                        }
                                    }
                                    conversation_active = true;
                                    conversation_timeout = std::time::Instant::now();
                                    conv_timeout_secs = CONVERSATION_TIMEOUT_SECS;
                                }
                                ProcessResult::Done(_) => {
                                    conversation_active = true;
                                    conversation_timeout = std::time::Instant::now();
                                    conv_timeout_secs = CONVERSATION_DONE_TIMEOUT_SECS;
                                }
                                ProcessResult::Dismiss(_) => {
                                    conversation_active = false;
                                    idle_since = std::time::Instant::now();
                                    send_preset(&mut socket, "dismiss_ack").await;
                                }
                                ProcessResult::Busy => {
                                    send_preset(&mut socket, "busy_processing").await;
                                }
                            }

                            // Reset for next utterance.
                            audio_buffer.clear();
                            speech_start_sample = 0;
                            vad_state = VadState::Idle;
                            silence_frames = 0;

                            // Check conversation timeout.
                            if conversation_active
                                && conversation_timeout.elapsed().as_secs() > conv_timeout_secs
                            {
                                tracing::info!("voice: conversation timed out");
                                send_preset(&mut socket, "timeout_idle").await;
                                conversation_active = false;
                                idle_since = std::time::Instant::now();
                            }
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
                        Some("resume") if !seen_resume => {
                            if let Some(id) = msg.get("session_id").and_then(|v| v.as_str())
                                && state.session_store.get_session(id).is_some()
                            {
                                seen_resume = true;
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
                        Some("vad_end") => {
                            // Client-side VAD signalled end-of-speech (mic stop or client VAD).
                            // Process if we have audio, regardless of server-side VAD state.
                            if !audio_buffer.is_empty() {
                                let start = speech_start_sample.min(audio_buffer.len());
                                let speech_audio = &audio_buffer[start..];
                                let _result = process_utterance(
                                    &mut socket,
                                    &state,
                                    &session_id,
                                    speech_audio,
                                    conversation_active,
                                    &last_response,
                                )
                                .await;
                                if let ProcessResult::Continue(ref text) | ProcessResult::Done(ref text) | ProcessResult::Dismiss(ref text) = _result {
                                    last_response = text.clone();
                                }

                                audio_buffer.clear();
                                speech_start_sample = 0;
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
    _session_id: &str,
    audio_buffer: &[i16],
    conversation_active: bool,
    _last_response: &str,
) -> ProcessResult {
    let http = &state.http_client;
    let voice_api_url = state.voice_api_url.as_deref().unwrap_or_default();
    let tts_url = voice_api_url;
    let omni_url = state.omni_url.as_deref();

    // Notify client that we are processing.
    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "processing"}).to_string().into(),
        ))
        .await;

    // ── Energy floor check — reject noise before burning inference ──
    let overall_rms = rms_energy_i16(audio_buffer);
    if overall_rms < VAD_THRESHOLD * 0.75 {
        tracing::debug!(rms = overall_rms, "voice: utterance below energy floor — skipping");
        return ProcessResult::NotAddressed;
    }

    // ── Busy check ────────────────────────────────────────────────
    if state.omni_busy.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::info!("voice: omni busy — playing busy preset");
        return ProcessResult::Busy;
    }

    // ── Wake word gate (skipped during active conversation) ─────
    if !conversation_active {
        let wake_user = state
            .wake_word_registry
            .check(audio_buffer, &state.skill_cache)
            .await;

        if let Some(ref m) = wake_user {
            tracing::info!(
                user = %m.user_id,
                similarity = m.similarity,
                "voice: SDR wake word match — fast path"
            );
        } else {
            tracing::debug!("voice: no SDR match — passing to omni for wake word detection");
        }
    } else {
        tracing::debug!("voice: conversation active — skipping wake word check");
    }

    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "transcript", "text": "listening..."})
                .to_string().into(),
        ))
        .await;

    // ── LFM-Audio path: single model handles STT + LLM + TTS in one pass ──
    // System prompt includes tool definitions; model outputs <tool_call> tags.
    let (response_text, response_audio) = if let Some(omni) = omni_url {
        // Build the system prompt with persona + tool routing + gaming context.
        // ── Speaker identification ────────────────────────────────
        let speaker_id = state
            .wake_word_registry
            .identify(audio_buffer, &state.skill_cache)
            .await
            .map(|m| m.user_id)
            .unwrap_or_else(|| {
                // Unknown speaker — auto-enroll as guest (fire and forget).
                "unknown".to_string()
            });
        tracing::info!(speaker = %speaker_id, "voice: identified speaker");

        let rag_context = crate::rag::RagContext::default();
        let gaming_ctx = state.gaming_config.as_ref().map(|gc| {
            let names: Vec<String> = gc.hosts.iter().flat_map(|h| {
                let vms = h.vms.iter().map(|v| v.name.as_str());
                let cts = h.containers.iter().map(|c| c.name.as_str());
                vms.chain(cts).map(|n| format!("{}/{}", h.name, n))
            }).collect();
            format!("Managed VMs/containers: {}", names.join(", "))
        });
        let mut system_prompt = crate::rag::build_system_prompt(
            &rag_context,
            "voice",
            gaming_ctx.as_deref(),
        );
        // Inject speaker context so Fergus can address the user by name.
        if speaker_id != "unknown" {
            system_prompt.push_str(&format!(
                "\n\nCurrent speaker: {speaker_id}. Address them by name when appropriate."
            ));
        }

        // ── SDR skill cache: fast-path for repeat commands ──────────
        // Fingerprint the raw audio directly into a 256-bit SDR (~1ms, no
        // network, no models). If a cached skill matches, skip the LLM entirely.
        let audio_sdr = state.skill_cache.fingerprint(audio_buffer);
        if let Some(skill_match) = state.skill_cache.match_skill(&audio_sdr).await {
            tracing::info!(
                tool = %skill_match.tool_name,
                similarity = skill_match.similarity,
                "skill cache HIT — skipping LLM inference"
            );

            // Execute cached tool directly.
            if let Some(spec) = crate::tool_registry::find_tool(
                &state.tool_registry, &skill_match.tool_name,
            ) {
                let effective_timeout = spec
                    .timeout_override_secs
                    .map(std::time::Duration::from_secs)
                    .unwrap_or(std::time::Duration::from_secs(15));

                let result = tokio::time::timeout(
                    effective_timeout,
                    crate::tool_registry::execute_tool(
                        state, spec, &skill_match.tool_args, effective_timeout,
                    ),
                ).await;

                let result_text = match result {
                    Ok(Ok(output)) => output,
                    Ok(Err(e)) => format!("Error: {e}"),
                    Err(_) => "timed out".to_string(),
                };

                // Brief confirmation via omni text chat.
                let (confirmation, conf_audio) = match call_omni_text_chat(
                    http, omni,
                    &format!("Tool {} result: {}. Give a brief spoken confirmation.", skill_match.tool_name, result_text),
                    &system_prompt,
                ).await {
                    Ok((text, audio, _sr)) => (text, audio),
                    Err(_) => (format!("Done, sir. {result_text}"), None),
                };

                let _ = socket
                    .send(Message::Text(
                        serde_json::json!({"type": "response", "text": &confirmation})
                            .to_string()
                            .into(),
                    ))
                    .await;
                state.omni_busy.store(true, std::sync::atomic::Ordering::Relaxed);
                if let Some(wav_bytes) = conf_audio {
                    send_wav_audio(socket, &wav_bytes).await;
                } else {
                    send_tts(socket, http, tts_url, &confirmation).await;
                }
                state.omni_busy.store(false, std::sync::atomic::Ordering::Relaxed);
                return ProcessResult::Done(confirmation);
            }
        }

        // Cache missed — compute pcm_bytes only now (deferred from top of fn).
        let pcm_bytes = pcm_to_bytes(audio_buffer);

        match call_omni_chat(http, omni, &pcm_bytes, &system_prompt).await {
            Ok((raw_response, audio, _sr)) if !raw_response.is_empty() => {
                // Check if omni decided the user wasn't addressing Fergus.
                let cleaned = strip_think_tags(&raw_response);
                if cleaned.contains("[NOT_ADDRESSED]") {
                    tracing::info!("voice: omni says not addressed — ignoring");
                    return ProcessResult::NotAddressed;
                }

                // Parse and execute any tool calls from the response.
                let (tool_calls, spoken_text_raw) = parse_tool_calls(&raw_response);
                let spoken_text = strip_think_tags(&spoken_text_raw);

                if !tool_calls.is_empty() {
                    let mut tool_results = Vec::new();
                    let tool_timeout = std::time::Duration::from_secs(
                        state.config.agent.as_ref()
                            .map(|a| a.tool_timeout_secs)
                            .unwrap_or(15),
                    );

                    for tc in &tool_calls {
                        if let Some(spec) = crate::tool_registry::find_tool(&state.tool_registry, &tc.name) {
                            let effective_timeout = spec
                                .timeout_override_secs
                                .map(std::time::Duration::from_secs)
                                .unwrap_or(tool_timeout);

                            tracing::info!(
                                tool = %tc.name,
                                args = %tc.args,
                                timeout_secs = effective_timeout.as_secs(),
                                "executing tool call from omni response"
                            );

                            let result = tokio::time::timeout(
                                effective_timeout,
                                crate::tool_registry::execute_tool(state, spec, &tc.args, effective_timeout),
                            )
                            .await;

                            match result {
                                Ok(Ok(output)) => {
                                    tracing::info!(tool = %tc.name, "tool executed successfully");
                                    tool_results.push(format!("{}: {}", tc.name, output));
                                }
                                Ok(Err(e)) => {
                                    tracing::warn!(tool = %tc.name, error = %e, "tool execution failed");
                                    tool_results.push(format!("{}: Error: {}", tc.name, e));
                                }
                                Err(_) => {
                                    tracing::warn!(tool = %tc.name, "tool execution timed out");
                                    tool_results.push(format!("{}: timed out", tc.name));
                                }
                            }
                        } else {
                            tracing::warn!(tool = %tc.name, "unknown tool in omni response");
                        }
                    }

                    // Learn successful tool calls in the SDR skill cache.
                    if tool_results.iter().any(|r| !r.contains("Error:") && !r.contains("timed out")) {
                        let skill_cache = state.skill_cache.clone();
                        let first_tc_name = tool_calls[0].name.clone();
                        let first_tc_args = tool_calls[0].args.clone();
                        let label = spoken_text.clone();
                        tokio::spawn(async move {
                            skill_cache.learn(audio_sdr, label, first_tc_name, first_tc_args).await;
                        });
                    }

                    // Ask omni for a spoken confirmation with tool results.
                    if !tool_results.is_empty() {
                        let confirmation_prompt = format!(
                            "Tool results:\n{}\n\nGive a brief spoken confirmation of what happened.",
                            tool_results.join("\n")
                        );
                        match call_omni_text_chat(http, omni, &confirmation_prompt, &system_prompt).await {
                            Ok((text, conf_audio, _sr)) => (text, conf_audio),
                            Err(_) => {
                                let fallback = if spoken_text.is_empty() {
                                    format!("Done, sir. {}", tool_results.join(". "))
                                } else {
                                    spoken_text
                                };
                                (fallback, None)
                            }
                        }
                    } else {
                        (spoken_text, audio)
                    }
                } else {
                    // No tool calls — pure conversational response.
                    (spoken_text, audio)
                }
            }
            Ok(_) => return ProcessResult::NotAddressed, // Empty response — silence
            Err(e) => {
                tracing::warn!(error = %e, "LFM-Audio chat failed");
                let _ = socket
                    .send(Message::Text(
                        serde_json::json!({"type": "error", "message": format!("voice model unavailable: {e}")})
                            .to_string().into(),
                    ))
                    .await;
                return ProcessResult::NotAddressed;
            }
        }
    } else {
        // No omni server configured — voice is non-functional.
        tracing::warn!("voice: no omni_url configured — cannot process audio");
        let _ = socket
            .send(Message::Text(
                serde_json::json!({"type": "error", "message": "voice model not configured"})
                    .to_string().into(),
            ))
            .await;
        return ProcessResult::NotAddressed;
    };

    // ── Parse conversation flow tag and send response ────────────
    tracing::info!(response_len = response_text.len(), response_preview = %response_text.chars().take(100).collect::<String>(), "voice: LLM response received");
    let (spoken_text, flow_tag) = parse_flow_tag(&response_text);

    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "response", "text": &spoken_text})
                .to_string()
                .into(),
        ))
        .await;

    // Mark omni as busy during audio playback (blocks audio input + other sessions).
    state.omni_busy.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(wav_bytes) = response_audio {
        // Use audio directly from LFM-Audio model response.
        send_wav_audio(socket, &wav_bytes).await;
    } else {
        // Fallback to TTS endpoint on same server.
        send_tts(socket, http, tts_url, &spoken_text).await;
    }
    state.omni_busy.store(false, std::sync::atomic::Ordering::Relaxed);

    // Echo cancellation: spoken_text is captured by caller via ProcessResult.

    match flow_tag {
        "[CONTINUE]" => ProcessResult::Continue(spoken_text),
        "[DONE]" => ProcessResult::Done(spoken_text),
        "[DISMISS]" => ProcessResult::Dismiss(spoken_text),
        _ => ProcessResult::Continue(spoken_text),
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

/// Send pre-encoded WAV audio (from LFM-Audio model response) over WebSocket.
/// Decodes the WAV to extract raw PCM, then streams it in chunks.
async fn send_wav_audio(socket: &mut WebSocket, wav_bytes: &[u8]) {
    // Parse WAV to get sample rate and raw PCM data.
    let (pcm_bytes, sample_rate) = if wav_bytes.len() > 44
        && wav_bytes[..4] == *b"RIFF"
        && wav_bytes[8..12] == *b"WAVE"
    {
        let wav_sr = u32::from_le_bytes([wav_bytes[24], wav_bytes[25], wav_bytes[26], wav_bytes[27]]);
        let bits = u16::from_le_bytes([wav_bytes[34], wav_bytes[35]]);
        let data_offset = find_wav_data_chunk(wav_bytes).unwrap_or(44);
        let pcm = &wav_bytes[data_offset..];
        let pcm_i16 = if bits == 32 { convert_f32_to_i16_pcm(pcm) } else { pcm.to_vec() };
        (pcm_i16, wav_sr)
    } else {
        // Not a WAV — treat as raw PCM at 24kHz.
        (wav_bytes.to_vec(), 24000)
    };

    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "audio_start", "sample_rate": sample_rate})
                .to_string().into(),
        ))
        .await;
    for chunk in pcm_bytes.chunks(8192) {
        if socket.send(Message::Binary(chunk.to_vec().into())).await.is_err() {
            return;
        }
    }
    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "audio_end"}).to_string().into(),
        ))
        .await;
}

/// Send TTS audio back over the WebSocket.
async fn send_tts(socket: &mut WebSocket, http: &reqwest::Client, tts_url: &str, text: &str) {
    match call_tts(http, tts_url, text).await {
        Ok((audio_bytes, sample_rate)) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "audio_start", "sample_rate": sample_rate})
                        .to_string().into(),
                ))
                .await;
            for chunk in audio_bytes.chunks(8192) {
                if socket.send(Message::Binary(chunk.to_vec().into())).await.is_err() {
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
                        .to_string().into(),
                ))
                .await;
        }
    }
}

/// Call LFM-Audio server with audio input AND a system prompt.
/// The model handles STT + LLM + TTS in a single pass, returning both
/// text and spoken audio. The model can respond with `<tool_call>` tags
/// which Odin will parse and execute.
///
/// Returns `(response_text, Option<audio_bytes>, sample_rate)` on success.
async fn call_omni_chat(
    client: &reqwest::Client,
    omni_url: &str,
    pcm_bytes: &[u8],
    system_prompt: &str,
) -> Result<(String, Option<Vec<u8>>, u32), String> {
    use base64::Engine;

    let wav_bytes = pcm_to_wav(pcm_bytes, 16000);
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&wav_bytes);

    let chat_url = format!("{omni_url}/api/v1/chat");
    let body = serde_json::json!({
        "audio_b64": audio_b64,
        "system_prompt": system_prompt,
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

    // Decode audio response if present (base64 WAV from LFM-Audio).
    let audio_bytes = result["audio_b64"]
        .as_str()
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok());

    tracing::info!(
        response = %response_text,
        has_audio = audio_bytes.is_some(),
        "LFM-Audio chat response"
    );
    Ok((response_text, audio_bytes, 24000))
}

/// Call LFM-Audio with text-only input (no audio) for follow-up confirmation.
/// Returns `(text, Option<audio_bytes>, sample_rate)`.
async fn call_omni_text_chat(
    client: &reqwest::Client,
    omni_url: &str,
    text: &str,
    system_prompt: &str,
) -> Result<(String, Option<Vec<u8>>, u32), String> {
    use base64::Engine;

    let chat_url = format!("{omni_url}/api/v1/chat");
    let body = serde_json::json!({
        "text": text,
        "system_prompt": system_prompt,
    });

    let resp = client
        .post(&chat_url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("omni text chat failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("omni text chat returned {}", resp.status()));
    }

    let result: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("omni text chat parse failed: {e}"))?;

    let response_text = strip_think_tags(result["text"].as_str().unwrap_or(""));

    let audio_bytes = result["audio_b64"]
        .as_str()
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok());

    Ok((response_text, audio_bytes, 24000))
}

// ─────────────────────────────────────────────────────────────────
// Tool call parsing
// ─────────────────────────────────────────────────────────────────

/// A tool call parsed from `<tool_call>{"name":"...","args":{...}}</tool_call>` tags.
struct ParsedToolCall {
    name: String,
    args: serde_json::Value,
}

/// Parse `<tool_call>...</tool_call>` tags from model output.
/// Returns (parsed_calls, remaining_text_with_tags_stripped).
fn parse_tool_calls(text: &str) -> (Vec<ParsedToolCall>, String) {
    let mut calls = Vec::new();
    let mut cleaned = text.to_string();

    while let Some(start) = cleaned.find("<tool_call>") {
        let Some(end) = cleaned.find("</tool_call>") else {
            break;
        };

        let json_str = &cleaned[start + "<tool_call>".len()..end];
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str.trim()) {
            let name = parsed["name"].as_str().unwrap_or("").to_string();
            let args = parsed["args"].clone();
            if !name.is_empty() {
                calls.push(ParsedToolCall { name, args });
            }
        } else {
            tracing::warn!(json = json_str, "failed to parse tool_call JSON");
        }

        cleaned.replace_range(start..end + "</tool_call>".len(), "");
    }

    (calls, cleaned.trim().to_string())
}

/// Convert raw i16 PCM samples to a flat `Vec<u8>` of little-endian s16 bytes.
fn pcm_to_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
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

// ─────────────────────────────────────────────────────────────────
// STT / TTS HTTP client helpers
// ─────────────────────────────────────────────────────────────────

/// Call ygg-voice `POST /api/v1/tts` with a JSON text payload.
///
/// Returns `(pcm_bytes, sample_rate)` on success.  The sample rate is read from
/// the `x-sample-rate` response header.  If the response is a WAV file (RIFF
/// header detected), the raw PCM data is extracted and 32-bit float samples are
/// converted to 16-bit integer — this handles backward compatibility with TTS
/// servers that return WAV instead of raw PCM.
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

    // Read sample rate from header (preferred source).
    let header_sample_rate: Option<u32> = resp
        .headers()
        .get("x-sample-rate")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let raw_bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("TTS body failed: {e}"))?;

    // Detect WAV format (RIFF header) and extract raw PCM data + sample rate.
    let (pcm_bytes, sample_rate) = if raw_bytes.len() > 44
        && raw_bytes[..4] == *b"RIFF"
        && raw_bytes[8..12] == *b"WAVE"
    {
        let wav_sample_rate = u32::from_le_bytes([
            raw_bytes[24], raw_bytes[25], raw_bytes[26], raw_bytes[27],
        ]);
        let bits_per_sample = u16::from_le_bytes([raw_bytes[34], raw_bytes[35]]);

        // Find the "data" chunk — usually at offset 36 but can vary.
        let data_offset = find_wav_data_chunk(&raw_bytes).unwrap_or(44);
        let pcm_data = &raw_bytes[data_offset..];

        // If WAV contains 32-bit float samples, convert to 16-bit integer PCM.
        let pcm_i16 = if bits_per_sample == 32 {
            convert_f32_to_i16_pcm(pcm_data)
        } else {
            pcm_data.to_vec()
        };

        let sr = header_sample_rate.unwrap_or(wav_sample_rate);
        tracing::debug!(
            wav_sr = wav_sample_rate,
            bits = bits_per_sample,
            data_offset,
            pcm_len = pcm_i16.len(),
            "parsed WAV response from TTS"
        );
        (pcm_i16, sr)
    } else {
        // Raw PCM — use as-is.
        (raw_bytes.to_vec(), header_sample_rate.unwrap_or(24_000))
    };

    Ok((pcm_bytes, sample_rate))
}

/// Find the byte offset of the "data" chunk payload in a WAV file.
fn find_wav_data_chunk(wav: &[u8]) -> Option<usize> {
    let mut pos = 12; // skip RIFF + file-size + WAVE
    while pos + 8 <= wav.len() {
        let chunk_size = u32::from_le_bytes([
            wav[pos + 4], wav[pos + 5], wav[pos + 6], wav[pos + 7],
        ]) as usize;
        if &wav[pos..pos + 4] == b"data" {
            return Some(pos + 8);
        }
        pos += 8 + chunk_size;
    }
    None
}

/// Convert 32-bit IEEE-754 float PCM samples to 16-bit integer PCM (little-endian).
fn convert_f32_to_i16_pcm(float_bytes: &[u8]) -> Vec<u8> {
    float_bytes
        .chunks_exact(4)
        .flat_map(|chunk| {
            let f = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let i = (f.clamp(-1.0, 1.0) * 32767.0) as i16;
            i.to_le_bytes()
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/voice/alert — inject alert to all voice clients
// ─────────────────────────────────────────────────────────────────

/// Request body for voice alert injection.
#[derive(Debug, Deserialize)]
pub struct VoiceAlertRequest {
    pub text: String,
}

/// Accept an alert from Sentinel (or any service) and broadcast it to all
/// connected voice WebSocket sessions. Each session will speak it via TTS.
pub async fn voice_alert_handler(
    State(state): State<AppState>,
    Json(req): Json<VoiceAlertRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    if req.text.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "text field must not be empty".to_string(),
        ));
    }

    let receivers = state.voice_alert_tx.receiver_count();
    tracing::info!(text = %req.text, receivers, "broadcasting voice alert to WebSocket clients");

    // broadcast::send returns Err if there are no active receivers — that's OK.
    let _ = state.voice_alert_tx.send(req.text);

    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({"status": "broadcast", "receivers": receivers}))))
}

// ─────────────────────────────────────────────────────────────────
// GET /voice — browser client
// ─────────────────────────────────────────────────────────────────

/// Serve the embedded voice client HTML page.
pub async fn voice_page() -> impl IntoResponse {
    Html(include_str!("voice_ui.html"))
}
