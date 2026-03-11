/// OpenAI-compatible API types for Odin's external surface.
///
/// This is a pure type-definition module with no I/O or business logic.
/// All types derive Serialize + Deserialize and correspond exactly to the
/// OpenAI Chat Completions v1 schema so that clients can treat Odin as an
/// OpenAI-compatible endpoint without modification.
///
/// Ollama-native request/response types are also defined here so that
/// `proxy.rs` can convert between the two formats without depending on any
/// other Odin module.
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────
// OpenAI request types
// ─────────────────────────────────────────────────────────────────

/// Role of a participant in the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System => f.write_str("system"),
            Self::User => f.write_str("user"),
            Self::Assistant => f.write_str("assistant"),
        }
    }
}

/// A single message in the conversation.
///
/// `content` accepts both a plain string and an array of content parts
/// (`[{"type":"text","text":"..."}]`) as per the OpenAI spec.  Array
/// content is flattened to a single string at deserialization time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(deserialize_with = "deserialize_content")]
    pub content: String,
}

/// Deserialize `content` from either a plain string or an array of parts.
fn deserialize_content<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ContentValue {
        String(String),
        Parts(Vec<ContentPart>),
    }

    #[derive(Deserialize)]
    struct ContentPart {
        #[serde(default)]
        text: Option<String>,
    }

    match ContentValue::deserialize(deserializer)? {
        ContentValue::String(s) => Ok(s),
        ContentValue::Parts(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.text {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&text);
                }
            }
            Ok(out)
        }
    }
}

/// OpenAI `/v1/chat/completions` request body.
///
/// `stream` defaults to `true` matching the sprint spec (clients that omit
/// the field get SSE streaming by default).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    /// Optional model override. When absent, the semantic router decides.
    #[serde(default)]
    pub model: Option<String>,
    /// Conversation history. Must be non-empty.
    pub messages: Vec<ChatMessage>,
    /// Whether to stream the response via SSE. Defaults to `true`.
    #[serde(default = "default_stream")]
    pub stream: bool,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
    /// Optional session ID for stateful multi-turn conversations.
    /// When provided, Odin tracks conversation history server-side so
    /// clients can send only new messages instead of the full history.
    /// When absent, Odin operates in stateless mode (standard OpenAI behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Optional project ID for project-scoped session history.
    /// When provided alongside session_id, Odin injects previous session summaries
    /// for this project as low-priority context, enabling cross-window continuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

fn default_stream() -> bool {
    true
}

// ─────────────────────────────────────────────────────────────────
// OpenAI non-streaming response types
// ─────────────────────────────────────────────────────────────────

/// OpenAI `/v1/chat/completions` non-streaming response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// A single completion choice inside `ChatCompletionResponse`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

// ─────────────────────────────────────────────────────────────────
// OpenAI streaming (SSE) chunk types
// ─────────────────────────────────────────────────────────────────

/// A single SSE data frame for streaming chat completions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

/// A single choice delta inside `ChatCompletionChunk`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

/// Incremental content in a streaming chunk.
///
/// Both fields are optional: the first chunk typically carries `role` only,
/// subsequent chunks carry `content` only, the final chunk may carry neither.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// ─────────────────────────────────────────────────────────────────
// OpenAI model listing types
// ─────────────────────────────────────────────────────────────────

/// OpenAI `/v1/models` response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelList {
    pub object: String,
    pub data: Vec<Model>,
}

/// A single model entry in `ModelList`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}

// ─────────────────────────────────────────────────────────────────
// Ollama upstream types (internal — not exposed in HTTP responses)
// ─────────────────────────────────────────────────────────────────

/// Body sent to Ollama `POST /api/chat`.
#[derive(Debug, Clone, Serialize)]
pub struct OllamaChatRequest {
    pub model: String,
    pub messages: Vec<OllamaMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<OllamaOptions>,
}

/// A single message in the Ollama format.
///
/// Ollama uses a plain string for role rather than an enum, so we keep it as
/// `String` to avoid any serialisation mismatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaMessage {
    pub role: String,
    pub content: String,
}

/// Ollama model-level generation options.
#[derive(Debug, Clone, Serialize)]
pub struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_predict: Option<u64>,
    /// Context window size in tokens. Overrides the model's default context
    /// length for this request, allowing per-backend context control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_ctx: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

/// One line of Ollama's newline-delimited streaming response.
///
/// Both streaming and non-streaming modes use the same JSON shape; the
/// difference is that non-streaming sends a single object with `done: true`
/// while streaming sends many objects with `done: false` followed by a
/// final object with `done: true`.
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaStreamLine {
    pub model: String,
    pub message: OllamaMessage,
    pub done: bool,
    #[serde(default)]
    pub total_duration: Option<u64>,
    #[serde(default)]
    pub eval_count: Option<u64>,
    #[serde(default)]
    pub prompt_eval_count: Option<u64>,
}

/// Ollama `GET /api/tags` response.
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaTagsResponse {
    pub models: Vec<OllamaModelInfo>,
}

/// Metadata for a single model reported by Ollama.
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaModelInfo {
    pub name: String,
    /// Ignored but must exist in the JSON to deserialise cleanly.
    #[allow(dead_code)]
    pub modified_at: Option<String>,
    #[allow(dead_code)]
    pub size: Option<u64>,
}
