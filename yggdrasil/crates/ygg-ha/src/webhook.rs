use axum::{Json, http::StatusCode};
use serde::{Deserialize, Serialize};

/// Incoming webhook payload from Home Assistant automations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    /// The webhook trigger ID (matches HA automation trigger).
    #[serde(default)]
    pub trigger_id: Option<String>,
    /// Entity that triggered the webhook (if entity-based).
    #[serde(default)]
    pub entity_id: Option<String>,
    /// New state value (if state-change trigger).
    #[serde(default)]
    pub new_state: Option<String>,
    /// Old state value (if state-change trigger).
    #[serde(default)]
    pub old_state: Option<String>,
    /// Arbitrary data payload from the automation.
    #[serde(default)]
    pub data: serde_json::Value,
}

/// Response to send back to HA after processing a webhook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookResponse {
    pub status: String,
    #[serde(default)]
    pub message: Option<String>,
}

impl WebhookResponse {
    pub fn ok() -> Self {
        Self {
            status: "ok".to_string(),
            message: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            message: Some(msg.into()),
        }
    }
}

/// Axum handler for incoming HA webhooks.
/// Mount at POST /api/v1/webhook on Odin.
pub async fn handle_webhook(
    Json(payload): Json<WebhookPayload>,
) -> (StatusCode, Json<WebhookResponse>) {
    tracing::info!(
        trigger_id = ?payload.trigger_id,
        entity_id = ?payload.entity_id,
        new_state = ?payload.new_state,
        "received HA webhook"
    );

    // Process the webhook based on trigger type
    match payload.trigger_id.as_deref() {
        Some("motion_detected") => {
            tracing::info!(entity = ?payload.entity_id, "motion detected event");
        }
        Some("door_opened") => {
            tracing::info!(entity = ?payload.entity_id, "door opened event");
        }
        Some(trigger) => {
            tracing::info!(trigger, "unhandled webhook trigger");
        }
        None => {
            tracing::debug!("webhook with no trigger_id");
        }
    }

    (StatusCode::OK, Json(WebhookResponse::ok()))
}
