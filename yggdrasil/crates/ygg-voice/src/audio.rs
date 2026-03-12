//! Audio capture and playback via cpal.
//!
//! Provides continuous 16kHz mono audio capture into a ring buffer
//! and playback for TTS output and canned sound files.

use std::path::Path;
use std::sync::{Arc, RwLock};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, StreamConfig};
use tracing::{error, info};

use crate::VoiceError;

/// Circular ring buffer for audio samples.
pub struct RingBuffer {
    data: Vec<f32>,
    write_pos: usize,
    capacity: usize,
    total_written: usize,
}

impl RingBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            data: vec![0.0; capacity],
            write_pos: 0,
            capacity,
            total_written: 0,
        }
    }

    fn write(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.data[self.write_pos] = sample;
            self.write_pos = (self.write_pos + 1) % self.capacity;
        }
        self.total_written += samples.len();
    }

    /// Read the last `n` samples from the ring buffer.
    /// Returns fewer samples if the buffer hasn't been filled that much yet.
    pub fn read_last_n(&self, n: usize) -> Vec<f32> {
        let available = n.min(self.total_written).min(self.capacity);
        let mut out = Vec::with_capacity(available);

        let start = if self.write_pos >= available {
            self.write_pos - available
        } else {
            self.capacity - (available - self.write_pos)
        };

        for i in 0..available {
            out.push(self.data[(start + i) % self.capacity]);
        }
        out
    }

    /// Get the current write position (for marking utterance start).
    pub fn position(&self) -> usize {
        self.total_written
    }

    /// Read samples written since a given position.
    pub fn read_since(&self, since_position: usize) -> Vec<f32> {
        if since_position >= self.total_written {
            return Vec::new();
        }
        let count = self.total_written - since_position;
        self.read_last_n(count)
    }
}

/// Continuous audio capture from a microphone.
pub struct AudioCapture {
    _stream: Stream,
    buffer: Arc<RwLock<RingBuffer>>,
}

// SAFETY: cpal::Stream is a handle to an OS audio stream. The underlying
// implementation is thread-safe (callbacks run on a separate OS thread and
// communicate via Arc). The raw pointer in Stream is an opaque platform handle
// that is safe to move between threads.
unsafe impl Send for AudioCapture {}
unsafe impl Sync for AudioCapture {}

impl AudioCapture {
    /// Start capturing audio from the specified device at the given sample rate.
    ///
    /// `device_name`: ALSA device name (e.g., "default") or empty for system default.
    /// `sample_rate`: Target sample rate (typically 16000 for Whisper).
    /// `buffer_seconds`: Ring buffer duration in seconds (default 30).
    pub fn new(
        device_name: &str,
        sample_rate: u32,
        buffer_seconds: u32,
    ) -> Result<Self, VoiceError> {
        let host = cpal::default_host();

        let device = if device_name.is_empty() || device_name == "default" {
            host.default_input_device()
                .ok_or_else(|| VoiceError::Audio("no default input device found".into()))?
        } else {
            host.input_devices()
                .map_err(|e| VoiceError::Audio(format!("failed to enumerate devices: {e}")))?
                .find(|d| {
                    d.name()
                        .map(|n| n.contains(device_name))
                        .unwrap_or(false)
                })
                .ok_or_else(|| {
                    VoiceError::Audio(format!("input device '{device_name}' not found"))
                })?
        };

        let device_name_str = device
            .name()
            .unwrap_or_else(|_| "unknown".into());
        info!(device = %device_name_str, sample_rate, "opening audio input");

        let config = StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let capacity = sample_rate as usize * buffer_seconds as usize;
        let buffer = Arc::new(RwLock::new(RingBuffer::new(capacity)));
        let buffer_writer = Arc::clone(&buffer);

        let supported = device
            .supported_input_configs()
            .map_err(|e| VoiceError::Audio(format!("failed to query supported configs: {e}")))?;

        // Log supported formats for debugging
        for cfg in supported {
            tracing::debug!(
                "supported input: channels={}, rate={}-{}, format={:?}",
                cfg.channels(),
                cfg.min_sample_rate().0,
                cfg.max_sample_rate().0,
                cfg.sample_format()
            );
        }

        let err_fn = |err: cpal::StreamError| {
            error!("audio capture stream error: {err}");
        };

        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut buf) = buffer_writer.write() {
                        buf.write(data);
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| VoiceError::Audio(format!("failed to build input stream: {e}")))?;

        stream
            .play()
            .map_err(|e| VoiceError::Audio(format!("failed to start capture: {e}")))?;

        info!("audio capture started");

        Ok(Self {
            _stream: stream,
            buffer,
        })
    }

    /// Read the last N seconds of captured audio.
    pub fn read_last_seconds(&self, seconds: f32, sample_rate: u32) -> Vec<f32> {
        let n_samples = (seconds * sample_rate as f32) as usize;
        self.buffer
            .read()
            .map(|buf| buf.read_last_n(n_samples))
            .unwrap_or_default()
    }

    /// Get the current buffer position (for marking utterance boundaries).
    pub fn position(&self) -> usize {
        self.buffer
            .read()
            .map(|buf| buf.position())
            .unwrap_or(0)
    }

    /// Read samples captured since the given position.
    pub fn read_since(&self, position: usize) -> Vec<f32> {
        self.buffer
            .read()
            .map(|buf| buf.read_since(position))
            .unwrap_or_default()
    }
}

/// Audio playback for TTS output and canned sounds.
pub struct AudioPlayer {
    device: cpal::Device,
    _sample_rate: u32,
}

impl AudioPlayer {
    /// Create a player using the default output device.
    pub fn new(sample_rate: u32) -> Result<Self, VoiceError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| VoiceError::Audio("no default output device found".into()))?;

        let name = device.name().unwrap_or_else(|_| "unknown".into());
        info!(device = %name, sample_rate, "audio player initialized");

        Ok(Self {
            device,
            _sample_rate: sample_rate,
        })
    }

    /// Play f32 audio samples at the configured sample rate.
    /// Blocks until playback completes.
    pub fn play_samples(&self, samples: &[f32], sample_rate: u32) -> Result<(), VoiceError> {
        if samples.is_empty() {
            return Ok(());
        }

        let config = StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let data = Arc::new(samples.to_vec());
        let pos = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let data_clone = Arc::clone(&data);
        let pos_clone = Arc::clone(&pos);
        let done_clone = Arc::clone(&done);

        let stream = self
            .device
            .build_output_stream(
                &config,
                move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let current = pos_clone.load(std::sync::atomic::Ordering::Relaxed);
                    for (i, sample) in output.iter_mut().enumerate() {
                        let idx = current + i;
                        if idx < data_clone.len() {
                            *sample = data_clone[idx];
                        } else {
                            *sample = 0.0;
                        }
                    }
                    let new_pos = current + output.len();
                    pos_clone.store(new_pos, std::sync::atomic::Ordering::Relaxed);
                    if new_pos >= data_clone.len() {
                        done_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                },
                |err| error!("playback error: {err}"),
                None,
            )
            .map_err(|e| VoiceError::Audio(format!("failed to build output stream: {e}")))?;

        stream
            .play()
            .map_err(|e| VoiceError::Audio(format!("failed to start playback: {e}")))?;

        // Wait for playback to complete
        while !done.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Small tail to ensure the last buffer is flushed
        std::thread::sleep(std::time::Duration::from_millis(50));

        Ok(())
    }

    /// Load and play a WAV file. Blocks until playback completes.
    pub fn play_wav(&self, path: &Path) -> Result<(), VoiceError> {
        let reader = hound::WavReader::open(path)
            .map_err(|e| VoiceError::Audio(format!("failed to open WAV {}: {e}", path.display())))?;

        let spec = reader.spec();
        let wav_sr = spec.sample_rate;

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

        // If stereo, convert to mono by averaging channels
        let mono = if spec.channels > 1 {
            let ch = spec.channels as usize;
            samples
                .chunks(ch)
                .map(|frame| frame.iter().sum::<f32>() / ch as f32)
                .collect()
        } else {
            samples
        };

        self.play_samples(&mono, wav_sr)
    }
}

/// Compute RMS energy of an audio buffer.
pub fn rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_basic() {
        let mut rb = RingBuffer::new(10);
        rb.write(&[1.0, 2.0, 3.0]);
        let out = rb.read_last_n(3);
        assert_eq!(out, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn ring_buffer_wrap_around() {
        let mut rb = RingBuffer::new(4);
        rb.write(&[1.0, 2.0, 3.0, 4.0]);
        rb.write(&[5.0, 6.0]);
        let out = rb.read_last_n(4);
        assert_eq!(out, vec![3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn ring_buffer_read_since() {
        let mut rb = RingBuffer::new(100);
        rb.write(&[1.0, 2.0, 3.0]);
        let pos = rb.position();
        rb.write(&[4.0, 5.0]);
        let since = rb.read_since(pos);
        assert_eq!(since, vec![4.0, 5.0]);
    }

    #[test]
    fn rms_energy_silence() {
        assert_eq!(rms_energy(&[0.0; 100]), 0.0);
    }

    #[test]
    fn rms_energy_nonzero() {
        let e = rms_energy(&[1.0; 100]);
        assert!((e - 1.0).abs() < 1e-6);
    }
}
