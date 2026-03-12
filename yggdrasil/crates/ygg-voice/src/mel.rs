//! Mel spectrogram computation for Whisper-compatible audio preprocessing.
//!
//! Converts raw f32 PCM audio at 16kHz into an 80-bin log-mel spectrogram
//! matching OpenAI Whisper's expected input format.
//!
//! Parameters (hardcoded to match Whisper):
//! - Sample rate: 16000 Hz
//! - FFT size: 400 (25ms window)
//! - Hop length: 160 (10ms hop)
//! - Mel bins: 80
//! - Window: Hann
//! - Max audio length: 30 seconds (480000 samples)

use ndarray::Array2;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use sha2::{Digest, Sha256};

/// Whisper audio parameters.
pub const SAMPLE_RATE: u32 = 16_000;
pub const FFT_SIZE: usize = 400;
pub const HOP_LENGTH: usize = 160;
pub const MEL_BINS: usize = 80;
pub const MAX_AUDIO_SECONDS: usize = 30;
pub const MAX_AUDIO_SAMPLES: usize = SAMPLE_RATE as usize * MAX_AUDIO_SECONDS;
/// Number of frequency bins in the power spectrum (FFT_SIZE / 2 + 1).
const FREQ_BINS: usize = FFT_SIZE / 2 + 1; // 201

/// Pre-computed mel filterbank and Hann window for efficient spectrogram computation.
pub struct MelSpectrogram {
    filterbank: Vec<Vec<f32>>, // [MEL_BINS x FREQ_BINS]
    hann_window: Vec<f32>,     // [FFT_SIZE]
}

impl MelSpectrogram {
    /// Create a new MelSpectrogram with pre-computed filterbank and window.
    pub fn new() -> Self {
        Self {
            filterbank: build_mel_filterbank(MEL_BINS, FFT_SIZE, SAMPLE_RATE),
            hann_window: build_hann_window(FFT_SIZE),
        }
    }

    /// Compute a Whisper-compatible log-mel spectrogram from raw audio.
    ///
    /// Input: f32 PCM samples at 16kHz.
    /// Output: `[MEL_BINS, num_frames]` log-mel spectrogram where
    ///         `num_frames = (padded_len - FFT_SIZE) / HOP_LENGTH + 1`.
    ///
    /// Audio is zero-padded to 30 seconds if shorter, or truncated if longer.
    pub fn compute(&self, audio: &[f32]) -> Array2<f32> {
        // Pad or truncate to exactly 30 seconds
        let mut padded = vec![0.0f32; MAX_AUDIO_SAMPLES];
        let copy_len = audio.len().min(MAX_AUDIO_SAMPLES);
        padded[..copy_len].copy_from_slice(&audio[..copy_len]);

        let num_frames = (MAX_AUDIO_SAMPLES - FFT_SIZE) / HOP_LENGTH + 1;

        // Plan FFT
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        let mut mel_spec = Array2::<f32>::zeros((MEL_BINS, num_frames));
        let mut fft_buffer = vec![Complex::new(0.0f32, 0.0f32); FFT_SIZE];

        for frame_idx in 0..num_frames {
            let start = frame_idx * HOP_LENGTH;

            // Apply Hann window and load into complex buffer
            for i in 0..FFT_SIZE {
                fft_buffer[i] = Complex::new(padded[start + i] * self.hann_window[i], 0.0);
            }

            // FFT in-place
            fft.process(&mut fft_buffer);

            // Power spectrum (only first FREQ_BINS = FFT_SIZE/2 + 1)
            let power: Vec<f32> = fft_buffer[..FREQ_BINS]
                .iter()
                .map(|c| c.norm_sqr())
                .collect();

            // Apply mel filterbank
            for (mel_idx, filter) in self.filterbank.iter().enumerate() {
                let mut energy = 0.0f32;
                for (freq_idx, &weight) in filter.iter().enumerate() {
                    energy += weight * power[freq_idx];
                }
                // Log scale with floor to avoid log(0)
                mel_spec[[mel_idx, frame_idx]] = (energy.max(1e-10)).log10();
            }
        }

        mel_spec
    }

    /// Compute a short mel fingerprint from the first ~2 seconds of audio,
    /// then hash it to a 256-bit SDR for fast command matching.
    pub fn fingerprint_sdr(&self, audio: &[f32]) -> ygg_domain::sdr::Sdr {
        let two_seconds = (SAMPLE_RATE as usize) * 2;
        let clip = &audio[..audio.len().min(two_seconds)];

        // Pad to 2 seconds
        let mut padded = vec![0.0f32; two_seconds];
        padded[..clip.len()].copy_from_slice(clip);

        let num_frames = (two_seconds.saturating_sub(FFT_SIZE)) / HOP_LENGTH + 1;
        if num_frames == 0 {
            return ygg_domain::sdr::ZERO;
        }

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let mut fft_buffer = vec![Complex::new(0.0f32, 0.0f32); FFT_SIZE];

        // Average mel energy across all frames
        let mut avg_mel = vec![0.0f32; MEL_BINS];

        for frame_idx in 0..num_frames {
            let start = frame_idx * HOP_LENGTH;
            if start + FFT_SIZE > padded.len() {
                break;
            }

            for i in 0..FFT_SIZE {
                fft_buffer[i] = Complex::new(padded[start + i] * self.hann_window[i], 0.0);
            }
            fft.process(&mut fft_buffer);

            let power: Vec<f32> = fft_buffer[..FREQ_BINS]
                .iter()
                .map(|c| c.norm_sqr())
                .collect();

            for (mel_idx, filter) in self.filterbank.iter().enumerate() {
                let mut energy = 0.0f32;
                for (freq_idx, &weight) in filter.iter().enumerate() {
                    energy += weight * power[freq_idx];
                }
                avg_mel[mel_idx] += energy;
            }
        }

        // Average and quantize to u8
        let scale = 1.0 / num_frames as f32;
        let mut quantized = vec![0u8; MEL_BINS];
        for i in 0..MEL_BINS {
            let log_val = (avg_mel[i] * scale).max(1e-10).log10();
            // Map typical range [-10, 2] to [0, 255]
            let normalized = ((log_val + 10.0) / 12.0).clamp(0.0, 1.0);
            quantized[i] = (normalized * 255.0) as u8;
        }

        // SHA-256 hash of the quantized mel vector → 32 bytes = 256 bits = Sdr
        let hash = Sha256::digest(&quantized);
        ygg_domain::sdr::from_bytes(&hash).unwrap_or(ygg_domain::sdr::ZERO)
    }
}

/// Build triangular mel filterbank matching Whisper/librosa defaults.
fn build_mel_filterbank(n_mels: usize, fft_size: usize, sample_rate: u32) -> Vec<Vec<f32>> {
    let n_freqs = fft_size / 2 + 1;
    let sr = sample_rate as f32;

    // Mel scale conversion helpers
    let hz_to_mel = |hz: f32| -> f32 { 2595.0 * (1.0 + hz / 700.0).log10() };
    let mel_to_hz = |mel: f32| -> f32 { 700.0 * (10.0f32.powf(mel / 2595.0) - 1.0) };

    let mel_low = hz_to_mel(0.0);
    let mel_high = hz_to_mel(sr / 2.0);

    // n_mels + 2 equally spaced points in mel scale
    let n_points = n_mels + 2;
    let mel_points: Vec<f32> = (0..n_points)
        .map(|i| mel_low + (mel_high - mel_low) * i as f32 / (n_points - 1) as f32)
        .collect();

    // Convert mel points back to Hz, then to FFT bin indices
    let hz_points: Vec<f32> = mel_points.iter().map(|&m| mel_to_hz(m)).collect();
    let bin_points: Vec<f32> = hz_points
        .iter()
        .map(|&hz| hz * fft_size as f32 / sr)
        .collect();

    let mut filterbank = vec![vec![0.0f32; n_freqs]; n_mels];

    for m in 0..n_mels {
        let left = bin_points[m];
        let center = bin_points[m + 1];
        let right = bin_points[m + 2];

        for k in 0..n_freqs {
            let freq = k as f32;
            if freq >= left && freq <= center && center > left {
                filterbank[m][k] = (freq - left) / (center - left);
            } else if freq > center && freq <= right && right > center {
                filterbank[m][k] = (right - freq) / (right - center);
            }
        }
    }

    filterbank
}

/// Build a Hann window of the given size.
fn build_hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / size as f32).cos())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mel_filterbank_shape() {
        let fb = build_mel_filterbank(MEL_BINS, FFT_SIZE, SAMPLE_RATE);
        assert_eq!(fb.len(), MEL_BINS);
        assert_eq!(fb[0].len(), FREQ_BINS);
    }

    #[test]
    fn mel_filterbank_non_negative() {
        let fb = build_mel_filterbank(MEL_BINS, FFT_SIZE, SAMPLE_RATE);
        for filter in &fb {
            for &val in filter {
                assert!(val >= 0.0, "filterbank values must be non-negative");
            }
        }
    }

    #[test]
    fn hann_window_endpoints() {
        let w = build_hann_window(FFT_SIZE);
        assert_eq!(w.len(), FFT_SIZE);
        // Hann window starts at 0
        assert!(w[0].abs() < 1e-6);
    }

    #[test]
    fn compute_mel_output_shape() {
        let mel = MelSpectrogram::new();
        // 1 second of silence
        let audio = vec![0.0f32; SAMPLE_RATE as usize];
        let spec = mel.compute(&audio);
        let expected_frames = (MAX_AUDIO_SAMPLES - FFT_SIZE) / HOP_LENGTH + 1;
        assert_eq!(spec.shape(), &[MEL_BINS, expected_frames]);
    }

    #[test]
    fn fingerprint_sdr_deterministic() {
        let mel = MelSpectrogram::new();
        let audio = vec![0.5f32; SAMPLE_RATE as usize];
        let sdr1 = mel.fingerprint_sdr(&audio);
        let sdr2 = mel.fingerprint_sdr(&audio);
        assert_eq!(sdr1, sdr2);
    }

    #[test]
    fn fingerprint_sdr_different_for_different_audio() {
        let mel = MelSpectrogram::new();
        let silence = vec![0.0f32; SAMPLE_RATE as usize];
        let tone: Vec<f32> = (0..SAMPLE_RATE as usize)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SAMPLE_RATE as f32).sin())
            .collect();
        let sdr_silence = mel.fingerprint_sdr(&silence);
        let sdr_tone = mel.fingerprint_sdr(&tone);
        // They should be different (low similarity)
        let sim = ygg_domain::sdr::hamming_similarity(&sdr_silence, &sdr_tone);
        assert!(sim < 0.9, "different audio should produce different SDRs, got similarity {sim}");
    }
}
