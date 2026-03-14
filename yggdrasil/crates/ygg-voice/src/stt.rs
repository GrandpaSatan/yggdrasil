//! Whisper ONNX speech-to-text via ort with OpenVINO ExecutionProvider.
//!
//! Runs Whisper encoder + decoder on Munin's Intel AI Boost NPU (11 TOPS).
//! Falls back to CPU if NPU is unavailable.
//!
//! Model layout (from onnx-community/whisper-base):
//!   - encoder_model.onnx: mel [1, 80, 3000] → encoder hidden states
//!   - decoder_model_merged.onnx: encoder output + decoder input IDs → logits
//!
//! Decoding: greedy argmax, stops on <|endoftext|> or max 448 tokens.

use std::path::Path;
use std::sync::{Arc, Mutex};

use ndarray::Array2;
use ort::session::Session;
use ort::value::TensorRef;
use tracing::info;

use crate::mel::MelSpectrogram;
use crate::VoiceError;

/// Whisper special token IDs (for whisper-base English).
const SOT_TOKEN: i64 = 50258; // <|startoftranscript|>
const EOT_TOKEN: i64 = 50257; // <|endoftext|>
const _TRANSLATE_TOKEN: i64 = 50358; // <|translate|>
const TRANSCRIBE_TOKEN: i64 = 50359; // <|transcribe|>
const NO_TIMESTAMPS_TOKEN: i64 = 50363; // <|notimestamps|>
const EN_TOKEN: i64 = 50259; // <|en|>
/// Max decode tokens. For voice commands we cap at 100 tokens (~50 words)
/// to limit CPU time. The decoder positional embedding limit is 448 total
/// (444 after the 4 prefix tokens), but voice utterances are short.
const MAX_DECODE_TOKENS: usize = 100;

/// Whisper STT engine using ONNX Runtime with OpenVINO EP.
pub struct WhisperStt {
    encoder: Arc<Mutex<Session>>,
    decoder: Arc<Mutex<Session>>,
    mel: Arc<MelSpectrogram>,
    /// Simple byte-level BPE vocabulary: token_id → text fragment.
    vocab: Vec<String>,
}

impl WhisperStt {
    /// Load the Whisper ONNX model from a directory.
    ///
    /// The directory must contain:
    ///   - `encoder_model.onnx`
    ///   - `decoder_model_merged.onnx`
    ///   - `vocab.json` (token ID → text mapping)
    ///
    /// `device`: preferred inference device ("NPU", "GPU", "CPU").
    /// `fallback`: fallback device if preferred is unavailable.
    pub fn load(
        model_dir: &Path,
        mel: Arc<MelSpectrogram>,
        device: &str,
        fallback: &str,
    ) -> Result<Self, VoiceError> {
        let encoder_path = model_dir.join("encoder_model.onnx");
        let decoder_path = model_dir.join("decoder_model_merged.onnx");
        let vocab_path = model_dir.join("vocab.json");

        if !encoder_path.exists() {
            return Err(VoiceError::ModelLoad(format!(
                "encoder_model.onnx not found in {}",
                model_dir.display()
            )));
        }
        if !decoder_path.exists() {
            return Err(VoiceError::ModelLoad(format!(
                "decoder_model_merged.onnx not found in {}",
                model_dir.display()
            )));
        }

        let encoder = build_session(&encoder_path, device, fallback)?;
        info!(model = %encoder_path.display(), device, "Whisper encoder loaded");

        let decoder = build_session(&decoder_path, device, fallback)?;
        info!(model = %decoder_path.display(), device, "Whisper decoder loaded");

        let vocab = load_vocab(&vocab_path)?;
        info!(vocab_size = vocab.len(), "Whisper vocabulary loaded");

        Ok(Self {
            encoder: Arc::new(Mutex::new(encoder)),
            decoder: Arc::new(Mutex::new(decoder)),
            mel,
            vocab,
        })
    }

    /// Transcribe audio samples to text (synchronous, blocking).
    ///
    /// Input: f32 PCM at 16kHz.
    /// Output: transcribed text string.
    pub fn transcribe(&self, audio: &[f32]) -> Result<String, VoiceError> {
        // Step 1: Compute mel spectrogram and pad to exactly [80, 3000] for Whisper
        let mel_spec = self.mel.compute(audio);
        let n_mels = mel_spec.shape()[0];
        let _n_frames = mel_spec.shape()[1];
        const WHISPER_FRAMES: usize = 3000;

        // Pad or truncate to exactly 3000 frames (Whisper's fixed input size)
        let mut mel_flat = mel_spec.into_raw_vec_and_offset().0;
        let target_len = n_mels * WHISPER_FRAMES;
        if mel_flat.len() < target_len {
            mel_flat.resize(target_len, 0.0);
        } else if mel_flat.len() > target_len {
            mel_flat.truncate(target_len);
        }

        // Reshape to [1, 80, 3000] for the encoder
        let mel_array =
            ndarray::Array3::from_shape_vec((1, n_mels, WHISPER_FRAMES), mel_flat)
                .map_err(|e| VoiceError::Stt(format!("mel reshape error: {e}")))?;

        // Step 2: Run encoder
        let encoder_output = {
            let mel_tensor = TensorRef::from_array_view(&mel_array)
                .map_err(|e| VoiceError::Stt(format!("mel tensor error: {e}")))?;

            let mut encoder = self
                .encoder
                .lock()
                .map_err(|_| VoiceError::Stt("encoder mutex poisoned".into()))?;

            let outputs = encoder
                .run(ort::inputs!["input_features" => mel_tensor])
                .map_err(|e| VoiceError::Stt(format!("encoder inference failed: {e}")))?;

            // Extract encoder hidden states
            let (shape, data) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| VoiceError::Stt(format!("encoder output extraction failed: {e}")))?;

            let dims: Vec<i64> = shape.iter().copied().collect();
            (dims, data.to_vec())
        };

        // Step 3: Autoregressive decoding
        let (enc_dims, enc_data) = encoder_output;
        let mut token_ids: Vec<i64> = vec![
            SOT_TOKEN,
            EN_TOKEN,
            TRANSCRIBE_TOKEN,
            NO_TIMESTAMPS_TOKEN,
        ];

        let decode_start = std::time::Instant::now();
        let mut repeat_count: usize = 0;
        let mut last_token: i64 = -1;
        for _step in 0..MAX_DECODE_TOKENS {
            let seq_len = token_ids.len();

            // Build decoder input tensors
            let decoder_input =
                Array2::from_shape_vec((1, seq_len), token_ids.clone())
                    .map_err(|e| VoiceError::Stt(format!("decoder input shape error: {e}")))?;

            let enc_shape = [
                enc_dims[0] as usize,
                enc_dims[1] as usize,
                enc_dims[2] as usize,
            ];
            let encoder_hidden = ndarray::Array3::from_shape_vec(enc_shape, enc_data.clone())
                .map_err(|e| {
                    VoiceError::Stt(format!("encoder hidden states reshape error: {e}"))
                })?;

            let decoder_input_tensor = TensorRef::from_array_view(&decoder_input)
                .map_err(|e| VoiceError::Stt(format!("decoder input tensor error: {e}")))?;
            let encoder_hidden_tensor = TensorRef::from_array_view(&encoder_hidden)
                .map_err(|e| VoiceError::Stt(format!("encoder hidden tensor error: {e}")))?;

            let logits_data = {
                let mut decoder = self
                    .decoder
                    .lock()
                    .map_err(|_| VoiceError::Stt("decoder mutex poisoned".into()))?;

                let outputs = decoder
                    .run(ort::inputs![
                        "input_ids" => decoder_input_tensor,
                        "encoder_hidden_states" => encoder_hidden_tensor,
                    ])
                    .map_err(|e| VoiceError::Stt(format!("decoder inference failed: {e}")))?;

                let (shape, data) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| {
                        VoiceError::Stt(format!("decoder output extraction failed: {e}"))
                    })?;

                let dims: Vec<i64> = shape.iter().copied().collect();
                (dims, data.to_vec())
            };

            let (logits_dims, logits) = logits_data;
            let vocab_size = *logits_dims.last().unwrap_or(&0) as usize;

            if vocab_size == 0 {
                break;
            }

            // Get logits for the last token position
            let last_pos_start = (seq_len - 1) * vocab_size;
            let last_logits = &logits[last_pos_start..last_pos_start + vocab_size];

            // Greedy argmax
            let next_token = last_logits
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(idx, _)| idx as i64)
                .unwrap_or(EOT_TOKEN);

            if next_token == EOT_TOKEN {
                break;
            }

            // Detect repetition loops (e.g., "that that that...")
            if next_token == last_token {
                repeat_count += 1;
                if repeat_count >= 3 {
                    info!(
                        token_id = next_token,
                        repeats = repeat_count,
                        "breaking decode loop — repetition detected"
                    );
                    break;
                }
            } else {
                repeat_count = 0;
            }
            last_token = next_token;

            token_ids.push(next_token);
        }

        // Step 4: Decode token IDs to text
        let decode_tokens = token_ids.len() - 4; // Exclude prefix tokens
        let decode_elapsed = decode_start.elapsed();
        info!(
            tokens = decode_tokens,
            elapsed_ms = decode_elapsed.as_millis() as u64,
            ms_per_token = if decode_tokens > 0 { decode_elapsed.as_millis() as u64 / decode_tokens as u64 } else { 0 },
            "STT decode complete"
        );
        let text = self.decode_tokens(&token_ids[4..]); // Skip SOT, EN, TRANSCRIBE, NO_TIMESTAMPS
        Ok(text.trim().to_string())
    }

    /// Transcribe audio asynchronously via spawn_blocking.
    pub async fn transcribe_async(&self, audio: Vec<f32>) -> Result<String, VoiceError> {
        let stt = WhisperSttHandle {
            encoder: Arc::clone(&self.encoder),
            decoder: Arc::clone(&self.decoder),
            mel: Arc::clone(&self.mel),
            vocab: self.vocab.clone(),
        };
        tokio::task::spawn_blocking(move || {
            let inner = WhisperStt {
                encoder: stt.encoder,
                decoder: stt.decoder,
                mel: stt.mel,
                vocab: stt.vocab,
            };
            inner.transcribe(&audio)
        })
        .await
        .map_err(|e| VoiceError::Stt(format!("transcribe task panicked: {e}")))?
    }

    /// Decode a sequence of token IDs to a text string.
    fn decode_tokens(&self, tokens: &[i64]) -> String {
        let mut text = String::new();
        for &token_id in tokens {
            let idx = token_id as usize;
            if idx < self.vocab.len() {
                text.push_str(&self.vocab[idx]);
            }
        }
        // Clean up byte-level BPE artifacts
        text.replace("Ġ", " ").replace("Ċ", "\n")
    }
}

/// Cloneable handle for async usage.
struct WhisperSttHandle {
    encoder: Arc<Mutex<Session>>,
    decoder: Arc<Mutex<Session>>,
    mel: Arc<MelSpectrogram>,
    vocab: Vec<String>,
}

/// Build an ONNX Runtime session with OpenVINO EP targeting the specified device.
///
/// Attempts to register the OpenVINO EP for NPU/GPU acceleration. Falls back
/// through the fallback device to CPU if the EP is unavailable at runtime.
pub(crate) fn build_session(model_path: &Path, device: &str, fallback: &str) -> Result<Session, VoiceError> {
    let builder = Session::builder()
        .map_err(|e| VoiceError::ModelLoad(format!("session builder error: {e}")))?;

    // Attempt OpenVINO EP if device is NPU or GPU
    let builder = if device == "NPU" || device == "GPU" {
        match builder.with_execution_providers([
            ort::ep::OpenVINO::default()
                .with_device_type(device)
                .build(),
        ]) {
            Ok(b) => {
                info!(device, "OpenVINO EP registered");
                b
            }
            Err(e) => {
                tracing::warn!(
                    device,
                    fallback,
                    error = %e,
                    "OpenVINO EP unavailable — falling back"
                );
                // Try fallback device if it is not CPU
                if fallback != "CPU" {
                    match e.recover().with_execution_providers([
                        ort::ep::OpenVINO::default()
                            .with_device_type(fallback)
                            .build(),
                    ]) {
                        Ok(b) => b,
                        Err(e2) => {
                            tracing::warn!("fallback EP also unavailable, using CPU: {e2}");
                            e2.recover()
                        }
                    }
                } else {
                    e.recover()
                }
            }
        }
    } else {
        builder
    };

    let session = builder
        .with_intra_threads(4)
        .unwrap_or_else(|e| {
            tracing::warn!("failed to set intra threads: {e}, using default");
            e.recover()
        })
        .commit_from_file(model_path)
        .map_err(|e| {
            VoiceError::ModelLoad(format!(
                "failed to load model {}: {e}",
                model_path.display()
            ))
        })?;

    info!(
        model = %model_path.display(),
        preferred_device = device,
        fallback_device = fallback,
        "ONNX session created"
    );

    Ok(session)
}

/// Load the Whisper vocabulary from vocab.json.
///
/// vocab.json is a JSON object mapping string token IDs to text fragments:
/// { "0": "!", "1": "\"", ... }
fn load_vocab(path: &Path) -> Result<Vec<String>, VoiceError> {
    if !path.exists() {
        // Return a minimal placeholder vocab if vocab.json is missing.
        // The model will still run but output will be token IDs.
        tracing::warn!(
            path = %path.display(),
            "vocab.json not found — transcription will use raw token IDs"
        );
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path)
        .map_err(|e| VoiceError::ModelLoad(format!("failed to read vocab.json: {e}")))?;

    let map: std::collections::HashMap<String, String> = serde_json::from_str(&content)
        .map_err(|e| VoiceError::ModelLoad(format!("failed to parse vocab.json: {e}")))?;

    // Find the maximum token ID to size the vector
    let max_id = map
        .keys()
        .filter_map(|k| k.parse::<usize>().ok())
        .max()
        .unwrap_or(0);

    let mut vocab = vec![String::new(); max_id + 1];
    for (key, value) in &map {
        if let Ok(idx) = key.parse::<usize>()
            && idx < vocab.len() {
                vocab[idx] = value.clone();
            }
    }

    Ok(vocab)
}
