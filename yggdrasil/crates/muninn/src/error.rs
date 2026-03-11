use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use ygg_embed::EmbedError;
use ygg_store::error::StoreError;

/// Unified error type for the Muninn HTTP service.
///
/// Every variant maps to a specific HTTP status code and a JSON error body
/// of the form `{ "error": "..." }`.
#[derive(Debug, thiserror::Error)]
pub enum MuninnError {
    /// Wraps errors from ygg-store (PostgreSQL + Qdrant).
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// Wraps errors from ygg-embed (ONNX in-process embedding).
    #[error("embedding error: {0}")]
    Embed(#[from] EmbedError),

    /// Configuration or startup errors (500).
    #[error("config error: {0}")]
    Config(String),

    /// Input validation failures (400).
    #[error("{0}")]
    BadRequest(String),

    /// Internal server errors not covered by the above variants.
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for MuninnError {
    fn into_response(self) -> Response {
        // Log before converting so every server-side error is traceable.
        match &self {
            MuninnError::Store(e) => tracing::error!(error = %e, "store error"),
            MuninnError::Embed(e) => tracing::error!(error = %e, "embed error"),
            MuninnError::Config(msg) => tracing::error!(error = %msg, "config error"),
            MuninnError::Internal(msg) => tracing::error!(error = %msg, "internal error"),
            MuninnError::BadRequest(msg) => tracing::warn!(error = %msg, "bad request"),
        }

        let (status, message) = match &self {
            MuninnError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            MuninnError::Store(StoreError::NotFound(msg)) => {
                (StatusCode::NOT_FOUND, msg.clone())
            }
            MuninnError::Store(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("storage failure: {e}"),
            ),
            MuninnError::Embed(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("embedding failed: {e}"),
            ),
            MuninnError::Config(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            MuninnError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}
