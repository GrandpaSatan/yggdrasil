//! HTTP-based sentence embedding via Ollama.
//!
//! Calls Ollama's `/v1/embeddings` endpoint (OpenAI-compatible) running
//! `all-minilm` (all-MiniLM-L6-v2) on Munin's Ollama instance (port 11434).
//!
//! ## Output
//!
//! all-MiniLM-L6-v2 produces 384-dimensional L2-normalised float vectors.

use std::sync::Arc;

/// Errors from the embedding service.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("embedding error: {0}")]
    Embedder(String),
}

/// Response from llama-server `/v1/embeddings`.
#[derive(serde::Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(serde::Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

/// HTTP-based sentence embedder backed by Ollama.
///
/// Thread-safe and cheap to clone (Arc over the HTTP client + URL).
#[derive(Clone)]
pub struct LlamaEmbedder {
    client: Arc<reqwest::blocking::Client>,
    url: Arc<String>,
    model: Arc<String>,
}

// Keep the old name as a type alias for backward compat in downstream crates.
pub type OnnxEmbedder = LlamaEmbedder;

impl LlamaEmbedder {
    /// Create an embedder pointing at an Ollama embedding endpoint.
    ///
    /// `base_url` should be e.g. `http://localhost:11434`.
    /// `model` is the Ollama model name (e.g. `all-minilm`).
    pub fn new(base_url: &str) -> Result<Self, EmbedError> {
        Self::with_model(base_url, "all-minilm")
    }

    /// Create an embedder with a specific model name.
    pub fn with_model(base_url: &str, model: &str) -> Result<Self, EmbedError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| EmbedError::Embedder(format!("http client build failed: {e}")))?;

        tracing::info!(url = %base_url, model = %model, "embedding service ready");

        Ok(Self {
            client: Arc::new(client),
            url: Arc::new(base_url.trim_end_matches('/').to_string()),
            model: Arc::new(model.to_string()),
        })
    }

    /// Backward-compatible constructor — accepts a path or URL.
    /// If the argument looks like a URL (starts with http), uses it directly.
    /// Otherwise treats it as a legacy path and falls back to Ollama at localhost:11434.
    pub fn load(path: &std::path::Path) -> Result<Self, EmbedError> {
        let s = path.to_string_lossy();
        let url = if s.starts_with("http") {
            s.to_string()
        } else {
            tracing::warn!(
                path = %s,
                "legacy ONNX model_dir detected, using Ollama at http://localhost:11434 instead"
            );
            "http://127.0.0.1:11434".to_string()
        };
        Self::new(&url)
    }

    /// Embed a single text string synchronously. Returns a 384-dim L2-normalised vector.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let body = serde_json::json!({
            "model": &*self.model,
            "input": text,
        });

        let resp = self
            .client
            .post(format!("{}/v1/embeddings", self.url))
            .json(&body)
            .send()
            .map_err(|e| EmbedError::Embedder(format!("embed request failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(EmbedError::Embedder(format!(
                "embed server returned {}",
                resp.status()
            )));
        }

        let embed_resp: EmbeddingResponse = resp
            .json()
            .map_err(|e| EmbedError::Embedder(format!("embed response parse failed: {e}")))?;

        let mut embedding = embed_resp
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or_else(|| EmbedError::Embedder("empty embedding response".into()))?;

        // L2 normalize in-place (llama-server may or may not normalize).
        let norm: f32 = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut embedding {
                *v /= norm;
            }
        }
        Ok(embedding)
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
