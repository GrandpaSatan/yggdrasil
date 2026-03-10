use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use ygg_store::error::StoreError;

/// Unified error type for the Mimir HTTP service.
///
/// Every variant maps to a specific HTTP status code and a JSON error body
/// of the form `{ "error": "..." }`.
#[derive(Debug, thiserror::Error)]
pub enum MimirError {
    /// Wraps errors from ygg-store (PostgreSQL + Qdrant).
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// Configuration or startup errors (500).
    #[error("config error: {0}")]
    Config(String),

    /// Input validation failures (400).
    #[error("{0}")]
    Validation(String),

    /// Summarization-related failures (Odin call, response parsing, etc.).
    #[error("summarization error: {0}")]
    Summarization(String),

    /// ONNX embedder failures (model load, tokenization, inference).
    #[error("embedder error: {0}")]
    Embedder(String),
}

impl IntoResponse for MimirError {
    fn into_response(self) -> Response {
        // Log before converting so every server-side error is traceable.
        match &self {
            MimirError::Store(e) => tracing::error!(error = %e, "store error"),
            MimirError::Config(msg) => tracing::error!(error = %msg, "config error"),
            MimirError::Validation(msg) => tracing::warn!(error = %msg, "validation error"),
            MimirError::Summarization(msg) => tracing::error!(error = %msg, "summarization error"),
            MimirError::Embedder(msg) => tracing::error!(error = %msg, "embedder error"),
        }

        let (status, message) = match &self {
            MimirError::Store(StoreError::Duplicate(msg)) => (StatusCode::CONFLICT, msg.clone()),
            MimirError::Store(StoreError::NotFound(msg)) => (StatusCode::NOT_FOUND, msg.clone()),
            MimirError::Store(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("storage failure: {e}"),
            ),
            MimirError::Config(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            MimirError::Validation(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            MimirError::Summarization(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            MimirError::Embedder(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}
