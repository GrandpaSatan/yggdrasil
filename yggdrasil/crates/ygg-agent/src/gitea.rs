use std::time::Duration;

use serde::Deserialize;
use tracing::debug;

/// Gitea REST API client.
pub struct GiteaClient {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

#[derive(Debug, Deserialize)]
pub struct GiteaRepo {
    pub full_name: String,
    pub clone_url: String,
    pub default_branch: String,
}

impl GiteaClient {
    pub fn new(base_url: String, token: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        }
    }

    /// List repositories accessible to the authenticated user.
    pub async fn list_repos(&self) -> Result<Vec<GiteaRepo>, GiteaError> {
        let url = format!("{}/api/v1/repos/search", self.base_url);

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .map_err(|e| GiteaError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GiteaError::Api {
                message: body,
                status,
            });
        }

        #[derive(Deserialize)]
        struct SearchResult {
            data: Vec<GiteaRepo>,
        }

        let result: SearchResult = resp
            .json()
            .await
            .map_err(|e| GiteaError::Network(e.to_string()))?;

        Ok(result.data)
    }

    /// Get a specific repository.
    pub async fn get_repo(&self, owner: &str, repo: &str) -> Result<GiteaRepo, GiteaError> {
        let url = format!("{}/api/v1/repos/{}/{}", self.base_url, owner, repo);

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .map_err(|e| GiteaError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GiteaError::Api {
                message: body,
                status,
            });
        }

        resp.json()
            .await
            .map_err(|e| GiteaError::Network(e.to_string()))
    }

    /// Create a pull request.
    pub async fn create_pr(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<(), GiteaError> {
        let url = format!("{}/api/v1/repos/{}/{}/pulls", self.base_url, owner, repo);

        let payload = serde_json::json!({
            "title": title,
            "body": body,
            "head": head,
            "base": base,
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("token {}", self.token))
            .json(&payload)
            .send()
            .await
            .map_err(|e| GiteaError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GiteaError::Api {
                message: body,
                status,
            });
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GiteaError {
    #[error("Gitea API error (HTTP {status}): {message}")]
    Api { message: String, status: u16 },

    #[error("network error: {0}")]
    Network(String),
}
