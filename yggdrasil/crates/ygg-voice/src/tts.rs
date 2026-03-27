//! Kokoro-82M ONNX text-to-speech via ort with OpenVINO ExecutionProvider.
//!
//! Runs on Munin's Intel AI Boost NPU. Same model as VoiceForge uses
//! via the Python `kokoro` package, but loaded directly as ONNX.
//!
//! Model files (from onnx-community/Kokoro-82M-v1.0-ONNX):
//!   - kokoro-v1.0.onnx: phoneme IDs + style vector → 24kHz audio
//!   - voices-v1.0.bin: pre-extracted style vectors for each voice
//!
//! Phonemization: shells out to espeak-ng for text → IPA phoneme conversion.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use ndarray::{Array1, Array2};
use ort::session::Session;
use ort::value::TensorRef;
use tracing::{info, warn};

use crate::VoiceError;

/// Kokoro output sample rate.
pub const KOKORO_SAMPLE_RATE: u32 = 24_000;

/// Style vector dimension for Kokoro voices.
const STYLE_DIM: usize = 256;

/// Kokoro TTS engine.
pub struct KokoroTts {
    session: Arc<Mutex<Session>>,
    voices: VoiceStyles,
    voice_name: String,
    speed: f32,
}

/// Pre-loaded voice style vectors.
struct VoiceStyles {
    /// Map of voice name → style vector [STYLE_DIM].
    styles: std::collections::HashMap<String, Vec<f32>>,
}

impl KokoroTts {
    /// Load the Kokoro ONNX model and voice styles.
    ///
    /// `model_path`: path to `kokoro-v1.0.onnx`
    /// `voices_path`: path to `voices-v1.0.bin`
    /// `voice`: default voice name (e.g., "af_heart")
    /// `speed`: speech rate multiplier (1.0 = normal)
    pub fn load(
        model_path: &Path,
        voices_path: &Path,
        voice: &str,
        speed: f32,
        device: &str,
        fallback: &str,
    ) -> Result<Self, VoiceError> {
        if !model_path.exists() {
            return Err(VoiceError::ModelLoad(format!(
                "kokoro model not found: {}",
                model_path.display()
            )));
        }

        let session = crate::stt::build_session(model_path, device, fallback)?;

        info!(model = %model_path.display(), device, fallback, "Kokoro TTS model loaded");

        let voices = load_voices(voices_path)?;
        info!(
            voices = voices.styles.len(),
            default_voice = voice,
            "Kokoro voice styles loaded"
        );

        if !voices.styles.contains_key(voice) && !voices.styles.is_empty() {
            warn!(
                voice,
                available = ?voices.styles.keys().collect::<Vec<_>>(),
                "requested voice not found — will use first available"
            );
        }

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            voices,
            voice_name: voice.to_string(),
            speed,
        })
    }

    /// Synthesize speech from text (synchronous, blocking).
    ///
    /// Returns f32 audio samples at 24kHz.
    #[allow(dead_code)]
    pub fn synthesize(&self, text: &str) -> Result<Vec<f32>, VoiceError> {
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }

        // Step 1: Phonemize via espeak-ng
        let phonemes = phonemize(text)?;
        if phonemes.is_empty() {
            return Ok(Vec::new());
        }

        // Step 2: Convert phonemes to token IDs
        let token_ids = phonemes_to_ids(&phonemes);
        if token_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Step 3: Get voice style vector
        let style = self
            .voices
            .styles
            .get(&self.voice_name)
            .or_else(|| self.voices.styles.values().next())
            .ok_or_else(|| VoiceError::Tts("no voice styles loaded".into()))?;

        // Step 4: Build input tensors
        let seq_len = token_ids.len();
        let tokens_array = Array2::from_shape_vec(
            (1, seq_len),
            token_ids.iter().map(|&id| id as i64).collect(),
        )
        .map_err(|e| VoiceError::Tts(format!("tokens shape error: {e}")))?;

        let style_array = Array2::from_shape_vec(
            (1, STYLE_DIM),
            style.to_vec(),
        )
        .map_err(|e| VoiceError::Tts(format!("style shape error: {e}")))?;

        let speed_array = Array1::from_vec(vec![self.speed]);

        // Step 5: Run inference
        let tokens_tensor = TensorRef::from_array_view(&tokens_array)
            .map_err(|e| VoiceError::Tts(format!("tokens tensor error: {e}")))?;
        let style_tensor = TensorRef::from_array_view(&style_array)
            .map_err(|e| VoiceError::Tts(format!("style tensor error: {e}")))?;
        let speed_tensor = TensorRef::from_array_view(&speed_array)
            .map_err(|e| VoiceError::Tts(format!("speed tensor error: {e}")))?;

        let audio_data = {
            let mut session = self
                .session
                .lock()
                .map_err(|_| VoiceError::Tts("TTS session mutex poisoned".into()))?;

            let outputs = session
                .run(ort::inputs![
                    "input_ids" => tokens_tensor,
                    "style" => style_tensor,
                    "speed" => speed_tensor,
                ])
                .map_err(|e| VoiceError::Tts(format!("TTS inference failed: {e}")))?;

            let (_shape, data) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| VoiceError::Tts(format!("TTS output extraction failed: {e}")))?;

            data.to_vec()
        };

        Ok(audio_data)
    }

    /// Synthesize asynchronously via spawn_blocking.
    pub async fn synthesize_async(&self, text: String) -> Result<Vec<f32>, VoiceError> {
        let session = Arc::clone(&self.session);
        let voices = self.voices.styles.clone();
        let voice_name = self.voice_name.clone();
        let speed = self.speed;

        tokio::task::spawn_blocking(move || {
            let tts = KokoroTtsHandle {
                session,
                voices,
                voice_name,
                speed,
            };
            tts.synthesize(&text)
        })
        .await
        .map_err(|e| VoiceError::Tts(format!("synthesize task panicked: {e}")))?
    }

    /// Get the output sample rate.
    pub fn sample_rate(&self) -> u32 {
        KOKORO_SAMPLE_RATE
    }
}

/// Handle for async synthesis.
struct KokoroTtsHandle {
    session: Arc<Mutex<Session>>,
    voices: std::collections::HashMap<String, Vec<f32>>,
    voice_name: String,
    speed: f32,
}

impl KokoroTtsHandle {
    fn synthesize(&self, text: &str) -> Result<Vec<f32>, VoiceError> {
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }

        let phonemes = phonemize(text)?;
        if phonemes.is_empty() {
            return Ok(Vec::new());
        }

        let token_ids = phonemes_to_ids(&phonemes);
        if token_ids.is_empty() {
            return Ok(Vec::new());
        }

        let style = self
            .voices
            .get(&self.voice_name)
            .or_else(|| self.voices.values().next())
            .ok_or_else(|| VoiceError::Tts("no voice styles loaded".into()))?;

        let seq_len = token_ids.len();
        let tokens_array = Array2::from_shape_vec(
            (1, seq_len),
            token_ids.iter().map(|&id| id as i64).collect(),
        )
        .map_err(|e| VoiceError::Tts(format!("tokens shape error: {e}")))?;

        let style_array = Array2::from_shape_vec(
            (1, STYLE_DIM),
            style.to_vec(),
        )
        .map_err(|e| VoiceError::Tts(format!("style shape error: {e}")))?;

        let speed_array = Array1::from_vec(vec![self.speed]);

        let tokens_tensor = TensorRef::from_array_view(&tokens_array)
            .map_err(|e| VoiceError::Tts(format!("tokens tensor error: {e}")))?;
        let style_tensor = TensorRef::from_array_view(&style_array)
            .map_err(|e| VoiceError::Tts(format!("style tensor error: {e}")))?;
        let speed_tensor = TensorRef::from_array_view(&speed_array)
            .map_err(|e| VoiceError::Tts(format!("speed tensor error: {e}")))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| VoiceError::Tts("TTS session mutex poisoned".into()))?;

        let outputs = session
            .run(ort::inputs![
                "input_ids" => tokens_tensor,
                "style" => style_tensor,
                "speed" => speed_tensor,
            ])
            .map_err(|e| VoiceError::Tts(format!("TTS inference failed: {e}")))?;

        let (_shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| VoiceError::Tts(format!("TTS output extraction failed: {e}")))?;

        Ok(data.to_vec())
    }
}

/// Run espeak-ng to convert English text to IPA phonemes.
fn phonemize(text: &str) -> Result<String, VoiceError> {
    let output = Command::new("/usr/bin/espeak-ng")
        .args(["--ipa", "-q", "--stdin", "-v", "en-us"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(text.as_bytes())?;
            }
            child.wait_with_output()
        })
        .map_err(|e| {
            VoiceError::Tts(format!(
                "espeak-ng not found or failed — install with `apt install espeak-ng`: {e}"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(VoiceError::Tts(format!("espeak-ng failed: {stderr}")));
    }

    let phonemes = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(phonemes)
}

/// Convert IPA phoneme string to Kokoro token IDs.
///
/// Kokoro uses a fixed phoneme-to-ID mapping. This is a simplified version
/// covering the core IPA symbols for American English.
fn phonemes_to_ids(phonemes: &str) -> Vec<u32> {
    // Kokoro's phoneme vocabulary (subset for American English).
    // Full mapping would come from the model's tokenizer config.
    // Token 0 = padding, 1 = BOS, 2 = EOS
    let mut ids = vec![1u32]; // BOS

    for ch in phonemes.chars() {
        let id = match ch {
            ' ' => 3,
            'a' => 4, 'b' => 5, 'c' => 6, 'd' => 7, 'e' => 8,
            'f' => 9, 'g' => 10, 'h' => 11, 'i' => 12, 'j' => 13,
            'k' => 14, 'l' => 15, 'm' => 16, 'n' => 17, 'o' => 18,
            'p' => 19, 'r' => 20, 's' => 21, 't' => 22, 'u' => 23,
            'v' => 24, 'w' => 25, 'x' => 26, 'z' => 27,
            // IPA-specific characters
            'ə' => 28, 'ɪ' => 29, 'ʊ' => 30, 'ɛ' => 31, 'ɔ' => 32,
            'æ' => 33, 'ʌ' => 34, 'ɑ' => 35, 'ð' => 36, 'θ' => 37,
            'ʃ' => 38, 'ʒ' => 39, 'ŋ' => 40, 'ɹ' => 41, 'ɾ' => 42,
            'ɫ' => 43, 'ɬ' => 44,
            // Suprasegmentals
            'ˈ' => 45, 'ˌ' => 46, 'ː' => 47,
            // Punctuation
            '.' => 48, ',' => 49, '?' => 50, '!' => 51,
            // Additional IPA vowels
            'ɒ' => 52, 'ø' => 53, 'y' => 54, 'ɵ' => 55,
            // Additional IPA consonants
            'ɡ' => 56, 'ç' => 57, 'ʔ' => 58, 'ɲ' => 59,
            'ɳ' => 60, 'ɻ' => 61, 'ɽ' => 62, 'ʎ' => 63,
            // Diacritics and modifiers
            '\u{0303}' => 64, '\u{0329}' => 65, '\u{032F}' => 66, '\u{032A}' => 67,
            // Additional suprasegmentals
            '|' => 68, '‖' => 69,
            // Digits (for reading numbers)
            '0' => 70, '1' => 71, '2' => 72, '3' => 73, '4' => 74,
            '5' => 75, '6' => 76, '7' => 77, '8' => 78, '9' => 79,
            // Common punctuation
            '-' => 80, ':' => 81, ';' => 82, '\'' => 83, '"' => 84,
            '(' => 85, ')' => 86,
            // Tie bar for affricates
            '\u{0361}' => 87,
            // Unknown characters map to a generic token
            _ => 3, // space/unknown
        };
        ids.push(id);
    }

    ids.push(2); // EOS
    ids
}

/// Load voice style vectors from the voices binary file.
///
/// Format: repeated entries of:
///   - name_len: u16 (little-endian)
///   - name: [u8; name_len]
///   - style: [f32; STYLE_DIM] (little-endian)
fn load_voices(path: &Path) -> Result<VoiceStyles, VoiceError> {
    if !path.exists() {
        warn!(
            path = %path.display(),
            "voices file not found — TTS will use zero style vector"
        );
        let mut styles = std::collections::HashMap::new();
        styles.insert("default".into(), vec![0.0f32; STYLE_DIM]);
        return Ok(VoiceStyles { styles });
    }

    let data = std::fs::read(path)
        .map_err(|e| VoiceError::ModelLoad(format!("failed to read voices file: {e}")))?;

    let mut styles = std::collections::HashMap::new();
    let mut offset = 0;

    while offset + 2 < data.len() {
        // Read name length
        let name_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;

        if offset + name_len > data.len() {
            break;
        }

        // Read name
        let name = String::from_utf8_lossy(&data[offset..offset + name_len]).to_string();
        offset += name_len;

        // Read style vector (STYLE_DIM * 4 bytes)
        let style_bytes = STYLE_DIM * 4;
        if offset + style_bytes > data.len() {
            break;
        }

        let style: Vec<f32> = data[offset..offset + style_bytes]
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        offset += style_bytes;

        styles.insert(name, style);
    }

    if styles.is_empty() {
        styles.insert("default".into(), vec![0.0f32; STYLE_DIM]);
    }

    Ok(VoiceStyles { styles })
}
