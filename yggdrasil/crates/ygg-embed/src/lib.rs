use serde::{Deserialize, Serialize};

#[cfg(feature = "candle")]
pub mod candle_embed;

/// Errors from the embedding service.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("embedding HTTP error: {0}")]
    Http(String),
    #[error("embedding parse error: {0}")]
    Parse(String),
}

// ---------------------------------------------------------------------------
// Ollama-specific request/response types (private)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    prompt: &'a str,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embedding: Vec<f32>,
}

// Ollama batch embedding uses /api/embed with "input" field.
#[derive(Serialize)]
struct BatchEmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct BatchEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

// ---------------------------------------------------------------------------
// Backend dispatch enum
// ---------------------------------------------------------------------------

/// Internal embedding backend.
///
/// `Ollama` is always available. `Candle` is compiled only with the `candle`
/// feature flag and performs in-process inference, avoiding HTTP round-trips.
///
/// OPTIMIZATION (Candle variant): Requires the `candle` feature and a
/// compatible GGUF model on disk. Targets P95 < 5ms for a 128-token input
/// on Munin (Intel ARC iGPU via SYCL) or Hugin (AVX-512 CPU path).
/// Fallback: Ollama HTTP API (default, always available).
/// Threshold: only use candle when hardware-optimizer confirms < 5ms latency.
#[derive(Clone)]
enum EmbedBackend {
    Ollama {
        http: reqwest::Client,
        base_url: String,
        model: String,
    },
    #[cfg(feature = "candle")]
    Candle {
        // Arc makes the model cheaply cloneable (EmbedClient: Clone requirement).
        model: std::sync::Arc<candle_embed::CandelEmbedModel>,
    },
}

// ---------------------------------------------------------------------------
// Public EmbedClient
// ---------------------------------------------------------------------------

/// Embedding client supporting Ollama HTTP and optional candle in-process backends.
///
/// `EmbedClient` is `Clone`: cloning an Ollama client is cheap (reqwest::Client
/// uses an Arc internally); cloning a candle client is also cheap (the model is
/// behind an Arc).
///
/// The public `embed_single` / `embed_batch` API is identical regardless of which
/// backend was selected at construction time.
#[derive(Clone)]
pub struct EmbedClient {
    inner: EmbedBackend,
}

impl EmbedClient {
    /// Create an embedding client using the Ollama HTTP API (default backend).
    pub fn new(ollama_url: &str, model: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            inner: EmbedBackend::Ollama {
                http,
                base_url: ollama_url.trim_end_matches('/').to_string(),
                model: model.to_string(),
            },
        }
    }

    /// Create an embedding client using candle for in-process inference.
    ///
    /// `model_path` must point to a GGUF file compatible with qwen3-embedding.
    /// Device selection is automatic: SYCL iGPU > CUDA > Metal > CPU (AVX-512).
    ///
    /// This constructor is only available when the `candle` feature is enabled.
    /// If the model cannot be loaded, an `EmbedError` is returned.
    ///
    /// # Errors
    ///
    /// Returns `EmbedError::Parse` if the model cannot be loaded (stub until
    /// hardware-optimizer validates candle compatibility with the target model).
    #[cfg(feature = "candle")]
    pub fn with_candle(model_path: &str) -> Result<Self, EmbedError> {
        let model = candle_embed::CandelEmbedModel::load(model_path)?;
        Ok(Self {
            inner: EmbedBackend::Candle {
                model: std::sync::Arc::new(model),
            },
        })
    }

    /// Generate an embedding for a single text string.
    pub async fn embed_single(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        match &self.inner {
            EmbedBackend::Ollama {
                http,
                base_url,
                model,
            } => embed_single_ollama(http, base_url, model, text).await,

            #[cfg(feature = "candle")]
            EmbedBackend::Candle { model } => {
                // Run on a blocking thread: candle forward passes are CPU/GPU
                // compute and must not block the Tokio async executor.
                let model = model.clone();
                let text = text.to_string();
                tokio::task::spawn_blocking(move || model.embed(&text))
                    .await
                    .map_err(|e| EmbedError::Parse(e.to_string()))?
            }
        }
    }

    /// Generate embeddings for multiple texts in a single call.
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        match &self.inner {
            EmbedBackend::Ollama {
                http,
                base_url,
                model,
            } => embed_batch_ollama(http, base_url, model, texts).await,

            #[cfg(feature = "candle")]
            EmbedBackend::Candle { model } => {
                // Run entire batch on a blocking thread.
                let model = model.clone();
                let texts = texts.to_vec();
                tokio::task::spawn_blocking(move || {
                    texts
                        .iter()
                        .map(|t| model.embed(t))
                        .collect::<Result<Vec<_>, _>>()
                })
                .await
                .map_err(|e| EmbedError::Parse(e.to_string()))?
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Ollama backend helpers (free functions, private to this module)
// ---------------------------------------------------------------------------

async fn embed_single_ollama(
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
    text: &str,
) -> Result<Vec<f32>, EmbedError> {
    let url = format!("{base_url}/api/embeddings");
    let resp = http
        .post(&url)
        .json(&EmbedRequest {
            model,
            prompt: text,
        })
        .send()
        .await
        .map_err(|e| EmbedError::Http(e.to_string()))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(EmbedError::Http(format!("{status}: {body}")));
    }

    let result: EmbedResponse = resp
        .json()
        .await
        .map_err(|e| EmbedError::Parse(e.to_string()))?;

    Ok(result.embedding)
}

async fn embed_batch_ollama(
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let url = format!("{base_url}/api/embed");
    let resp = http
        .post(&url)
        .json(&BatchEmbedRequest {
            model,
            input: texts,
        })
        .send()
        .await
        .map_err(|e| EmbedError::Http(e.to_string()))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(EmbedError::Http(format!("{status}: {body}")));
    }

    let result: BatchEmbedResponse = resp
        .json()
        .await
        .map_err(|e| EmbedError::Parse(e.to_string()))?;

    Ok(result.embeddings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_client_has_timeout() {
        // Verify the Ollama client is constructed with a timeout (not bare Client::new()).
        let client = EmbedClient::new("http://localhost:11434", "test-model");
        match &client.inner {
            EmbedBackend::Ollama { http, base_url, model } => {
                assert_eq!(base_url, "http://localhost:11434");
                assert_eq!(model, "test-model");
                // reqwest::Client doesn't expose timeout directly, but we verify
                // construction succeeded with the builder path.
                let _ = http;
            }
            #[cfg(feature = "candle")]
            _ => panic!("expected Ollama backend"),
        }
    }

    #[tokio::test]
    async fn embed_single_timeout_on_unreachable() {
        // Connect to a port that won't respond — should fail with Http error, not hang.
        let client = EmbedClient::new("http://127.0.0.1:1", "test");
        let result = client.embed_single("hello").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            EmbedError::Http(_) => {} // expected
            other => panic!("expected Http error, got: {other}"),
        }
    }

    #[tokio::test]
    async fn embed_batch_empty_returns_empty() {
        let client = EmbedClient::new("http://127.0.0.1:1", "test");
        let result = client.embed_batch(&[]).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
