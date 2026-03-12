//! Voice processing pipeline: two-tier SDR fast path + Whisper/Kokoro slow path.
//!
//! State machine:
//!   IDLE → (voice detected) → LISTENING → (silence) → PROCESSING → IDLE
//!
//! PROCESSING has two tiers:
//!   1. SDR fast path: mel fingerprint → Hamming match → cached response (~4μs)
//!   2. Full STT: Whisper → wake word check → Odin intent → Kokoro TTS → playback

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tracing::{error, info, warn};

use crate::audio::{self, AudioCapture, AudioPlayer};
use crate::mel::MelSpectrogram;
use crate::sdr_commands::{CommandResponse, SdrCommandRegistry};
use crate::stt::WhisperStt;
use crate::tts::KokoroTts;

/// Energy VAD threshold (RMS). Tuned for typical USB/built-in microphones.
const VAD_ENERGY_THRESHOLD: f32 = 0.01;
/// Duration of audio to check for VAD (seconds).
const VAD_WINDOW_SECONDS: f32 = 0.5;
/// Silence duration to end an utterance (seconds).
const SILENCE_TIMEOUT_SECONDS: f32 = 1.5;
/// How often to poll the audio buffer (milliseconds).
const POLL_INTERVAL_MS: u64 = 100;

/// Pipeline state machine states.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PipelineState {
    /// Waiting for voice activity.
    Idle,
    /// Capturing an utterance (voice detected).
    Listening,
    /// Processing the captured utterance.
    Processing,
}

/// Two-tier voice processing pipeline.
pub struct VoicePipeline {
    capture: AudioCapture,
    _player: AudioPlayer,
    stt: Arc<WhisperStt>,
    tts: Arc<KokoroTts>,
    sdr_registry: SdrCommandRegistry,
    mel: Arc<MelSpectrogram>,
    odin_url: String,
    client: reqwest::Client,
    busy_sound: Option<PathBuf>,
    busy: Arc<AtomicBool>,
    wake_word: String,
    sample_rate: u32,
}

impl VoicePipeline {
    /// Construct the pipeline from initialized components.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capture: AudioCapture,
        player: AudioPlayer,
        stt: WhisperStt,
        tts: KokoroTts,
        sdr_registry: SdrCommandRegistry,
        mel: MelSpectrogram,
        odin_url: String,
        busy_sound: Option<PathBuf>,
        wake_word: String,
        sample_rate: u32,
    ) -> Self {
        Self {
            capture,
            _player: player,
            stt: Arc::new(stt),
            tts: Arc::new(tts),
            sdr_registry,
            mel: Arc::new(mel),
            odin_url,
            client: reqwest::Client::new(),
            busy_sound,
            busy: Arc::new(AtomicBool::new(false)),
            wake_word,
            sample_rate,
        }
    }

    /// Run the voice pipeline loop. Does not return unless interrupted.
    pub async fn run(&self) {
        info!(
            wake_word = %self.wake_word,
            sdr_commands = self.sdr_registry.len(),
            "voice pipeline started"
        );

        let mut state = PipelineState::Idle;
        let mut utterance_start_pos: usize = 0;
        let mut silence_counter: u32 = 0;
        let silence_limit =
            (SILENCE_TIMEOUT_SECONDS * 1000.0 / POLL_INTERVAL_MS as f32) as u32;

        loop {
            tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;

            match state {
                PipelineState::Idle => {
                    let window = self
                        .capture
                        .read_last_seconds(VAD_WINDOW_SECONDS, self.sample_rate);
                    let energy = audio::rms_energy(&window);

                    if energy > VAD_ENERGY_THRESHOLD {
                        utterance_start_pos = self.capture.position();
                        silence_counter = 0;
                        state = PipelineState::Listening;
                        tracing::debug!(energy, "voice activity detected → LISTENING");
                    }
                }

                PipelineState::Listening => {
                    let window = self
                        .capture
                        .read_last_seconds(VAD_WINDOW_SECONDS, self.sample_rate);
                    let energy = audio::rms_energy(&window);

                    if energy <= VAD_ENERGY_THRESHOLD {
                        silence_counter += 1;
                        if silence_counter >= silence_limit {
                            state = PipelineState::Processing;
                            tracing::debug!("silence timeout → PROCESSING");
                        }
                    } else {
                        silence_counter = 0;
                    }
                }

                PipelineState::Processing => {
                    let utterance = self.capture.read_since(utterance_start_pos);
                    if utterance.is_empty() {
                        state = PipelineState::Idle;
                        continue;
                    }

                    self.busy.store(true, Ordering::Relaxed);

                    if let Err(e) = self.process_utterance(&utterance).await {
                        error!("utterance processing failed: {e}");
                    }

                    self.busy.store(false, Ordering::Relaxed);
                    state = PipelineState::Idle;
                }
            }
        }
    }

    /// Process a captured utterance through the two-tier pipeline.
    async fn process_utterance(&self, utterance: &[f32]) -> Result<(), VoiceError> {
        // Tier 1: SDR fast path
        if let Some(cmd) = self.sdr_registry.match_command(utterance, &self.mel) {
            info!(label = %cmd.label, "SDR fast-path hit");
            return self.execute_command_response(&cmd.response).await;
        }

        // Tier 2: Full STT → Odin → TTS
        // Play busy sound if configured (non-blocking)
        if let Some(busy_path) = &self.busy_sound {
            let path = busy_path.clone();
            let sr = self.tts.sample_rate();
            tokio::task::spawn_blocking(move || {
                let player = AudioPlayer::new(sr);
                if let Ok(p) = player
                    && let Err(e) = p.play_wav(&path) {
                        warn!("failed to play busy sound: {e}");
                    }
            });
        }

        // STT
        let audio_clone = utterance.to_vec();
        let stt = Arc::clone(&self.stt);
        let transcript = stt.transcribe_async(audio_clone).await?;

        if transcript.is_empty() {
            tracing::debug!("empty transcript — ignoring");
            return Ok(());
        }

        info!(transcript = %transcript, "STT result");

        // Check for wake word
        if !transcript
            .to_lowercase()
            .contains(&self.wake_word.to_lowercase())
        {
            tracing::debug!(
                wake_word = %self.wake_word,
                "wake word not found in transcript — ignoring"
            );
            return Ok(());
        }

        // Strip wake word from the command text
        let command_text = transcript
            .to_lowercase()
            .replace(&self.wake_word.to_lowercase(), "")
            .trim()
            .to_string();

        if command_text.is_empty() {
            // Just the wake word with no command
            let response = "I'm listening. What can I help you with?";
            return self.speak(response).await;
        }

        // Route through Odin for intent processing
        let response_text = self.process_intent(&command_text).await?;
        info!(response = %response_text, "Odin response");

        // TTS
        self.speak(&response_text).await
    }

    /// Speak a text response via Kokoro TTS + audio playback.
    async fn speak(&self, text: &str) -> Result<(), VoiceError> {
        let tts = Arc::clone(&self.tts);
        let text_owned = text.to_string();
        let audio = tts.synthesize_async(text_owned).await?;

        if audio.is_empty() {
            warn!("TTS produced no audio");
            return Ok(());
        }

        let sr = self.tts.sample_rate();
        tokio::task::spawn_blocking(move || {
            let player = AudioPlayer::new(sr)?;
            player.play_samples(&audio, sr)
        })
        .await
        .map_err(|e| VoiceError::Audio(format!("playback task panicked: {e}")))?
    }

    /// Execute an SDR command response.
    async fn execute_command_response(
        &self,
        response: &CommandResponse,
    ) -> Result<(), VoiceError> {
        match response {
            CommandResponse::CannedAudio { path } => {
                let path = path.clone();
                let sr = self.tts.sample_rate();
                tokio::task::spawn_blocking(move || {
                    let player = AudioPlayer::new(sr)?;
                    player.play_wav(&path)
                })
                .await
                .map_err(|e| VoiceError::Audio(format!("canned audio task panicked: {e}")))?
            }

            CommandResponse::HaAction {
                entity_id,
                service,
            } => {
                info!(entity_id, service, "executing HA action via Odin");
                let command = format!("{service} {entity_id}");
                let _response = self.process_intent(&command).await?;
                Ok(())
            }

            CommandResponse::OdinIntent { text } => {
                let response = self.process_intent(text).await?;
                self.speak(&response).await
            }
        }
    }

    /// Process text through Odin for intent routing.
    async fn process_intent(&self, text: &str) -> Result<String, VoiceError> {
        let payload = serde_json::json!({
            "model": "default",
            "messages": [
                {"role": "user", "content": text}
            ]
        });

        let resp = self
            .client
            .post(format!("{}/api/v1/chat", self.odin_url))
            .json(&payload)
            .send()
            .await
            .map_err(|e| VoiceError::Network(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(VoiceError::OdinError { status, body });
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| VoiceError::Network(e.to_string()))?;

        let response = body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("I couldn't process that request.")
            .to_string();

        Ok(response)
    }
}

/// Voice pipeline errors.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("network error: {0}")]
    Network(String),

    #[error("Odin error (HTTP {status}): {body}")]
    OdinError { status: u16, body: String },

    #[error("audio error: {0}")]
    Audio(String),

    #[error("STT error: {0}")]
    Stt(String),

    #[error("TTS error: {0}")]
    Tts(String),

    #[error("model load error: {0}")]
    ModelLoad(String),
}
