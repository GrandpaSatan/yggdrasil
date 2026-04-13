/// Camera watch — motion-triggered vision analysis via Gemma 4 E4B (Sprint 057).
///
/// When a Wyze camera detects motion, HA sends a webhook to Odin. This module:
/// 1. Fetches a JPEG snapshot from wyze-bridge
/// 2. Base64-encodes it and sends to the perceive flow (Gemma 4 E4B)
/// 3. Analyzes the response for importance (delivery, person, package, etc.)
/// 4. Sends a push notification via HA if important
/// 5. Stores the event in engram memory for pattern learning
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use base64::Engine;
use tracing::{info, warn};

use crate::error::OdinError;
use crate::state::AppState;

/// Result of analyzing a camera snapshot.
#[derive(Debug)]
pub struct CameraAnalysis {
    pub camera: String,
    pub label: String,
    pub description: String,
    pub is_important: bool,
}

/// Per-camera cooldown tracker to prevent notification spam.
pub struct CooldownTracker {
    last_notify: Mutex<HashMap<String, Instant>>,
}

impl CooldownTracker {
    pub fn new() -> Self {
        Self {
            last_notify: Mutex::new(HashMap::new()),
        }
    }

    /// Returns true if the camera is past its cooldown period.
    pub fn check_and_set(&self, camera: &str, cooldown_secs: u64) -> bool {
        let mut map = self.last_notify.lock().unwrap();
        let now = Instant::now();
        if let Some(last) = map.get(camera) {
            if now.duration_since(*last).as_secs() < cooldown_secs {
                return false;
            }
        }
        map.insert(camera.to_string(), now);
        true
    }
}

/// Handle a motion detection event from a Wyze camera.
///
/// Fetches a snapshot, analyzes it with Gemma 4 E4B, and notifies if important.
pub async fn handle_motion_event(
    state: &AppState,
    camera_name: &str,
) -> Result<CameraAnalysis, OdinError> {
    let camera_config = state
        .config
        .cameras
        .as_ref()
        .ok_or_else(|| OdinError::BadRequest("cameras not configured".into()))?;

    // Look up the camera entry for its human label.
    let camera_entry = camera_config
        .cameras
        .iter()
        .find(|c| c.name == camera_name);
    let label = camera_entry
        .map(|c| c.label.as_str())
        .unwrap_or(camera_name);

    info!(camera = camera_name, label, "processing motion event");

    // ── 1. Check cooldown ──────────────────────────────────────────
    if !state.camera_cooldown.check_and_set(camera_name, camera_config.cooldown_secs) {
        info!(camera = camera_name, "skipping — within cooldown period");
        return Ok(CameraAnalysis {
            camera: camera_name.to_string(),
            label: label.to_string(),
            description: "Skipped (cooldown)".into(),
            is_important: false,
        });
    }

    // ── 2. Fetch snapshot from wyze-bridge ─────────────────────────
    let snapshot_url = format!("{}/{}.jpg", camera_config.snapshot_base_url, camera_name);
    let snapshot_bytes = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        state.http_client.get(&snapshot_url).send(),
    )
    .await
    {
        Ok(Ok(resp)) if resp.status().is_success() => {
            resp.bytes().await.map_err(|e| {
                OdinError::Upstream(format!("failed to read snapshot bytes: {e}"))
            })?
        }
        Ok(Ok(resp)) => {
            warn!(camera = camera_name, status = %resp.status(), "snapshot fetch failed");
            return Ok(CameraAnalysis {
                camera: camera_name.to_string(),
                label: label.to_string(),
                description: format!("Camera offline (HTTP {})", resp.status()),
                is_important: false,
            });
        }
        Ok(Err(e)) => {
            warn!(camera = camera_name, error = %e, "snapshot fetch error");
            return Ok(CameraAnalysis {
                camera: camera_name.to_string(),
                label: label.to_string(),
                description: format!("Camera unreachable: {e}"),
                is_important: false,
            });
        }
        Err(_) => {
            warn!(camera = camera_name, "snapshot fetch timed out (5s)");
            return Ok(CameraAnalysis {
                camera: camera_name.to_string(),
                label: label.to_string(),
                description: "Camera offline (timeout)".into(),
                is_important: false,
            });
        }
    };

    let image_b64 = base64::engine::general_purpose::STANDARD.encode(&snapshot_bytes);
    info!(camera = camera_name, bytes = snapshot_bytes.len(), "snapshot fetched");

    // ── 3. Analyze with perceive flow (Gemma 4 E4B) ───────────────
    let flows_snapshot = state.flows.read().unwrap().clone();
    let flow = state
        .flow_engine
        .find_by_modality(&flows_snapshot, "omni")
        .ok_or_else(|| OdinError::BadRequest("no 'omni' modality flow configured".into()))?;

    let prompt = format!(
        "This is a security camera image from the {} camera. \
         Describe what you see: who or what is there, what are they doing? \
         End your response with one of these tags:\n\
         [IMPORTANT] — if there's a delivery, package, person at the door, unknown visitor, or anything the homeowner should know about\n\
         [ROUTINE] — if it's nothing notable (empty scene, pet, familiar car, wind/shadow)\n\
         Be concise — 1-3 sentences max.",
        label
    );

    let result = state
        .flow_engine
        .execute(flow, &prompt, Some(&[image_b64]), Some(state))
        .await?;

    let response_text = result.final_output().to_string();
    let is_important = response_text.contains("[IMPORTANT]");
    let description = response_text
        .replace("[IMPORTANT]", "")
        .replace("[ROUTINE]", "")
        .trim()
        .to_string();

    info!(
        camera = camera_name,
        important = is_important,
        description = %description,
        "camera analysis complete"
    );

    // ── 4. Notify if important ─────────────────────────────────────
    if is_important {
        if let Some(ref ha) = state.ha_client {
            let title = format!("{} — Motion Alert", label);
            let notification = ygg_ha::Notification {
                title: title.clone(),
                message: description.clone(),
                priority: Some("high".into()),
                channel: None,
                image: Some(snapshot_url.clone()),
                actions: vec![],
            };

            if let Err(e) = ha
                .send_notification(&camera_config.notify_entity, &notification)
                .await
            {
                warn!(error = %e, "failed to send camera notification");
            } else {
                info!(camera = camera_name, target = %camera_config.notify_entity, "notification sent");
            }
        }

        // Also announce via voice if voice pipeline is active.
        if let Some(ref voice_url) = state.voice_api_url {
            let alert_text = format!("Sir, {}", description);
            let _ = state
                .http_client
                .post(format!("{voice_url}/api/v1/voice/alert"))
                .json(&serde_json::json!({"text": alert_text}))
                .send()
                .await;
        }
    }

    // ── 5. Store in memory for pattern learning ────────────────────
    let cause = format!("Motion detected on {} camera", label);
    let effect = format!(
        "Camera analysis: {}. Important: {}",
        description, is_important
    );
    let store_body = serde_json::json!({
        "cause": cause,
        "effect": effect,
        "tags": ["camera", "motion", camera_name],
    });
    let _ = state
        .http_client
        .post(format!("{}/api/v1/store", state.mimir_url))
        .json(&store_body)
        .send()
        .await;

    Ok(CameraAnalysis {
        camera: camera_name.to_string(),
        label: label.to_string(),
        description,
        is_important,
    })
}
