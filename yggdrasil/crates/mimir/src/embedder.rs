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

use std::path::Path;
use std::sync::{Arc, Mutex};

use ndarray::Array2;
use ort::session::Session;
use ort::value::TensorRef;
use tokenizers::Tokenizer;

use crate::error::MimirError;

/// In-process ONNX sentence embedder.
///
/// Thread-safe via `Arc<Mutex<Session>>` — `Session::run` takes `&mut self` so we
/// require exclusive access during inference. Since `embed()` is called exclusively
/// from `spawn_blocking`, only one blocking thread runs inference at a time per
/// embedder instance. Cloning produces a second reference to the same session+tokenizer.
///
/// # Hardware note
/// OPTIMIZATION: Configured with 4 intra-op threads, matching the Munin Intel Core
/// Ultra 185H efficiency core count available for background tasks. Fallback: if
/// thread config fails the ORT runtime default applies.
#[derive(Clone)]
pub struct OnnxEmbedder {
    /// Mutex needed because `Session::run` takes `&mut self`.
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl OnnxEmbedder {
    /// Load the ONNX model and tokenizer from `model_dir`.
    ///
    /// The directory must contain `model.onnx` and `tokenizer.json`.
    /// Blocks on file I/O and model parsing — call at startup, not in a hot path.
    pub fn load(model_dir: &Path) -> Result<Self, MimirError> {
        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !model_path.exists() {
            return Err(MimirError::Embedder(format!(
                "model.onnx not found in {}",
                model_dir.display()
            )));
        }
        if !tokenizer_path.exists() {
            return Err(MimirError::Embedder(format!(
                "tokenizer.json not found in {}",
                model_dir.display()
            )));
        }

        // Build the ONNX Runtime session.
        // OPTIMIZATION: 4 intra-op threads for parallelism within single ops (matmul, etc.).
        // Fallback: if with_intra_threads fails (e.g. minimal ORT build), recover and continue.
        let session = {
            let mut builder = Session::builder()
                .map_err(|e| MimirError::Embedder(format!("failed to create session builder: {e}")))?;

            // Apply thread setting; recover silently if unsupported by this ORT build.
            builder = builder.with_intra_threads(4).unwrap_or_else(|e| e.recover());

            builder
                .commit_from_file(&model_path)
                .map_err(|e| MimirError::Embedder(format!("failed to load ONNX model: {e}")))?
        };

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| MimirError::Embedder(format!("failed to load tokenizer: {e}")))?;

        tracing::info!(
            model = %model_path.display(),
            "ONNX embedder loaded"
        );

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
        })
    }

    /// Embed a single text string. Returns a dense float vector.
    ///
    /// For all-MiniLM-L6-v2, the output is 384-dimensional.
    ///
    /// Runs synchronously — MUST be called via `tokio::task::spawn_blocking` from async context.
    ///
    /// # Implementation
    ///
    /// Pipeline: tokenize → build i64 tensors → ONNX inference → mean pool over sequence
    /// dimension → L2 normalize. The token_embeddings output has shape [1, seq_len, hidden_dim].
    /// We extract this as a flat `&[f32]` slice and perform mean pooling manually without
    /// converting to ndarray to avoid an extra allocation.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, MimirError> {
        // Tokenize
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| MimirError::Embedder(format!("tokenization failed: {e}")))?;

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

        // Build ndarray Arrays (batch_size=1) for creating TensorRef inputs.
        let input_ids_array = Array2::from_shape_vec((1, seq_len), input_ids)
            .map_err(|e| MimirError::Embedder(format!("input_ids shape error: {e}")))?;
        let attention_mask_array = Array2::from_shape_vec((1, seq_len), attention_mask)
            .map_err(|e| MimirError::Embedder(format!("attention_mask shape error: {e}")))?;
        let token_type_ids_array = Array2::from_shape_vec((1, seq_len), token_type_ids)
            .map_err(|e| MimirError::Embedder(format!("token_type_ids shape error: {e}")))?;

        // Create TensorRef views over the arrays (zero-copy, borrows the data).
        let input_ids_ref = TensorRef::from_array_view(&input_ids_array)
            .map_err(|e| MimirError::Embedder(format!("input_ids tensor ref failed: {e}")))?;
        let attention_mask_ref = TensorRef::from_array_view(&attention_mask_array)
            .map_err(|e| MimirError::Embedder(format!("attention_mask tensor ref failed: {e}")))?;
        let token_type_ids_ref = TensorRef::from_array_view(&token_type_ids_array)
            .map_err(|e| MimirError::Embedder(format!("token_type_ids tensor ref failed: {e}")))?;

        // Run ONNX inference.
        // Named inputs must match the model's input names exactly.
        // For all-MiniLM-L6-v2 the inputs are: input_ids, attention_mask, token_type_ids.
        //
        // Session::run takes &mut self, so we acquire the mutex.
        // The guard must be bound to a `let` binding (not a temporary) so it lives long
        // enough for `outputs` to borrow from the session outputs.
        // In practice this lock is uncontended: embed() is called from spawn_blocking only.
        let mut guard = self
            .session
            .lock()
            .map_err(|_| MimirError::Embedder("ONNX session mutex poisoned".into()))?;
        let outputs = guard
            .run(ort::inputs![
                "input_ids" => input_ids_ref,
                "attention_mask" => attention_mask_ref,
                "token_type_ids" => token_type_ids_ref,
            ])
            .map_err(|e| MimirError::Embedder(format!("ONNX inference failed: {e}")))?;

        // Extract the first output tensor (token_embeddings: [1, seq_len, hidden_dim]).
        // try_extract_tensor returns (&Shape, &[T]) — shape is [1, seq_len, hidden_dim].
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| MimirError::Embedder(format!("output extraction failed: {e}")))?;

        // Shape: [batch=1, seq_len, hidden_dim]
        let dims: Vec<i64> = shape.iter().copied().collect();
        if dims.len() < 3 {
            return Err(MimirError::Embedder(format!(
                "unexpected output shape: expected [1, seq_len, hidden_dim], got {dims:?}"
            )));
        }
        let actual_seq_len = dims[1] as usize;
        let hidden_dim = dims[2] as usize;

        if data.len() < actual_seq_len * hidden_dim {
            return Err(MimirError::Embedder(format!(
                "output data length mismatch: got {}, expected {}",
                data.len(),
                actual_seq_len * hidden_dim
            )));
        }

        // Mean pooling: sum over seq_len dimension, then divide by seq_len.
        // data layout: [token_0_dim_0, token_0_dim_1, ..., token_0_dim_H-1, token_1_dim_0, ...]
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

        // L2 normalize so downstream dot-product similarity == cosine similarity.
        let norm: f32 = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut pooled {
                *val /= norm;
            }
        }

        Ok(pooled)
    }
}
