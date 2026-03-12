use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::adapter::{
    ChatRequest, ChatResponse, CloudAdapter, CloudError, CloudProvider, ModelInfo,
    TokenUsage,
};
use crate::RateLimiter;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic Claude cloud adapter.
pub struct ClaudeAdapter {
    client: Client,
    api_key: String,
    rate_limiter: RateLimiter,
}

impl ClaudeAdapter {
    pub fn new(api_key: String, requests_per_minute: u32) -> Result<Self, CloudError> {
        if api_key.is_empty() {
            return Err(CloudError::MissingApiKey(CloudProvider::Claude));
        }
        Ok(Self {
            client: Client::new(),
            api_key,
            rate_limiter: RateLimiter::new(requests_per_minute, CloudProvider::Claude),
        })
    }
}

#[derive(Serialize)]
struct ClaudeRequest {
    model: String,
    messages: Vec<ClaudeMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Serialize, Deserialize)]
struct ClaudeMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ClaudeResponse {
    content: Vec<ClaudeContent>,
    model: String,
    usage: ClaudeUsage,
}

#[derive(Deserialize)]
struct ClaudeContent {
    text: String,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[async_trait]
impl CloudAdapter for ClaudeAdapter {
    async fn chat_completion(&self, req: ChatRequest) -> Result<ChatResponse, CloudError> {
        self.rate_limiter.acquire().await;

        // Claude API expects system message separately; extract it
        let messages: Vec<ClaudeMessage> = req
            .messages
            .into_iter()
            .filter(|m| m.role != "system")
            .map(|m| ClaudeMessage {
                role: m.role,
                content: m.content,
            })
            .collect();

        let claude_req = ClaudeRequest {
            model: req.model.clone(),
            messages,
            max_tokens: req.max_tokens.unwrap_or(4096),
            temperature: req.temperature,
        };

        debug!(model = %req.model, "sending Claude chat completion");

        let resp = self
            .client
            .post(format!("{}/messages", ANTHROPIC_API_URL))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&claude_req)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status == 429 {
            return Err(CloudError::RateLimited {
                provider: CloudProvider::Claude,
                retry_after_secs: 60,
            });
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CloudError::Api {
                provider: CloudProvider::Claude,
                message: body,
                status,
            });
        }

        let claude_resp: ClaudeResponse = resp
            .json()
            .await
            .map_err(|e| CloudError::Deserialize(e.to_string()))?;

        let content = claude_resp
            .content
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>()
            .join("");

        let usage = TokenUsage {
            prompt_tokens: claude_resp.usage.input_tokens,
            completion_tokens: claude_resp.usage.output_tokens,
            total_tokens: claude_resp.usage.input_tokens + claude_resp.usage.output_tokens,
        };

        Ok(ChatResponse {
            content,
            model: claude_resp.model,
            provider: CloudProvider::Claude,
            usage,
        })
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, CloudError> {
        // Claude doesn't have a models list endpoint; return known models
        Ok(vec![
            ModelInfo {
                id: "claude-sonnet-4-6".to_string(),
                provider: CloudProvider::Claude,
                context_window: 200000,
            },
            ModelInfo {
                id: "claude-haiku-4-5-20251001".to_string(),
                provider: CloudProvider::Claude,
                context_window: 200000,
            },
        ])
    }

    fn provider(&self) -> CloudProvider {
        CloudProvider::Claude
    }
}
