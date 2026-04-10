//! Voice pipeline: WebSocket client to Odin's `/v1/voice` endpoint.
//!
//! Captures audio from the local microphone, streams PCM frames to Odin
//! over WebSocket, and plays back TTS audio through the speakers. All
//! speech understanding (STT, wake word, omni chat, tool execution) is
//! handled server-side by Odin — this module is a thin audio I/O bridge.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::audio::{self, AudioCapture, AudioPlayer};

/// Energy VAD threshold for speech onset (RMS). Tuned for built-in laptop mics.
const VAD_ENERGY_THRESHOLD: f32 = 0.02;
/// Silence threshold (hysteresis — lower than onset to avoid stuck VAD).
const VAD_SILENCE_THRESHOLD: f32 = 0.008;
/// Duration of audio to check for VAD (seconds).
const VAD_WINDOW_SECONDS: f32 = 0.5;
/// Silence duration to end an utterance (seconds).
const SILENCE_TIMEOUT_SECONDS: f32 = 1.5;
/// Maximum utterance duration (seconds) — force processing if VAD stays in Listening.
const MAX_UTTERANCE_SECONDS: f32 = 10.0;
/// How often to poll the audio buffer (milliseconds).
const POLL_INTERVAL_MS: u64 = 100;
/// Reconnect backoff (seconds).
const RECONNECT_DELAY_SECS: u64 = 3;
/// Size of PCM chunks sent per WebSocket frame (4096 samples = 0.256s at 16kHz).
const PCM_CHUNK_SAMPLES: usize = 4096;

/// VAD calibration constants — must stay in sync with odin voice_ws.rs.
const CALIBRATION_WINDOW_SECS: f32 = 2.0;
const VAD_ONSET_MULTIPLIER: f32 = 3.0;
const VAD_SILENCE_MULTIPLIER: f32 = 1.5;
const VAD_MAX_ONSET: f32 = 0.15;
const VAD_MAX_SILENCE: f32 = 0.10;

/// Pipeline state machine states.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PipelineState {
    Idle,
    Listening,
}

/// Voice pipeline — thin WebSocket audio bridge to Odin.
pub struct VoicePipeline {
    capture: AudioCapture,
    odin_ws_url: String,
    sample_rate: u32,
    busy: Arc<AtomicBool>,
    alert_rx: Mutex<tokio::sync::mpsc::Receiver<String>>,
}

impl VoicePipeline {
    pub fn new(
        capture: AudioCapture,
        odin_ws_url: String,
        sample_rate: u32,
        alert_rx: tokio::sync::mpsc::Receiver<String>,
    ) -> Self {
        Self {
            capture,
            odin_ws_url,
            sample_rate,
            busy: Arc::new(AtomicBool::new(false)),
            alert_rx: Mutex::new(alert_rx),
        }
    }

    /// Run the voice pipeline loop. Reconnects on WebSocket drop.
    pub async fn run(&self) {
        info!(ws_url = %self.odin_ws_url, "voice pipeline started — connecting to Odin");

        loop {
            match self.run_session().await {
                Ok(()) => info!("WebSocket session ended cleanly"),
                Err(e) => warn!(error = %e, "WebSocket session failed"),
            }
            info!(delay_secs = RECONNECT_DELAY_SECS, "reconnecting...");
            tokio::time::sleep(std::time::Duration::from_secs(RECONNECT_DELAY_SECS)).await;
        }
    }

    /// Run a single WebSocket session. Returns when the connection drops.
    async fn run_session(&self) -> Result<(), VoiceError> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.odin_ws_url)
            .await
            .map_err(|e| VoiceError::Network(format!("WebSocket connect failed: {e}")))?;

        let (mut ws_tx, mut ws_rx) = ws_stream.split();
        info!("connected to Odin voice WebSocket");

        let mut state = PipelineState::Idle;
        let mut silence_counter: u32 = 0;
        let mut utterance_polls: u32 = 0;
        let mut last_send_pos: usize = self.capture.position();
        let silence_limit =
            (SILENCE_TIMEOUT_SECONDS * 1000.0 / POLL_INTERVAL_MS as f32) as u32;
        let max_utterance_polls =
            (MAX_UTTERANCE_SECONDS * 1000.0 / POLL_INTERVAL_MS as f32) as u32;

        // ── Dynamic VAD calibration ──────────────────────────────
        // Measure ambient noise to set thresholds adaptively.
        let calibration_needed = (CALIBRATION_WINDOW_SECS * self.sample_rate as f32) as usize;
        let mut calibration_samples: usize = 0;
        let mut calibration_energy_sum: f64 = 0.0;
        let mut calibration_energy_count: u32 = 0;
        let mut calibrated = false;
        let mut dyn_vad_threshold = VAD_ENERGY_THRESHOLD;
        let mut dyn_silence_threshold = VAD_SILENCE_THRESHOLD;

        // Audio playback state for received TTS.
        let mut audio_buf: Vec<u8> = Vec::new();
        let mut playback_sample_rate: u32 = 24_000;

        loop {
            tokio::select! {
                // ── Poll mic and send audio ──────────────────────────
                _ = tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)) => {
                    // Drain alerts in Idle.
                    if state == PipelineState::Idle {
                        if let Ok(mut rx) = self.alert_rx.try_lock()
                            && let Ok(text) = rx.try_recv()
                        {
                            info!(text = %text, "sending alert text to Odin for TTS");
                            // Send alert as a text command — Odin will TTS it.
                            let msg = serde_json::json!({"type": "alert", "text": text});
                            ws_tx.send(Message::Text(msg.to_string().into()))
                                .await
                                .map_err(|e| VoiceError::Network(e.to_string()))?;
                            continue;
                        }
                    }

                    let window = self.capture.read_last_seconds(VAD_WINDOW_SECONDS, self.sample_rate);
                    let energy = audio::rms_energy(&window);

                    match state {
                        PipelineState::Idle => {
                            // ── Calibration: measure noise floor ──
                            if !calibrated {
                                let window_samples = (VAD_WINDOW_SECONDS * self.sample_rate as f32) as usize;
                                calibration_samples += window_samples;
                                calibration_energy_sum += energy as f64;
                                calibration_energy_count += 1;
                                if calibration_samples >= calibration_needed {
                                    let noise_floor = (calibration_energy_sum / calibration_energy_count as f64) as f32;
                                    // Cap thresholds so speech triggers even in noisy rooms (fans/PCs).
                                    dyn_vad_threshold = (noise_floor * VAD_ONSET_MULTIPLIER).max(VAD_ENERGY_THRESHOLD).min(VAD_MAX_ONSET);
                                    dyn_silence_threshold = (noise_floor * VAD_SILENCE_MULTIPLIER).max(VAD_SILENCE_THRESHOLD).min(VAD_MAX_SILENCE);
                                    calibrated = true;
                                    info!(
                                        noise_floor = %format!("{noise_floor:.6}"),
                                        vad = %format!("{dyn_vad_threshold:.6}"),
                                        silence = %format!("{dyn_silence_threshold:.6}"),
                                        "VAD calibrated to ambient noise"
                                    );
                                }
                                // Don't trigger speech during calibration
                            } else if energy > dyn_vad_threshold {
                                state = PipelineState::Listening;
                                silence_counter = 0;
                                utterance_polls = 0;
                                last_send_pos = self.capture.position()
                                    .saturating_sub((VAD_WINDOW_SECONDS * self.sample_rate as f32) as usize);
                                tracing::debug!(energy, "voice activity → LISTENING");
                            }
                        }
                        PipelineState::Listening => {
                            utterance_polls += 1;

                            // Stream new audio to Odin as PCM s16le binary frames.
                            let current_pos = self.capture.position();
                            if current_pos > last_send_pos {
                                let new_samples = self.capture.read_since(last_send_pos);
                                last_send_pos = current_pos;

                                // Convert f32 → i16 le bytes and send in chunks.
                                let pcm_bytes: Vec<u8> = new_samples.iter().flat_map(|&s| {
                                    let i = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                                    i.to_le_bytes()
                                }).collect();

                                for chunk in pcm_bytes.chunks(PCM_CHUNK_SAMPLES * 2) {
                                    ws_tx.send(Message::Binary(chunk.to_vec().into()))
                                        .await
                                        .map_err(|e| VoiceError::Network(e.to_string()))?;
                                }
                            }

                            // Silence detection with hysteresis.
                            if energy <= dyn_silence_threshold {
                                silence_counter += 1;
                            } else {
                                silence_counter = 0;
                            }

                            if silence_counter >= silence_limit || utterance_polls >= max_utterance_polls {
                                if utterance_polls >= max_utterance_polls {
                                    info!("max utterance timeout — sending vad_end");
                                }
                                // Signal end-of-speech to Odin.
                                let vad_end = serde_json::json!({"type": "vad_end"});
                                ws_tx.send(Message::Text(vad_end.to_string().into()))
                                    .await
                                    .map_err(|e| VoiceError::Network(e.to_string()))?;

                                self.busy.store(true, Ordering::Relaxed);
                                state = PipelineState::Idle;
                                silence_counter = 0;
                                utterance_polls = 0;
                            }
                        }
                    }
                }

                // ── Receive messages from Odin ───────────────────────
                msg = ws_rx.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                match json.get("type").and_then(|t| t.as_str()) {
                                    Some("ready") => {
                                        let sid = json["session_id"].as_str().unwrap_or("?");
                                        info!(session_id = sid, "Odin voice session ready");
                                    }
                                    Some("listening") => {
                                        tracing::debug!("Odin: listening");
                                    }
                                    Some("processing") => {
                                        info!("Odin: processing utterance");
                                    }
                                    Some("transcript") => {
                                        let t = json["text"].as_str().unwrap_or("");
                                        info!(transcript = t, "Odin: transcript");
                                    }
                                    Some("response") => {
                                        let r = json["text"].as_str().unwrap_or("");
                                        info!(response = r, "Odin: response");
                                    }
                                    Some("audio_start") => {
                                        playback_sample_rate = json["sample_rate"]
                                            .as_u64()
                                            .unwrap_or(24_000) as u32;
                                        audio_buf.clear();
                                    }
                                    Some("audio_end") => {
                                        if !audio_buf.is_empty() {
                                            self.play_pcm_bytes(&audio_buf, playback_sample_rate).await;
                                            audio_buf.clear();
                                        }
                                        self.busy.store(false, Ordering::Relaxed);
                                    }
                                    Some("error") => {
                                        let m = json["message"].as_str().unwrap_or("unknown");
                                        warn!(message = m, "Odin voice error");
                                        self.busy.store(false, Ordering::Relaxed);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Some(Ok(Message::Binary(data))) => {
                            // TTS audio chunk — accumulate for playback.
                            audio_buf.extend_from_slice(&data);
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            info!("Odin WebSocket closed");
                            return Ok(());
                        }
                        Some(Err(e)) => {
                            return Err(VoiceError::Network(format!("WebSocket error: {e}")));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Play raw PCM s16le bytes through the speakers.
    async fn play_pcm_bytes(&self, pcm: &[u8], sample_rate: u32) {
        // Convert bytes → f32 samples for cpal playback.
        let samples: Vec<f32> = pcm
            .chunks_exact(2)
            .map(|chunk| {
                let i = i16::from_le_bytes([chunk[0], chunk[1]]);
                i as f32 / 32768.0
            })
            .collect();

        if samples.is_empty() {
            return;
        }

        let sr = sample_rate;
        if let Err(e) = tokio::task::spawn_blocking(move || {
            let player = AudioPlayer::new(sr)?;
            player.play_samples(&samples, sr)
        })
        .await
        .unwrap_or_else(|e| Err(crate::VoiceError::Audio(format!("playback panicked: {e}"))))
        {
            error!(error = %e, "TTS playback failed");
        }
    }
}

/// Voice pipeline errors (shared across the crate).
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("network error: {0}")]
    Network(String),

    #[error("audio error: {0}")]
    Audio(String),

    #[error("STT error: {0}")]
    Stt(String),

    #[error("TTS error: {0}")]
    Tts(String),

    #[error("model load error: {0}")]
    ModelLoad(String),

    #[error("Odin error (HTTP {status}): {body}")]
    OdinError { status: u16, body: String },
}
