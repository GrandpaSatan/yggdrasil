/// SDR-based wake word detection with multi-user enrollment.
///
/// Each enrolled user provides 1+ audio samples of themselves saying the wake
/// word ("Fergus"). The audio is fingerprinted into a 256-bit SDR via Mel
/// spectrogram → SHA-256 (same algorithm as `SkillCache::fingerprint`).
///
/// At runtime, incoming audio is fingerprinted and compared against all
/// enrolled user SDRs via Hamming similarity. A match above the threshold
/// identifies the speaker and gates the voice pipeline — no STT needed.
///
/// Enrollment data is persisted to a JSON file on disk so it survives restarts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use ygg_domain::sdr::{self, Sdr};

use crate::skill_cache::SkillCache;

/// Minimum Hamming similarity for wake word match.
const DEFAULT_THRESHOLD: f64 = 0.80;

/// A single enrollment sample for a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrolledSample {
    /// 256-bit SDR as hex string (64 chars).
    pub sdr_hex: String,
}

/// A registered user with their wake word samples.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrolledUser {
    pub user_id: String,
    pub samples: Vec<EnrolledSample>,
}

/// Result of a wake word match.
#[derive(Debug, Clone)]
pub struct WakeWordMatch {
    pub user_id: String,
    pub similarity: f64,
}

/// Persisted enrollment data.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EnrollmentData {
    users: Vec<EnrolledUser>,
}

/// Thread-safe wake word registry.
pub struct WakeWordRegistry {
    users: RwLock<Vec<EnrolledUser>>,
    threshold: f64,
    persist_path: Option<PathBuf>,
}

impl WakeWordRegistry {
    /// Create a new registry, optionally loading persisted enrollments from disk.
    pub fn new(persist_path: Option<PathBuf>) -> Self {
        let users = if let Some(ref path) = persist_path {
            load_from_disk(path).unwrap_or_default()
        } else {
            Vec::new()
        };

        let count: usize = users.iter().map(|u| u.samples.len()).sum();
        if count > 0 {
            info!(
                users = users.len(),
                samples = count,
                "wake word registry loaded"
            );
        }

        Self {
            users: RwLock::new(users),
            threshold: DEFAULT_THRESHOLD,
            persist_path,
        }
    }

    /// Enroll a wake word sample for a user. Returns the sample's SDR hex.
    ///
    /// `audio_i16` is raw PCM s16le 16kHz mono. The `skill_cache` is borrowed
    /// for its pre-computed FFT/mel infrastructure.
    pub async fn enroll(
        &self,
        user_id: &str,
        audio_i16: &[i16],
        skill_cache: &SkillCache,
    ) -> String {
        let sdr = skill_cache.fingerprint(audio_i16);
        let sdr_hex = sdr::to_hex(&sdr);

        let sample = EnrolledSample {
            sdr_hex: sdr_hex.clone(),
        };

        let mut guard = self.users.write().await;
        if let Some(user) = guard.iter_mut().find(|u| u.user_id == user_id) {
            user.samples.push(sample);
        } else {
            guard.push(EnrolledUser {
                user_id: user_id.to_string(),
                samples: vec![sample],
            });
        }

        let total: usize = guard.iter().map(|u| u.samples.len()).sum();
        info!(
            user_id,
            sdr = &sdr_hex[..16],
            total_samples = total,
            "wake word sample enrolled"
        );

        // Persist to disk.
        if let Some(ref path) = self.persist_path {
            if let Err(e) = save_to_disk(path, &guard) {
                warn!(error = %e, "failed to persist wake word enrollments");
            }
        }

        sdr_hex
    }

    /// Check if audio matches any enrolled user's wake word.
    ///
    /// Returns the best matching user if similarity exceeds threshold.
    pub async fn check(
        &self,
        audio_i16: &[i16],
        skill_cache: &SkillCache,
    ) -> Option<WakeWordMatch> {
        let query_sdr = skill_cache.fingerprint(audio_i16);

        let guard = self.users.read().await;
        let mut best: Option<WakeWordMatch> = None;
        let mut best_sim_overall = 0.0_f64;

        for user in guard.iter() {
            for sample in &user.samples {
                if let Some(ref_sdr) = parse_sdr_hex(&sample.sdr_hex) {
                    let sim = sdr::hamming_similarity(&query_sdr, &ref_sdr);
                    if sim > best_sim_overall {
                        best_sim_overall = sim;
                    }
                    if sim >= self.threshold {
                        if best.as_ref().map_or(true, |b| sim > b.similarity) {
                            best = Some(WakeWordMatch {
                                user_id: user.user_id.clone(),
                                similarity: sim,
                            });
                        }
                    }
                }
            }
        }

        if best.is_none() && !guard.is_empty() {
            tracing::info!(
                best_similarity = best_sim_overall,
                threshold = self.threshold,
                enrolled_samples = guard.iter().map(|u| u.samples.len()).sum::<usize>(),
                "wake word SDR: no match"
            );
        }

        best
    }

    /// List all enrolled users and their sample counts.
    pub async fn list_users(&self) -> Vec<(String, usize)> {
        let guard = self.users.read().await;
        guard
            .iter()
            .map(|u| (u.user_id.clone(), u.samples.len()))
            .collect()
    }

    /// Identify a speaker from any audio (not just wake word).
    /// Uses a lower threshold than wake word detection since we're matching
    /// voice characteristics across different spoken content.
    pub async fn identify(
        &self,
        audio_i16: &[i16],
        skill_cache: &SkillCache,
    ) -> Option<WakeWordMatch> {
        let query_sdr = skill_cache.fingerprint(audio_i16);
        let guard = self.users.read().await;

        let mut best: Option<WakeWordMatch> = None;
        // Lower threshold for general voice ID (voice timbre varies more across words).
        let id_threshold = self.threshold - 0.10;

        for user in guard.iter() {
            for sample in &user.samples {
                if let Some(ref_sdr) = parse_sdr_hex(&sample.sdr_hex) {
                    let sim = sdr::hamming_similarity(&query_sdr, &ref_sdr);
                    if sim >= id_threshold {
                        if best.as_ref().map_or(true, |b| sim > b.similarity) {
                            best = Some(WakeWordMatch {
                                user_id: user.user_id.clone(),
                                similarity: sim,
                            });
                        }
                    }
                }
            }
        }

        best
    }

    /// Auto-enroll a new speaker. If audio matches an existing user, return
    /// their ID. Otherwise create a new guest profile and return the new ID.
    pub async fn auto_enroll(
        &self,
        audio_i16: &[i16],
        skill_cache: &SkillCache,
    ) -> String {
        // Check if this voice matches an existing user.
        if let Some(m) = self.identify(audio_i16, skill_cache).await {
            return m.user_id;
        }

        // New voice — assign a guest ID.
        let sdr = skill_cache.fingerprint(audio_i16);
        let sdr_hex = sdr::to_hex(&sdr);

        let mut guard = self.users.write().await;

        // Find next guest number.
        let guest_num = guard
            .iter()
            .filter(|u| u.user_id.starts_with("guest_"))
            .count()
            + 1;
        let user_id = format!("guest_{guest_num}");

        guard.push(EnrolledUser {
            user_id: user_id.clone(),
            samples: vec![EnrolledSample { sdr_hex }],
        });

        info!(user_id = %user_id, "auto-enrolled new speaker");

        if let Some(ref path) = self.persist_path {
            if let Err(e) = save_to_disk(path, &guard) {
                warn!(error = %e, "failed to persist auto-enrollment");
            }
        }

        user_id
    }

    /// Remove all samples for a user.
    pub async fn remove_user(&self, user_id: &str) -> bool {
        let mut guard = self.users.write().await;
        let before = guard.len();
        guard.retain(|u| u.user_id != user_id);
        let removed = guard.len() < before;

        if removed {
            if let Some(ref path) = self.persist_path {
                if let Err(e) = save_to_disk(path, &guard) {
                    warn!(error = %e, "failed to persist after removal");
                }
            }
        }

        removed
    }
}

fn parse_sdr_hex(hex: &str) -> Option<Sdr> {
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    sdr::from_bytes(&bytes)
}

fn load_from_disk(path: &Path) -> Option<Vec<EnrolledUser>> {
    let data = std::fs::read_to_string(path).ok()?;
    let enrollment: EnrollmentData = serde_json::from_str(&data).ok()?;
    Some(enrollment.users)
}

fn save_to_disk(path: &Path, users: &[EnrolledUser]) -> Result<(), std::io::Error> {
    let data = EnrollmentData {
        users: users.to_vec(),
    };
    let json = serde_json::to_string_pretty(&data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, json)
}
