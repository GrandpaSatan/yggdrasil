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
    Json,
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::OdinError;
use crate::handlers;
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
                                let speech_audio = &audio_buffer[speech_start_sample..];
                                process_utterance(
                                    &mut socket,
                                    &state,
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
    session_id: &str,
    audio_buffer: &[i16],
) {
    let http = &state.http_client;
    let voice_api_url = state.voice_api_url.as_deref().unwrap_or_default();
    let stt_url = state.stt_url.as_deref().unwrap_or(voice_api_url);
    let tts_url = voice_api_url;
    let omni_url = state.omni_url.as_deref();

    // Notify client that we are processing.
    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "processing"}).to_string().into(),
        ))
        .await;

    // ── Wake word gate ─────────────────────────────────────────────
    // Run STT first to check for "fergus". If not found, silently discard.
    // This prevents TV/music/background noise from triggering the full pipeline.
    // The transcript is reused by the legacy path to avoid a double STT call.
    let pcm_bytes_precheck = pcm_to_bytes(audio_buffer);
    tracing::info!(
        samples = audio_buffer.len(),
        stt_url = %stt_url,
        "voice: running STT for wake word check"
    );
    let wake_transcript = match call_stt(http, stt_url, &pcm_bytes_precheck).await {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => return, // empty transcript — silence, no error needed
        Err(e) => {
            tracing::warn!(error = %e, "STT failed in wake word check");
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "error", "message": format!("STT unavailable: {e}")})
                        .to_string().into(),
                ))
                .await;
            return;
        }
    };

    tracing::info!(transcript = %wake_transcript, "voice: STT result");
    let transcript_lower = wake_transcript.to_lowercase();
    // Match "fergus" and common STT mishearings
    let wake_words = ["fergus", "vergus", "furgus", "for gus", "fur gus", "fairgus"];
    let matched_wake = wake_words.iter().find(|w| transcript_lower.contains(*w));
    if matched_wake.is_none() {
        tracing::info!(transcript = %wake_transcript, "voice: no wake word — ignoring");
        return;
    }
    let wake_str = matched_wake.unwrap();
    tracing::info!(wake_word = %wake_str, "voice: wake word detected — processing command");

    // Strip the matched wake word variant to get the command.
    let command_text = transcript_lower.replace(wake_str, "").trim().to_string();

    // Just the wake word alone — acknowledge.
    if command_text.is_empty() {
        let _ = socket
            .send(Message::Text(
                serde_json::json!({"type": "transcript", "text": &wake_transcript})
                    .to_string().into(),
            ))
            .await;
        let _ = socket
            .send(Message::Text(
                serde_json::json!({"type": "response", "text": "I'm listening, sir."})
                    .to_string().into(),
            ))
            .await;
        send_tts(socket, http, tts_url, "I'm listening, sir.").await;
        return;
    }

    // Send transcript to client.
    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "transcript", "text": &wake_transcript})
                .to_string().into(),
        ))
        .await;

    // ── Omni path: MiniCPM-o handles STT + LLM + tool calls in one pass ──
    // System prompt includes tool definitions; model outputs <tool_call> tags.
    // Falls back to legacy STT → Ollama agent loop if omni is unavailable.
    let response_text = if let Some(omni) = omni_url {
        // Build the system prompt with persona + tool routing + gaming context.
        let rag_context = crate::rag::RagContext::default();
        let gaming_ctx = state.gaming_config.as_ref().map(|gc| {
            let vm_names: Vec<&str> = gc.vms.iter().map(|v| v.name.as_str()).collect();
            format!("Available VMs on Thor: {}", vm_names.join(", "))
        });
        let system_prompt = crate::rag::build_system_prompt(
            &rag_context,
            "voice",
            gaming_ctx.as_deref(),
        );

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
                let confirmation = call_omni_text_chat(
                    http, omni,
                    &format!("Tool {} result: {}. Give a brief spoken confirmation.", skill_match.tool_name, result_text),
                    &system_prompt,
                ).await.unwrap_or_else(|_| format!("Done, sir. {result_text}"));

                let _ = socket
                    .send(Message::Text(
                        serde_json::json!({"type": "response", "text": &confirmation})
                            .to_string()
                            .into(),
                    ))
                    .await;
                send_tts(socket, http, tts_url, &confirmation).await;
                return;
            }
        }

        // Cache missed — compute pcm_bytes only now (deferred from top of fn).
        let pcm_bytes = pcm_to_bytes(audio_buffer);

        match call_omni_chat(http, omni, &pcm_bytes, &system_prompt).await {
            Ok(raw_response) if !raw_response.is_empty() => {
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
                            // Use per-tool timeout if available.
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
                    // Uses the audio SDR already computed at the top of this block.
                    if tool_results.iter().any(|r| !r.contains("Error:") && !r.contains("timed out")) {
                        let skill_cache = state.skill_cache.clone();
                        let first_tc_name = tool_calls[0].name.clone();
                        let first_tc_args = tool_calls[0].args.clone();
                        let label = spoken_text.clone();
                        tokio::spawn(async move {
                            skill_cache.learn(audio_sdr, label, first_tc_name, first_tc_args).await;
                        });
                    }

                    // If we have tool results, ask omni for a spoken confirmation.
                    if !tool_results.is_empty() {
                        let confirmation_prompt = format!(
                            "Tool results:\n{}\n\nGive a brief spoken confirmation of what happened.",
                            tool_results.join("\n")
                        );
                        match call_omni_text_chat(http, omni, &confirmation_prompt, &system_prompt).await {
                            Ok(confirmation) => confirmation,
                            Err(_) => {
                                // Fallback: use the spoken text from the original response
                                if spoken_text.is_empty() {
                                    format!("Done, sir. {}", tool_results.join(". "))
                                } else {
                                    spoken_text
                                }
                            }
                        }
                    } else {
                        spoken_text
                    }
                } else {
                    // No tool calls — pure conversational response.
                    spoken_text
                }
            }
            Ok(_) => return, // Empty response — silence
            Err(e) => {
                tracing::warn!(error = %e, "omni chat failed, falling back to legacy pipeline");
                // Reuse the transcript from the wake word pre-check (no double STT).
                match handlers::process_chat_text(state, &command_text, session_id).await {
                    Ok(text) => strip_think_tags(&text),
                    Err(e) => {
                        let _ = socket
                            .send(Message::Text(
                                serde_json::json!({"type": "error", "message": format!("chat failed: {e}")})
                                    .to_string().into(),
                            ))
                            .await;
                        return;
                    }
                }
            }
        }
    } else {
        // Legacy path: reuse transcript from wake word pre-check (no double STT).
        match handlers::process_chat_text(state, &command_text, session_id).await {
            Ok(text) => strip_think_tags(&text),
            Err(e) => {
                let _ = socket
                    .send(Message::Text(
                        serde_json::json!({"type": "error", "message": format!("chat failed: {e}")})
                            .to_string().into(),
                    ))
                    .await;
                return;
            }
        }
    };

    let _ = socket
        .send(Message::Text(
            serde_json::json!({"type": "response", "text": &response_text})
                .to_string()
                .into(),
        ))
        .await;

    send_tts(socket, http, tts_url, &response_text).await;
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

/// Call MiniCPM-o omni server with audio input AND a system prompt that
/// includes tool definitions. The model can respond with `<tool_call>` tags
/// which Odin will parse and execute.
///
/// Returns `(transcript_placeholder, response_text)` on success.
async fn call_omni_chat(
    client: &reqwest::Client,
    omni_url: &str,
    pcm_bytes: &[u8],
    system_prompt: &str,
) -> Result<String, String> {
    use base64::Engine;

    let wav_bytes = pcm_to_wav(pcm_bytes, 16000);
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&wav_bytes);

    let chat_url = format!("{omni_url}/api/v1/chat");
    let body = serde_json::json!({
        "audio_b64": audio_b64,
        "generate_audio": false,
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

    tracing::info!(response = %response_text, "omni chat response");
    // Do NOT strip_think_tags here — the caller needs raw output to parse
    // <tool_call> tags first. Think tags are stripped from the spoken text
    // after tool call extraction in process_utterance.
    Ok(response_text)
}

/// Call MiniCPM-o with text-only input (no audio) for follow-up confirmation.
async fn call_omni_text_chat(
    client: &reqwest::Client,
    omni_url: &str,
    text: &str,
    system_prompt: &str,
) -> Result<String, String> {
    let chat_url = format!("{omni_url}/api/v1/chat");
    let body = serde_json::json!({
        "text": text,
        "generate_audio": false,
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

    Ok(strip_think_tags(result["text"].as_str().unwrap_or("")))
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
