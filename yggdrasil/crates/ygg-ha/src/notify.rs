use serde::{Deserialize, Serialize};
use tracing::info;

use crate::client::HaClient;
use crate::error::HaError;

/// Notification payload for mobile push via HA Companion App.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub title: String,
    pub message: String,
    /// Optional notification priority: "high", "default", "low".
    #[serde(default)]
    pub priority: Option<String>,
    /// Optional notification channel (Android).
    #[serde(default)]
    pub channel: Option<String>,
    /// Optional image URL to include.
    #[serde(default)]
    pub image: Option<String>,
    /// Optional actions (interactive buttons).
    #[serde(default)]
    pub actions: Vec<NotificationAction>,
}

/// An interactive action button on a notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationAction {
    pub action: String,
    pub title: String,
    #[serde(default)]
    pub uri: Option<String>,
}

impl HaClient {
    /// Send a push notification via HA's mobile app notification service.
    ///
    /// `target` is the HA notify service name, e.g. "mobile_app_pixel_8".
    pub async fn send_notification(
        &self,
        target: &str,
        notification: &Notification,
    ) -> Result<(), HaError> {
        let mut data = serde_json::json!({
            "title": notification.title,
            "message": notification.message,
        });

        // Add optional fields to data map
        let data_obj = data.as_object_mut().unwrap();

        if let Some(ref priority) = notification.priority {
            data_obj.insert(
                "data".to_string(),
                serde_json::json!({
                    "priority": priority,
                }),
            );
        }

        if let Some(ref image) = notification.image {
            let data_inner = data_obj
                .entry("data")
                .or_insert_with(|| serde_json::json!({}));
            data_inner
                .as_object_mut()
                .unwrap()
                .insert("image".to_string(), serde_json::Value::String(image.clone()));
        }

        if !notification.actions.is_empty() {
            let data_inner = data_obj
                .entry("data")
                .or_insert_with(|| serde_json::json!({}));
            data_inner.as_object_mut().unwrap().insert(
                "actions".to_string(),
                serde_json::to_value(&notification.actions)
                    .unwrap_or(serde_json::Value::Array(vec![])),
            );
        }

        info!(
            target = target,
            title = %notification.title,
            "sending HA notification"
        );

        self.call_service("notify", target, data).await
    }

    /// Send a simple text notification (convenience wrapper).
    pub async fn notify_simple(
        &self,
        target: &str,
        title: &str,
        message: &str,
    ) -> Result<(), HaError> {
        self.send_notification(
            target,
            &Notification {
                title: title.to_string(),
                message: message.to_string(),
                priority: None,
                channel: None,
                image: None,
                actions: vec![],
            },
        )
        .await
    }
}
