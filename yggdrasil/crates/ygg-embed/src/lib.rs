//! ONNX Runtime-based sentence embedding.
//!
//! Loads a sentence-transformer model (all-MiniLM-L6-v2) from a local directory
//! and runs inference in-process on CPU. No Ollama dependency.
//!
//! ## Model directory layout
//!
//! The `model_dir` must contain:
//!   - `model.onnx` — the ONNX model file
//!   - `tokenizer.json` — HuggingFace fast tokenizer config
//!
//! ## Output
//!
//! all-MiniLM-L6-v2 produces 384-dimensional L2-normalised float vectors.

use std::path::Path;
use std::sync::{Arc, Mutex};

use ndarray::Array2;
use ort::session::Session;
use ort::value::TensorRef;
use tokenizers::Tokenizer;

/// Errors from the embedding service.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("embedding error: {0}")]
    Embedder(String),
}

/// In-process ONNX sentence embedder.
///
/// Thread-safe via `Arc<Mutex<Session>>` — `Session::run` takes `&mut self` so we
/// require exclusive access during inference. Cloning produces a second reference
/// to the same session+tokenizer (cheap Arc bump).
///
/// # Hardware note
/// Configured with 4 intra-op threads, targeting efficiency cores on Intel Core
/// Ultra 185H (Munin) and AMD Ryzen (Hugin). Fallback: if thread config fails
/// the ORT runtime default applies.
#[derive(Clone)]
pub struct OnnxEmbedder {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl OnnxEmbedder {
    /// Load the ONNX model and tokenizer from `model_dir`.
    ///
    /// The directory must contain `model.onnx` and `tokenizer.json`.
    /// Blocks on file I/O and model parsing — call at startup, not in a hot path.
    pub fn load(model_dir: &Path) -> Result<Self, EmbedError> {
        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !model_path.exists() {
            return Err(EmbedError::Embedder(format!(
                "model.onnx not found in {}",
                model_dir.display()
            )));
        }
        if !tokenizer_path.exists() {
            return Err(EmbedError::Embedder(format!(
                "tokenizer.json not found in {}",
                model_dir.display()
            )));
        }

        let session = {
            let mut builder = Session::builder()
                .map_err(|e| EmbedError::Embedder(format!("failed to create session builder: {e}")))?;

            builder = builder.with_intra_threads(4).unwrap_or_else(|e| e.recover());

            builder
                .commit_from_file(&model_path)
                .map_err(|e| EmbedError::Embedder(format!("failed to load ONNX model: {e}")))?
        };

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| EmbedError::Embedder(format!("failed to load tokenizer: {e}")))?;

        tracing::info!(
            model = %model_path.display(),
            "ONNX embedder loaded"
        );

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
        })
    }

    /// Embed a single text string synchronously. Returns a 384-dim L2-normalised vector.
    ///
    /// Runs synchronously — callers in async context should use [`embed_single`] or
    /// wrap this in `tokio::task::spawn_blocking`.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| EmbedError::Embedder(format!("tokenization failed: {e}")))?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let token_type_ids: Vec<i64> = encoding
            .get_type_ids()
            .iter()
            .map(|&t| t as i64)
            .collect();

        let seq_len = input_ids.len();

        let input_ids_array = Array2::from_shape_vec((1, seq_len), input_ids)
            .map_err(|e| EmbedError::Embedder(format!("input_ids shape error: {e}")))?;
        let attention_mask_array = Array2::from_shape_vec((1, seq_len), attention_mask)
            .map_err(|e| EmbedError::Embedder(format!("attention_mask shape error: {e}")))?;
        let token_type_ids_array = Array2::from_shape_vec((1, seq_len), token_type_ids)
            .map_err(|e| EmbedError::Embedder(format!("token_type_ids shape error: {e}")))?;

        let input_ids_ref = TensorRef::from_array_view(&input_ids_array)
            .map_err(|e| EmbedError::Embedder(format!("input_ids tensor ref failed: {e}")))?;
        let attention_mask_ref = TensorRef::from_array_view(&attention_mask_array)
            .map_err(|e| EmbedError::Embedder(format!("attention_mask tensor ref failed: {e}")))?;
        let token_type_ids_ref = TensorRef::from_array_view(&token_type_ids_array)
            .map_err(|e| EmbedError::Embedder(format!("token_type_ids tensor ref failed: {e}")))?;

        let mut guard = self
            .session
            .lock()
            .map_err(|_| EmbedError::Embedder("ONNX session mutex poisoned".into()))?;
        let outputs = guard
            .run(ort::inputs![
                "input_ids" => input_ids_ref,
                "attention_mask" => attention_mask_ref,
                "token_type_ids" => token_type_ids_ref,
            ])
            .map_err(|e| EmbedError::Embedder(format!("ONNX inference failed: {e}")))?;

        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| EmbedError::Embedder(format!("output extraction failed: {e}")))?;

        let dims: Vec<i64> = shape.iter().copied().collect();
        if dims.len() < 3 {
            return Err(EmbedError::Embedder(format!(
                "unexpected output shape: expected [1, seq_len, hidden_dim], got {dims:?}"
            )));
        }
        let actual_seq_len = dims[1] as usize;
        let hidden_dim = dims[2] as usize;

        if data.len() < actual_seq_len * hidden_dim {
            return Err(EmbedError::Embedder(format!(
                "output data length mismatch: got {}, expected {}",
                data.len(),
                actual_seq_len * hidden_dim
            )));
        }

        // Mean pooling over sequence dimension.
        let mut pooled = vec![0.0f32; hidden_dim];
        for token_idx in 0..actual_seq_len {
            let offset = token_idx * hidden_dim;
            for dim_idx in 0..hidden_dim {
                pooled[dim_idx] += data[offset + dim_idx];
            }
        }
        let scale = 1.0 / actual_seq_len as f32;
        for val in &mut pooled {
            *val *= scale;
        }

        // L2 normalize so downstream dot-product == cosine similarity.
        let norm: f32 = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut pooled {
                *val /= norm;
            }
        }

        Ok(pooled)
    }

    /// Embed a single text asynchronously via `spawn_blocking`.
    pub async fn embed_single(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let embedder = self.clone();
        let text = text.to_string();
        tokio::task::spawn_blocking(move || embedder.embed(&text))
            .await
            .map_err(|e| EmbedError::Embedder(format!("embed task panicked: {e}")))?
    }

    /// Embed multiple texts asynchronously via `spawn_blocking`.
    ///
    /// Texts are embedded sequentially within a single blocking thread. Each text
    /// takes ~15ms on CPU, so a batch of 10 completes in ~150ms.
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let embedder = self.clone();
        let texts = texts.to_vec();
        tokio::task::spawn_blocking(move || {
            texts
                .iter()
                .map(|t| embedder.embed(t))
                .collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|e| EmbedError::Embedder(format!("embed batch task panicked: {e}")))?
    }
}
