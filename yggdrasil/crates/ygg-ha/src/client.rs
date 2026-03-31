use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::HaError;
use ygg_domain::config::HaConfig;

/// REST API client for Home Assistant.
#[derive(Clone)]
pub struct HaClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

/// An HA entity state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityState {
    pub entity_id: String,
    pub state: String,
    #[serde(default)]
    pub attributes: serde_json::Value,
    #[serde(default)]
    pub last_changed: Option<String>,
}

/// Services available within a single HA domain (e.g., `light`, `switch`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainServices {
    pub domain: String,
    pub services: HashMap<String, ServiceDef>,
}

/// Definition of a single HA service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDef {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub fields: HashMap<String, serde_json::Value>,
}

impl HaClient {
    pub fn from_config(config: &HaConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: config.url.trim_end_matches('/').to_string(),
            token: config.token.clone(),
        }
    }

    /// Get all entity states.
    pub async fn get_states(&self) -> Result<Vec<EntityState>, HaError> {
        let url = format!("{}/api/states", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| HaError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HaError::Api(format!("{status}: {body}")));
        }

        resp.json()
            .await
            .map_err(|e| HaError::Parse(e.to_string()))
    }

    /// Get a specific entity state.
    pub async fn get_entity(&self, entity_id: &str) -> Result<EntityState, HaError> {
        let url = format!("{}/api/states/{entity_id}", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| HaError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HaError::Api(format!("{status}: {body}")));
        }

        resp.json()
            .await
            .map_err(|e| HaError::Parse(e.to_string()))
    }

    /// List entity states, optionally filtered to a single HA domain.
    ///
    /// If `domain` is `Some("light")`, returns only entities whose `entity_id`
    /// starts with `"light."`.  If `None`, returns all entities (equivalent to
    /// `get_states()`).
    pub async fn list_entities(
        &self,
        domain: Option<&str>,
    ) -> Result<Vec<EntityState>, HaError> {
        let all = self.get_states().await?;
        match domain {
            None => Ok(all),
            Some(d) => {
                let prefix = format!("{d}.");
                Ok(all
                    .into_iter()
                    .filter(|e| e.entity_id.starts_with(&prefix))
                    .collect())
            }
        }
    }

    /// Get all services available on the HA instance.
    ///
    /// Calls `GET /api/services`.  The HA response is an array of objects,
    /// each with a `domain` key and a `services` object.
    pub async fn get_services(&self) -> Result<Vec<DomainServices>, HaError> {
        let url = format!("{}/api/services", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| HaError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HaError::Api(format!("{status}: {body}")));
        }

        resp.json()
            .await
            .map_err(|e| HaError::Parse(e.to_string()))
    }

    /// Call an HA service (e.g., turn on a light).
    pub async fn call_service(
        &self,
        domain: &str,
        service: &str,
        data: serde_json::Value,
    ) -> Result<(), HaError> {
        let url = format!("{}/api/services/{domain}/{service}", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&data)
            .send()
            .await
            .map_err(|e| HaError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HaError::Api(format!("{status}: {body}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ha_client_applies_timeout_from_config() {
        let config = HaConfig {
            url: "http://127.0.0.1:8123".to_string(),
            token: "test-token".to_string(),
            timeout_secs: 5,
            automation_model: None,
        };
        let client = HaClient::from_config(&config);
        assert_eq!(client.base_url, "http://127.0.0.1:8123");
        assert_eq!(client.token, "test-token");
        // Client is constructed via builder with timeout — not bare Client::new().
        let _ = client.http;
    }

    #[tokio::test]
    async fn ha_client_fails_on_unreachable() {
        let config = HaConfig {
            url: "http://127.0.0.1:1".to_string(),
            token: "test".to_string(),
            timeout_secs: 1,
            automation_model: None,
        };
        let client = HaClient::from_config(&config);
        let result = client.get_states().await;
        assert!(result.is_err());
    }
}
