use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::adapter::{
    ChatRequest, ChatResponse, CloudAdapter, CloudError, CloudProvider, ModelInfo, TokenUsage,
};
use crate::RateLimiter;

/// OpenAI-compatible cloud adapter.
pub struct OpenAiAdapter {
    client: Client,
    api_key: String,
    base_url: String,
    rate_limiter: RateLimiter,
}

impl OpenAiAdapter {
    pub fn new(api_key: String, requests_per_minute: u32) -> Result<Self, CloudError> {
        if api_key.is_empty() {
            return Err(CloudError::MissingApiKey(CloudProvider::Openai));
        }
        Ok(Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
            rate_limiter: RateLimiter::new(requests_per_minute, CloudProvider::Openai),
        })
    }

    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }
}

#[derive(Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    model: String,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModel>,
}

#[derive(Deserialize)]
struct OpenAiModel {
    id: String,
}

#[async_trait]
impl CloudAdapter for OpenAiAdapter {
    async fn chat_completion(&self, req: ChatRequest) -> Result<ChatResponse, CloudError> {
        self.rate_limiter.acquire().await;

        let oai_req = OpenAiRequest {
            model: req.model.clone(),
            messages: req
                .messages
                .into_iter()
                .map(|m| OpenAiMessage {
                    role: m.role,
                    content: m.content,
                })
                .collect(),
            temperature: req.temperature,
            max_tokens: req.max_tokens,
        };

        debug!(model = %req.model, "sending OpenAI chat completion");

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&oai_req)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status == 429 {
            return Err(CloudError::RateLimited {
                provider: CloudProvider::Openai,
                retry_after_secs: 60,
            });
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CloudError::Api {
                provider: CloudProvider::Openai,
                message: body,
                status,
            });
        }

        let oai_resp: OpenAiResponse = resp
            .json()
            .await
            .map_err(|e| CloudError::Deserialize(e.to_string()))?;

        let content = oai_resp
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();

        let usage = oai_resp.usage.map(|u| TokenUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }).unwrap_or_default();

        Ok(ChatResponse {
            content,
            model: oai_resp.model,
            provider: CloudProvider::Openai,
            usage,
        })
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, CloudError> {
        let resp = self
            .client
            .get(format!("{}/models", self.base_url))
            .bearer_auth(&self.api_key)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CloudError::Api {
                provider: CloudProvider::Openai,
                message: body,
                status,
            });
        }

        let models: OpenAiModelsResponse = resp
            .json()
            .await
            .map_err(|e| CloudError::Deserialize(e.to_string()))?;

        Ok(models
            .data
            .into_iter()
            .map(|m| ModelInfo {
                id: m.id,
                provider: CloudProvider::Openai,
                context_window: 128000,
            })
            .collect())
    }

    fn provider(&self) -> CloudProvider {
        CloudProvider::Openai
    }
}
