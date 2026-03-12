use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::adapter::{
    ChatRequest, ChatResponse, CloudAdapter, CloudError, CloudProvider, ModelInfo,
    TokenUsage,
};
use crate::RateLimiter;

const GEMINI_API_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google Gemini cloud adapter.
pub struct GeminiAdapter {
    client: Client,
    api_key: String,
    rate_limiter: RateLimiter,
}

impl GeminiAdapter {
    pub fn new(api_key: String, requests_per_minute: u32) -> Result<Self, CloudError> {
        if api_key.is_empty() {
            return Err(CloudError::MissingApiKey(CloudProvider::Gemini));
        }
        Ok(Self {
            client: Client::new(),
            api_key,
            rate_limiter: RateLimiter::new(requests_per_minute, CloudProvider::Gemini),
        })
    }
}

#[derive(Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Serialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Serialize)]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiResponseContent,
}

#[derive(Deserialize)]
struct GeminiResponseContent {
    parts: Vec<GeminiResponsePart>,
}

#[derive(Deserialize)]
struct GeminiResponsePart {
    text: String,
}

#[derive(Deserialize)]
struct GeminiUsage {
    #[serde(default)]
    prompt_token_count: u32,
    #[serde(default)]
    candidates_token_count: u32,
    #[serde(default)]
    total_token_count: u32,
}

#[derive(Deserialize)]
struct GeminiModelsResponse {
    models: Vec<GeminiModel>,
}

#[derive(Deserialize)]
struct GeminiModel {
    name: String,
    #[serde(default)]
    input_token_limit: Option<u32>,
}

#[async_trait]
impl CloudAdapter for GeminiAdapter {
    async fn chat_completion(&self, req: ChatRequest) -> Result<ChatResponse, CloudError> {
        self.rate_limiter.acquire().await;

        let contents: Vec<GeminiContent> = req
            .messages
            .into_iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "assistant" => "model".to_string(),
                    other => other.to_string(),
                };
                GeminiContent {
                    role,
                    parts: vec![GeminiPart { text: m.content }],
                }
            })
            .collect();

        let generation_config = if req.temperature.is_some() || req.max_tokens.is_some() {
            Some(GeminiGenerationConfig {
                temperature: req.temperature,
                max_output_tokens: req.max_tokens,
            })
        } else {
            None
        };

        let gemini_req = GeminiRequest {
            contents,
            generation_config,
        };

        let url = format!(
            "{}/models/{}:generateContent?key={}",
            GEMINI_API_URL, req.model, self.api_key
        );

        debug!(model = %req.model, "sending Gemini chat completion");

        let resp = self.client.post(&url).json(&gemini_req).send().await?;

        let status = resp.status().as_u16();
        if status == 429 {
            return Err(CloudError::RateLimited {
                provider: CloudProvider::Gemini,
                retry_after_secs: 60,
            });
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CloudError::Api {
                provider: CloudProvider::Gemini,
                message: body,
                status,
            });
        }

        let gemini_resp: GeminiResponse = resp
            .json()
            .await
            .map_err(|e| CloudError::Deserialize(e.to_string()))?;

        let content = gemini_resp
            .candidates
            .first()
            .and_then(|c| c.content.parts.first())
            .map(|p| p.text.clone())
            .unwrap_or_default();

        let usage = gemini_resp
            .usage_metadata
            .map(|u| TokenUsage {
                prompt_tokens: u.prompt_token_count,
                completion_tokens: u.candidates_token_count,
                total_tokens: u.total_token_count,
            })
            .unwrap_or_default();

        Ok(ChatResponse {
            content,
            model: req.model,
            provider: CloudProvider::Gemini,
            usage,
        })
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, CloudError> {
        let url = format!("{}/models?key={}", GEMINI_API_URL, self.api_key);

        let resp = self.client.get(&url).send().await?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CloudError::Api {
                provider: CloudProvider::Gemini,
                message: body,
                status,
            });
        }

        let models: GeminiModelsResponse = resp
            .json()
            .await
            .map_err(|e| CloudError::Deserialize(e.to_string()))?;

        Ok(models
            .models
            .into_iter()
            .map(|m| ModelInfo {
                id: m.name.trim_start_matches("models/").to_string(),
                provider: CloudProvider::Gemini,
                context_window: m.input_token_limit.unwrap_or(32000),
            })
            .collect())
    }

    fn provider(&self) -> CloudProvider {
        CloudProvider::Gemini
    }
}
