/// Unified error type for Odin's request handlers.
///
/// Implements `axum::response::IntoResponse` so handlers can use the
/// `Result<T, OdinError>` return type and have errors automatically
/// serialised to JSON with the appropriate HTTP status code.
///
/// `/v1/*` endpoints return OpenAI-compatible error JSON:
/// `{"error": {"message": "...", "type": "...", "code": null}}`
///
/// `/api/v1/*` proxy endpoints return plain error JSON:
/// `{"error": "..."}`
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OdinError {
    /// Client sent an invalid request (400).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// A backend Ollama instance is at its concurrency limit (503).
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),

    /// The upstream Ollama backend returned an error or was unreachable (502).
    #[error("upstream error: {0}")]
    Upstream(String),

    /// The transparent Mimir proxy encountered a connection error (502).
    #[error("proxy error: {0}")]
    Proxy(String),

    /// An unexpected internal error (500).
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for OdinError {
    fn into_response(self) -> Response {
        match &self {
            // ── /v1/* endpoints: OpenAI-compatible error format ──────────────
            OdinError::BadRequest(msg) => {
                let body = json!({
                    "error": {
                        "message": msg,
                        "type": "invalid_request_error",
                        "code": null
                    }
                });
                (StatusCode::BAD_REQUEST, Json(body)).into_response()
            }

            OdinError::BackendUnavailable(msg) => {
                let body = json!({
                    "error": {
                        "message": msg,
                        "type": "service_unavailable",
                        "code": null
                    }
                });
                (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
            }

            OdinError::Upstream(msg) => {
                let body = json!({
                    "error": {
                        "message": msg,
                        "type": "server_error",
                        "code": null
                    }
                });
                (StatusCode::BAD_GATEWAY, Json(body)).into_response()
            }

            OdinError::Internal(msg) => {
                let body = json!({
                    "error": {
                        "message": msg,
                        "type": "server_error",
                        "code": null
                    }
                });
                (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
            }

            // ── /api/v1/* proxy endpoints: plain error format ────────────────
            OdinError::Proxy(msg) => {
                let body = json!({ "error": msg });
                (StatusCode::BAD_GATEWAY, Json(body)).into_response()
            }
        }
    }
}

/// Convert a `reqwest::Error` into `OdinError::Upstream` so handlers can
/// use the `?` operator on reqwest calls in Ollama proxy functions.
impl From<reqwest::Error> for OdinError {
    fn from(err: reqwest::Error) -> Self {
        OdinError::Upstream(err.to_string())
    }
}
