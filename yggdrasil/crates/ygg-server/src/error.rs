//! Unified service error type with Axum `IntoResponse` integration.
//!
//! Provides `ServiceError` — a common error enum covering the variants shared
//! by Mimir, Muninn, and Huginn. Odin retains its own `OdinError` for
//! OpenAI-compatible formatting but can convert via `From<ServiceError>`.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Unified error type for Yggdrasil HTTP services.
///
/// Maps each variant to an HTTP status code and a JSON body of the form
/// `{ "error": "..." }`.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// Wraps errors from ygg-store (PostgreSQL + Qdrant).
    #[error("store error: {0}")]
    Store(#[from] ygg_store::error::StoreError),

    /// Wraps errors from ygg-embed (ONNX in-process embedding).
    #[error("embedding error: {0}")]
    Embed(#[from] ygg_embed::EmbedError),

    /// Configuration or startup errors (500).
    #[error("config error: {0}")]
    Config(String),

    /// Input validation failures (400).
    #[error("{0}")]
    Validation(String),

    /// Summarization-related failures (500).
    #[error("summarization error: {0}")]
    Summarization(String),

    /// Internal server errors (500).
    #[error("{0}")]
    Internal(String),

    /// Resource not found (404).
    #[error("{0}")]
    NotFound(String),
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        // Log before converting so every server-side error is traceable.
        match &self {
            ServiceError::Store(e) => tracing::error!(error = %e, "store error"),
            ServiceError::Embed(e) => tracing::error!(error = %e, "embed error"),
            ServiceError::Config(msg) => tracing::error!(error = %msg, "config error"),
            ServiceError::Validation(msg) => tracing::warn!(error = %msg, "validation error"),
            ServiceError::Summarization(msg) => {
                tracing::error!(error = %msg, "summarization error")
            }
            ServiceError::Internal(msg) => tracing::error!(error = %msg, "internal error"),
            ServiceError::NotFound(msg) => tracing::warn!(error = %msg, "not found"),
        }

        let (status, message) = match &self {
            ServiceError::Store(ygg_store::error::StoreError::Duplicate(msg)) => {
                (StatusCode::CONFLICT, msg.clone())
            }
            ServiceError::Store(ygg_store::error::StoreError::NotFound(msg)) => {
                (StatusCode::NOT_FOUND, msg.clone())
            }
            ServiceError::Store(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("storage failure: {e}"),
            ),
            ServiceError::Embed(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("embedding failed: {e}"),
            ),
            ServiceError::Config(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            ServiceError::Validation(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            ServiceError::Summarization(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, msg.clone())
            }
            ServiceError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            ServiceError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}
