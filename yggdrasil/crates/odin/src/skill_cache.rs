/// SDR-based skill cache for instant tool dispatch from raw audio.
///
/// When a voice command successfully triggers a tool call, the raw PCM audio
/// is fingerprinted into a 256-bit SDR (via Mel spectrogram → SHA-256) and
/// cached alongside the tool name and arguments.
///
/// On subsequent utterances, the audio SDR is computed directly from the PCM
/// buffer (~1ms, pure CPU) and matched against cached skills via Hamming
/// similarity. A cache hit skips LLM inference entirely.
///
/// This follows the same pattern as `ygg-voice::sdr_commands::SdrCommandRegistry`
/// but learns dynamically from successful tool calls instead of pre-registered commands.
use std::sync::Arc;
use std::time::Instant;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use ygg_domain::sdr::{self, Sdr};

/// Minimum Hamming similarity for a cache hit.
const DEFAULT_THRESHOLD: f64 = 0.85;
/// Maximum number of cached skills. LRU eviction when this cap is reached.
const MAX_SKILLS: usize = 512;

// Mel spectrogram constants (matching ygg-voice/mel.rs Whisper params).
const SAMPLE_RATE: usize = 16_000;
const FFT_SIZE: usize = 400;
const HOP_LENGTH: usize = 160;
const MEL_BINS: usize = 80;
const FREQ_BINS: usize = FFT_SIZE / 2 + 1;
/// Fingerprint window: first 2 seconds of audio.
const FINGERPRINT_SAMPLES: usize = SAMPLE_RATE * 2;

/// A cached skill: audio SDR → tool call mapping.
#[derive(Debug, Clone)]
pub struct CachedSkill {
    pub sdr: Sdr,
    pub label: String,
    pub tool_name: String,
    pub tool_args: JsonValue,
    pub hit_count: u32,
    pub last_used: Instant,
}

/// Result of a skill cache lookup.
pub struct SkillMatch {
    pub tool_name: String,
    pub tool_args: JsonValue,
    pub similarity: f64,
}

/// Thread-safe skill cache with audio-SDR matching.
///
/// Pre-computes the Mel filterbank, Hann window, and FFT plan at construction time.
pub struct SkillCache {
    skills: RwLock<Vec<CachedSkill>>,
    threshold: f64,
    max_skills: usize,
    filterbank: Vec<Vec<f32>>,
    hann_window: Vec<f32>,
    /// Pre-computed FFT plan for `FFT_SIZE`. `Arc<dyn Fft>` is `Send + Sync`.
    fft_plan: Arc<dyn Fft<f32>>,
}

impl Default for SkillCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillCache {
    pub fn new() -> Self {
        Self::with_max_skills(MAX_SKILLS)
    }

    fn with_max_skills(max_skills: usize) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft_plan = planner.plan_fft_forward(FFT_SIZE);
        Self {
            skills: RwLock::new(Vec::new()),
            threshold: DEFAULT_THRESHOLD,
            max_skills,
            filterbank: build_mel_filterbank(),
            hann_window: build_hann_window(),
            fft_plan,
        }
    }

    /// Compute a 256-bit SDR fingerprint from raw i16 PCM audio.
    ///
    /// Uses the first 2 seconds: Mel spectrogram → average energy → quantize → SHA-256.
    /// ~1ms on CPU, no network, no models.
    pub fn fingerprint(&self, audio: &[i16]) -> Sdr {
        // Convert i16 → f32 directly into padded (avoids a 64 KB intermediate Vec).
        let mut padded = vec![0.0f32; FINGERPRINT_SAMPLES];
        let copy_len = audio.len().min(FINGERPRINT_SAMPLES);
        for (dst, &s) in padded[..copy_len].iter_mut().zip(audio[..copy_len].iter()) {
            *dst = s as f32 / 32768.0;
        }

        let num_frames = FINGERPRINT_SAMPLES.saturating_sub(FFT_SIZE) / HOP_LENGTH + 1;
        if num_frames == 0 {
            return sdr::ZERO;
        }

        let fft = &self.fft_plan;
        let mut fft_buffer = vec![Complex::new(0.0f32, 0.0f32); FFT_SIZE];
        // Reused across frames — hoisted to avoid 200 allocations per fingerprint call.
        let mut power = vec![0.0f32; FREQ_BINS];

        // Average mel energy across all frames. Stack-allocated (MEL_BINS=80, 320 B)
        // to avoid a heap allocation in the fingerprinting hot path.
        let mut avg_mel = [0.0f32; MEL_BINS];

        for frame_idx in 0..num_frames {
            let start = frame_idx * HOP_LENGTH;
            if start + FFT_SIZE > padded.len() {
                break;
            }

            for i in 0..FFT_SIZE {
                fft_buffer[i] = Complex::new(padded[start + i] * self.hann_window[i], 0.0);
            }
            fft.process(&mut fft_buffer);

            for (k, c) in fft_buffer[..FREQ_BINS].iter().enumerate() {
                power[k] = c.norm_sqr();
            }

            for (mel_idx, filter) in self.filterbank.iter().enumerate() {
                let mut energy = 0.0f32;
                for (freq_idx, &weight) in filter.iter().enumerate() {
                    energy += weight * power[freq_idx];
                }
                avg_mel[mel_idx] += energy;
            }
        }

        // Quantize to u8 and hash.
        let scale = 1.0 / num_frames as f32;
        let mut quantized = [0u8; MEL_BINS];
        for i in 0..MEL_BINS {
            let log_val = (avg_mel[i] * scale).max(1e-10).log10();
            let normalized = ((log_val + 10.0) / 12.0).clamp(0.0, 1.0);
            quantized[i] = (normalized * 255.0) as u8;
        }

        let hash = Sha256::digest(quantized);
        sdr::from_bytes(&hash).unwrap_or(sdr::ZERO)
    }

    /// Query the cache for a matching skill.
    ///
    /// Uses a read lock for the linear scan, then a write lock only when a hit
    /// is found to update `hit_count` and `last_used`. This unblocks concurrent
    /// readers during the O(N) scan phase. Hit-count updates are best-effort.
    pub async fn match_skill(&self, query_sdr: &Sdr) -> Option<SkillMatch> {
        // Phase 1: read-only scan.
        let best = {
            let guard = self.skills.read().await;
            let mut best_idx = None;
            let mut best_sim = 0.0_f64;
            for (i, skill) in guard.iter().enumerate() {
                let sim = sdr::hamming_similarity(query_sdr, &skill.sdr);
                if sim >= self.threshold && sim > best_sim {
                    best_sim = sim;
                    best_idx = Some(i);
                }
            }
            best_idx.map(|idx| {
                let skill = &guard[idx];
                (idx, skill.sdr, skill.tool_name.clone(), skill.tool_args.clone(), best_sim)
            })
        }; // read lock dropped

        // Phase 2: write lock only on hit (best-effort hit tracking).
        // Re-verify the skill at `idx` by SDR identity — guards against a concurrent
        // `learn()` inserting/removing skills between the two lock acquisitions.
        if let Some((idx, matched_sdr, tool_name, tool_args, similarity)) = best {
            let mut guard = self.skills.write().await;
            if guard.get(idx).map(|s| s.sdr == matched_sdr).unwrap_or(false) {
                guard[idx].hit_count += 1;
                guard[idx].last_used = Instant::now();
            }
            Some(SkillMatch { tool_name, tool_args, similarity })
        } else {
            None
        }
    }

    /// Cache a new skill after a successful tool call.
    ///
    /// Two-phase: read lock for the dedup scan (unblocks concurrent `match_skill`
    /// callers), then write lock only to mutate. Final dedup re-check under write
    /// lock guards against concurrent `learn()` inserting the same skill between
    /// the two lock acquisitions.
    pub async fn learn(&self, audio_sdr: Sdr, label: String, tool_name: String, tool_args: JsonValue) {
        // Phase 1: read-only dedup scan — record the matching SDR for cheap
        // identity re-verify in Phase 2 (equality, not Hamming re-scoring).
        let found_sdr: Option<Sdr> = {
            let guard = self.skills.read().await;
            guard.iter()
                .find(|s| sdr::hamming_similarity(&audio_sdr, &s.sdr) >= self.threshold)
                .map(|s| s.sdr)
        }; // read lock dropped

        let mut guard = self.skills.write().await;

        if let Some(matched_sdr) = found_sdr {
            // Re-find by SDR identity under write lock (O(1) equality vs Hamming).
            if let Some(skill) = guard.iter_mut().find(|s| s.sdr == matched_sdr) { // exact bit equality, not Hamming
                skill.hit_count += 1;
                skill.last_used = Instant::now();
                tracing::debug!(tool = %tool_name, "skill cache: updated existing skill");
                return;
            }
            // Skill was evicted between Phase 1 and Phase 2 — fall through to insert.
        }

        // Final dedup check: guard against a concurrent learn() that inserted
        // the same skill after Phase 1 dropped the read lock.
        if guard.iter().any(|s| sdr::hamming_similarity(&audio_sdr, &s.sdr) >= self.threshold) {
            return;
        }

        // Cache at capacity — evict LRU before inserting so post-insert size stays <= max_skills.
        if guard.len() >= self.max_skills
            && let Some(lru_idx) = guard.iter().enumerate().min_by_key(|(_, s)| s.last_used).map(|(i, _)| i)
        {
            guard.swap_remove(lru_idx);
        }

        tracing::info!(
            tool = %tool_name,
            label = %label,
            total = guard.len() + 1,
            "skill cache: learned new skill from audio"
        );

        guard.push(CachedSkill {
            sdr: audio_sdr,
            label,
            tool_name,
            tool_args,
            hit_count: 1,
            last_used: Instant::now(),
        });
    }

    pub async fn len(&self) -> usize {
        self.skills.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.skills.read().await.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────
// Mel filterbank (matches ygg-voice/mel.rs exactly)
// ─────────────────────────────────────────────────────────────────

fn build_mel_filterbank() -> Vec<Vec<f32>> {
    let sr = SAMPLE_RATE as f32;
    let hz_to_mel = |hz: f32| -> f32 { 2595.0 * (1.0 + hz / 700.0).log10() };
    let mel_to_hz = |mel: f32| -> f32 { 700.0 * (10.0f32.powf(mel / 2595.0) - 1.0) };

    let mel_low = hz_to_mel(0.0);
    let mel_high = hz_to_mel(sr / 2.0);

    let n_points = MEL_BINS + 2;
    let mel_points: Vec<f32> = (0..n_points)
        .map(|i| mel_low + (mel_high - mel_low) * i as f32 / (n_points - 1) as f32)
        .collect();

    let bin_points: Vec<f32> = mel_points
        .iter()
        .map(|&m| mel_to_hz(m) * FFT_SIZE as f32 / sr)
        .collect();

    let mut filterbank = vec![vec![0.0f32; FREQ_BINS]; MEL_BINS];

    for m in 0..MEL_BINS {
        let left = bin_points[m];
        let center = bin_points[m + 1];
        let right = bin_points[m + 2];

        for (k, bin) in filterbank[m].iter_mut().enumerate() {
            let freq = k as f32;
            if freq >= left && freq <= center && center > left {
                *bin = (freq - left) / (center - left);
            } else if freq > center && freq <= right && right > center {
                *bin = (right - freq) / (right - center);
            }
        }
    }

    filterbank
}

fn build_hann_window() -> Vec<f32> {
    (0..FFT_SIZE)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_on_empty_cache() {
        let cache = SkillCache::new();
        let silence = vec![0i16; 16000];
        let sdr = cache.fingerprint(&silence);
        assert!(cache.match_skill(&sdr).await.is_none());
    }

    #[tokio::test]
    async fn exact_audio_matches() {
        let cache = SkillCache::new();
        let tone: Vec<i16> = (0..16000)
            .map(|i| ((2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0).sin() * 16000.0) as i16)
            .collect();

        let sdr_val = cache.fingerprint(&tone);
        cache.learn(
            sdr_val,
            "test tone".into(),
            "gaming".into(),
            serde_json::json!({"action": "launch", "vm_name": "harpy"}),
        ).await;

        let query_sdr = cache.fingerprint(&tone);
        let result = cache.match_skill(&query_sdr).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().tool_name, "gaming");
    }

    #[tokio::test]
    async fn different_audio_does_not_match() {
        let cache = SkillCache::new();
        let tone: Vec<i16> = (0..16000)
            .map(|i| ((2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0).sin() * 16000.0) as i16)
            .collect();

        let sdr_val = cache.fingerprint(&tone);
        cache.learn(sdr_val, "tone".into(), "gaming".into(), serde_json::json!({})).await;

        // Very different audio (silence).
        let silence = vec![0i16; 16000];
        let query_sdr = cache.fingerprint(&silence);
        assert!(cache.match_skill(&query_sdr).await.is_none());
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let cache = SkillCache::new();
        let audio: Vec<i16> = (0..16000).map(|i| (i % 1000) as i16).collect();
        let sdr1 = cache.fingerprint(&audio);
        let sdr2 = cache.fingerprint(&audio);
        assert_eq!(sdr1, sdr2);
    }

    /// Verify that a skill evicted by LRU is correctly re-inserted on the next learn()
    /// call, rather than silently dropped (the eviction-race fix).
    ///
    /// Determinism: `sdr_a` is always the LRU when `sdr_c` triggers eviction because
    /// `Instant::now()` is monotonic — a was inserted before b, so a.last_used < b.last_used.
    #[tokio::test]
    async fn evicted_skill_is_relearned() {
        // Use well-separated frequencies to guarantee distinct SDRs (no Hamming collision).
        let make_tone = |freq: f32| -> Vec<i16> {
            (0..16000)
                .map(|i| ((2.0 * std::f32::consts::PI * freq * i as f32 / 16000.0).sin() * 16000.0) as i16)
                .collect()
        };

        // Use a tiny capacity of 2 to trigger eviction without filling 512 slots.
        let cache = SkillCache::with_max_skills(2);

        let tone_a = make_tone(200.0);
        let tone_b = make_tone(2000.0);
        let tone_c = make_tone(440.0);
        let sdr_a = cache.fingerprint(&tone_a);
        let sdr_b = cache.fingerprint(&tone_b);
        let sdr_c = cache.fingerprint(&tone_c);

        // Fill cache: a is inserted first → a.last_used < b.last_used → a is the LRU.
        cache.learn(sdr_a, "a".into(), "tool_a".into(), serde_json::json!({})).await;
        cache.learn(sdr_b, "b".into(), "tool_b".into(), serde_json::json!({})).await;
        assert_eq!(cache.len().await, 2);

        // Inserting c evicts a (the LRU). Cache now holds {b, c}.
        cache.learn(sdr_c, "c".into(), "tool_c".into(), serde_json::json!({})).await;
        assert_eq!(cache.len().await, 2);
        assert!(cache.match_skill(&sdr_c).await.is_some(), "c must be cached");
        assert!(cache.match_skill(&sdr_b).await.is_some(), "b must still be cached");

        // Re-learn a — it was evicted, so it must be re-inserted (not silently dropped).
        cache.learn(sdr_a, "a".into(), "tool_a".into(), serde_json::json!({})).await;
        assert_eq!(cache.len().await, 2);
        let hit = cache.match_skill(&sdr_a).await;
        assert!(hit.is_some(), "evicted skill a must be re-insertable after LRU eviction");
        assert_eq!(hit.unwrap().tool_name, "tool_a");
    }

    /// Verify LRU eviction works at the minimum valid capacity (1 slot).
    /// Every insert after the first must evict the previous occupant.
    #[tokio::test]
    async fn eviction_at_capacity_one() {
        let make_tone = |freq: f32| -> Vec<i16> {
            (0..16000)
                .map(|i| ((2.0 * std::f32::consts::PI * freq * i as f32 / 16000.0).sin() * 16000.0) as i16)
                .collect()
        };

        let cache = SkillCache::with_max_skills(1);
        let tone_a = make_tone(300.0);
        let tone_b = make_tone(3000.0);
        let sdr_a = cache.fingerprint(&tone_a);
        let sdr_b = cache.fingerprint(&tone_b);

        cache.learn(sdr_a, "a".into(), "tool_a".into(), serde_json::json!({})).await;
        assert_eq!(cache.len().await, 1);

        // Inserting b must evict a so capacity stays at 1.
        cache.learn(sdr_b, "b".into(), "tool_b".into(), serde_json::json!({})).await;
        assert_eq!(cache.len().await, 1, "capacity must not exceed 1");
        assert!(cache.match_skill(&sdr_b).await.is_some(), "b must be cached");
        assert!(cache.match_skill(&sdr_a).await.is_none(), "a must have been evicted");
    }
}
