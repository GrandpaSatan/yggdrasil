//! SDR-based fast-path command matching.
//!
//! Stores pre-computed 256-bit SDRs for known voice commands and matches
//! incoming audio fingerprints against them using Hamming similarity.
//! Matching takes ~4μs for 50 commands (50 × XOR + popcount on 4×u64).

use std::path::{Path, PathBuf};

use tracing::info;
use ygg_domain::sdr::{self, Sdr};

use crate::mel::MelSpectrogram;
use crate::VoiceError;

/// What to do when a command matches.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandResponse {
    /// Play a pre-recorded WAV file.
    CannedAudio { path: PathBuf },
    /// Trigger a Home Assistant service call.
    HaAction {
        entity_id: String,
        service: String,
    },
    /// Forward fixed text to Odin for intent routing.
    OdinIntent { text: String },
}

/// A registered voice command with its SDR fingerprint.
#[derive(Debug)]
pub struct SdrCommand {
    pub sdr: Sdr,
    pub label: String,
    pub response: CommandResponse,
}

/// Configuration for a command entry (loaded from JSON).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CommandConfig {
    /// Path to a reference audio WAV file for this command.
    pub reference_audio: PathBuf,
    /// Human-readable label.
    pub label: String,
    /// Action to execute on match.
    pub response: CommandResponse,
}

/// Configuration for the SDR command registry.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SdrCommandsConfig {
    /// Hamming similarity threshold for a match (0.0–1.0, default 0.85).
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    /// Registered command definitions.
    #[serde(default)]
    pub commands: Vec<CommandConfig>,
}

fn default_threshold() -> f64 {
    0.85
}

impl Default for SdrCommandsConfig {
    fn default() -> Self {
        Self {
            threshold: default_threshold(),
            commands: Vec::new(),
        }
    }
}

/// Registry of known voice commands with SDR fingerprints.
pub struct SdrCommandRegistry {
    commands: Vec<SdrCommand>,
    threshold: f64,
}

impl SdrCommandRegistry {
    /// Create an empty registry.
    pub fn new(threshold: f64) -> Self {
        Self {
            commands: Vec::new(),
            threshold,
        }
    }

    /// Load commands from config, computing SDR fingerprints from reference audio.
    pub fn load_from_config(
        config: &SdrCommandsConfig,
        mel: &MelSpectrogram,
    ) -> Result<Self, VoiceError> {
        let mut registry = Self::new(config.threshold);

        for cmd_cfg in &config.commands {
            match load_reference_audio(&cmd_cfg.reference_audio) {
                Ok(samples) => {
                    let fingerprint = mel.fingerprint_sdr(&samples);
                    info!(
                        label = %cmd_cfg.label,
                        sdr = %sdr::to_hex(&fingerprint),
                        "registered SDR command"
                    );
                    registry.commands.push(SdrCommand {
                        sdr: fingerprint,
                        label: cmd_cfg.label.clone(),
                        response: cmd_cfg.response.clone(),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        label = %cmd_cfg.label,
                        path = %cmd_cfg.reference_audio.display(),
                        "skipping command — failed to load reference audio: {e}"
                    );
                }
            }
        }

        info!(
            count = registry.commands.len(),
            threshold = registry.threshold,
            "SDR command registry loaded"
        );
        Ok(registry)
    }

    /// Register a command manually from audio samples.
    #[allow(dead_code)]
    pub fn register(
        &mut self,
        audio: &[f32],
        label: String,
        response: CommandResponse,
        mel: &MelSpectrogram,
    ) {
        let fingerprint = mel.fingerprint_sdr(audio);
        self.commands.push(SdrCommand {
            sdr: fingerprint,
            label,
            response,
        });
    }

    /// Match incoming audio against all registered commands.
    /// Returns the best match above the threshold, or None.
    pub fn match_command(&self, audio: &[f32], mel: &MelSpectrogram) -> Option<&SdrCommand> {
        if self.commands.is_empty() {
            return None;
        }

        let query_sdr = mel.fingerprint_sdr(audio);
        let mut best: Option<(&SdrCommand, f64)> = None;

        for cmd in &self.commands {
            let sim = sdr::hamming_similarity(&query_sdr, &cmd.sdr);
            if sim >= self.threshold {
                match best {
                    Some((_, best_sim)) if sim > best_sim => {
                        best = Some((cmd, sim));
                    }
                    None => {
                        best = Some((cmd, sim));
                    }
                    _ => {}
                }
            }
        }

        if let Some((cmd, sim)) = best {
            info!(
                label = %cmd.label,
                similarity = sim,
                threshold = self.threshold,
                "SDR fast-path match"
            );
            Some(cmd)
        } else {
            None
        }
    }

    /// Number of registered commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether the registry is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// Load reference audio from a WAV file and convert to f32 mono at 16kHz.
fn load_reference_audio(path: &Path) -> Result<Vec<f32>, VoiceError> {
    let reader = hound::WavReader::open(path)
        .map_err(|e| VoiceError::Audio(format!("failed to open {}: {e}", path.display())))?;

    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .filter_map(|s| s.ok())
            .collect(),
        hound::SampleFormat::Int => {
            let max_val = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / max_val)
                .collect()
        }
    };

    // Convert to mono if stereo
    let mono = if spec.channels > 1 {
        let ch = spec.channels as usize;
        samples
            .chunks(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    } else {
        samples
    };

    Ok(mono)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_returns_none() {
        let mel = MelSpectrogram::new();
        let registry = SdrCommandRegistry::new(0.85);
        let audio = vec![0.0f32; 16000];
        assert!(registry.match_command(&audio, &mel).is_none());
    }

    #[test]
    fn self_match_succeeds() {
        let mel = MelSpectrogram::new();
        let mut registry = SdrCommandRegistry::new(0.85);

        // A 440Hz tone as a "command"
        let tone: Vec<f32> = (0..16000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0).sin())
            .collect();

        registry.register(
            &tone,
            "test_tone".into(),
            CommandResponse::OdinIntent {
                text: "test".into(),
            },
            &mel,
        );

        // Same audio should match itself
        let result = registry.match_command(&tone, &mel);
        assert!(result.is_some());
        assert_eq!(result.unwrap().label, "test_tone");
    }
}
