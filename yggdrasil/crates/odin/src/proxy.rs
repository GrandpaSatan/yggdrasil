/// Ollama HTTP proxy and SSE stream conversion.
///
/// This module is the only place in Odin that communicates with Ollama's
/// `/api/chat` and `/api/tags` endpoints.  All other modules work with
/// internal types and delegate actual HTTP I/O here.
///
/// Stream conversion strategy:
///   Ollama streams newline-delimited JSON objects. Each object is parsed into
///   `OllamaStreamLine` and then converted to an OpenAI `ChatCompletionChunk`
///   serialised as SSE `data: <json>\n\n`.  After the final line (`done:true`),
///   the sentinel `data: [DONE]\n\n` is emitted.
///
/// Timeout: the caller sets a 120-second timeout on the reqwest client
///   connection so that long inference runs are not prematurely killed, but
///   the initial connection to Ollama will still fail fast if the host is down.
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{body::Bytes, response::sse::Event};
use futures::{Stream, StreamExt};

use crate::error::OdinError;
use crate::openai::{
    ChatCompletionChunk, ChatCompletionResponse, ChunkChoice, Choice, Delta, OllamaChatRequest,
    OllamaStreamLine, OllamaTagsResponse, Role, Usage,
};

/// Handle returned by streaming functions, containing both the SSE stream
/// and a oneshot receiver that delivers the full accumulated response text
/// after the stream completes. Used for engram storage and session updates.
pub struct StreamHandle<S> {
    pub stream: S,
    pub completion_rx: tokio::sync::oneshot::Receiver<String>,
}

// ─────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────

/// Current Unix timestamp in seconds.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ─────────────────────────────────────────────────────────────────
// Streaming chat
// ─────────────────────────────────────────────────────────────────

/// Stream a chat completion from Ollama, converting newline-delimited JSON to
/// OpenAI-compatible SSE events.
///
/// Returns a `Stream` of `Result<Event, OdinError>` for use with
/// `axum::response::sse::Sse::new`.
///
/// Takes an owned `reqwest::Client` (cheap clone — internally Arc'd) and an
/// owned `String` backend URL so the returned stream has no lifetime
/// dependencies and satisfies `Sse::new`'s `'static` bound.
///
/// The returned stream:
///   1. Yields one `Event` per Ollama JSON line (content delta chunk).
///   2. Yields a final `Event` with `data: [DONE]` after the done-line.
pub async fn stream_chat(
    client: reqwest::Client,
    backend_url: String,
    request: OllamaChatRequest,
    completion_id: String,
) -> Result<StreamHandle<impl Stream<Item = Result<Event, OdinError>>>, OdinError> {
    let url = format!("{backend_url}/api/chat");

    tracing::debug!(url = %url, model = %request.model, "streaming chat request to Ollama");

    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await
        .map_err(|e| OdinError::Upstream(format!("ollama connection failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(OdinError::Upstream(format!(
            "ollama returned {status}: {body}"
        )));
    }

    let model_name = request.model.clone();
    let mut byte_buf: Vec<u8> = Vec::new();
    let mut is_first_chunk = true;
    let byte_stream = response.bytes_stream();

    // Accumulator for the full response text + oneshot to deliver it.
    let accumulator: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let acc_clone = accumulator.clone();
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel::<String>();
    let mut completion_tx = Some(completion_tx);

    let event_stream = byte_stream
        .map(move |chunk_result| -> Vec<Result<Event, OdinError>> {
            let bytes: Bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    return vec![Err(OdinError::Upstream(format!(
                        "stream read error: {e}"
                    )))];
                }
            };
            byte_buf.extend_from_slice(&bytes);

            const MAX_LINE_BUF: usize = 10 * 1024 * 1024;
            if byte_buf.len() > MAX_LINE_BUF {
                byte_buf.clear();
                return vec![Err(OdinError::Upstream(
                    "stream line buffer exceeded 10MB — aborting".to_string(),
                ))];
            }

            let mut events = Vec::new();
            while let Some(pos) = byte_buf.iter().position(|&b| b == b'\n') {
                let line_bytes = byte_buf.drain(..=pos).collect::<Vec<u8>>();
                let line = String::from_utf8_lossy(&line_bytes).trim().to_string();

                if line.is_empty() {
                    continue;
                }

                let stream_line: OllamaStreamLine = match serde_json::from_str(&line) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(line = %line, error = %e, "failed to parse Ollama stream line — skipping");
                        continue;
                    }
                };

                let done = stream_line.done;
                let content = stream_line.message.content.clone();

                // Accumulate content tokens for post-stream engram storage.
                if !content.is_empty()
                    && let Ok(mut acc) = acc_clone.lock() {
                        acc.push_str(&content);
                    }

                let delta = if is_first_chunk {
                    is_first_chunk = false;
                    Delta {
                        role: Some(Role::Assistant),
                        content: if content.is_empty() { None } else { Some(content) },
                    }
                } else {
                    Delta {
                        role: None,
                        content: if content.is_empty() { None } else { Some(content) },
                    }
                };

                let finish_reason = if done {
                    Some("stop".to_string())
                } else {
                    None
                };

                let chunk = ChatCompletionChunk {
                    id: completion_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created: unix_now(),
                    model: model_name.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta,
                        finish_reason,
                    }],
                };

                let json = match serde_json::to_string(&chunk) {
                    Ok(j) => j,
                    Err(e) => {
                        tracing::error!(error = %e, "failed to serialise chunk — skipping");
                        continue;
                    }
                };

                events.push(Ok(Event::default().data(json)));

                if done {
                    events.push(Ok(Event::default().data("[DONE]")));
                    // Send the accumulated text to the handler via oneshot.
                    if let Some(tx) = completion_tx.take() {
                        let text = acc_clone.lock().map(|g| g.clone()).unwrap_or_default();
                        let _ = tx.send(text);
                    }
                }
            }

            events
        })
        .flat_map(futures::stream::iter);

    Ok(StreamHandle { stream: event_stream, completion_rx })
}

// ─────────────────────────────────────────────────────────────────
// Non-streaming chat
// ─────────────────────────────────────────────────────────────────

/// Generate a non-streaming chat completion from Ollama.
///
/// POSTs to `{backend_url}/api/chat` with `stream: false` and accumulates
/// the single-object response into a `ChatCompletionResponse`.
pub async fn generate_chat(
    client: &reqwest::Client,
    backend_url: &str,
    request: OllamaChatRequest,
    completion_id: &str,
) -> Result<ChatCompletionResponse, OdinError> {
    let url = format!("{backend_url}/api/chat");

    tracing::debug!(url = %url, model = %request.model, "non-streaming chat request to Ollama");

    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await
        .map_err(|e| OdinError::Upstream(format!("ollama connection failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(OdinError::Upstream(format!(
            "ollama returned {status}: {body}"
        )));
    }

    let stream_line: OllamaStreamLine = response
        .json()
        .await
        .map_err(|e| OdinError::Upstream(format!("failed to parse Ollama response: {e}")))?;

    // Build Usage from Ollama's token counts if available.
    let usage = match (stream_line.prompt_eval_count, stream_line.eval_count) {
        (Some(prompt), Some(completion)) => Some(Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
        }),
        _ => None,
    };

    Ok(ChatCompletionResponse {
        id: completion_id.to_string(),
        object: "chat.completion".to_string(),
        created: unix_now(),
        model: stream_line.model,
        choices: vec![Choice {
            index: 0,
            message: crate::openai::ChatMessage {
                role: Role::Assistant,
                content: stream_line.message.content,
            },
            finish_reason: Some("stop".to_string()),
        }],
        usage,
    })
}

// ─────────────────────────────────────────────────────────────────
// Streaming chat (OpenAI-compatible backend)
// ─────────────────────────────────────────────────────────────────

/// Stream a chat completion from an OpenAI-compatible backend (e.g. vLLM).
///
/// The upstream already emits SSE in the correct OpenAI format (`data: {...}\n\n`
/// terminated by `data: [DONE]\n\n`).  We parse each `data:` line and re-emit
/// it as an `axum::response::sse::Event` so the response passes through Odin's
/// SSE plumbing unchanged.
pub async fn stream_chat_openai(
    client: reqwest::Client,
    backend_url: String,
    request: crate::openai::ChatCompletionRequest,
) -> Result<StreamHandle<impl Stream<Item = Result<Event, OdinError>>>, OdinError> {
    let url = format!("{backend_url}/v1/chat/completions");

    tracing::debug!(url = %url, "streaming chat request to OpenAI-compatible backend");

    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await
        .map_err(|e| OdinError::Upstream(format!("openai backend connection failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(OdinError::Upstream(format!(
            "openai backend returned {status}: {body}"
        )));
    }

    let mut byte_buf: Vec<u8> = Vec::new();
    let byte_stream = response.bytes_stream();

    // Accumulator for the full response text + oneshot to deliver it.
    let accumulator: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let acc_clone = accumulator.clone();
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel::<String>();
    let mut completion_tx = Some(completion_tx);

    let event_stream = byte_stream
        .map(move |chunk_result| -> Vec<Result<Event, OdinError>> {
            let bytes: Bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    return vec![Err(OdinError::Upstream(format!(
                        "stream read error: {e}"
                    )))];
                }
            };
            byte_buf.extend_from_slice(&bytes);

            const MAX_LINE_BUF: usize = 10 * 1024 * 1024;
            if byte_buf.len() > MAX_LINE_BUF {
                byte_buf.clear();
                return vec![Err(OdinError::Upstream(
                    "stream line buffer exceeded 10MB — aborting".to_string(),
                ))];
            }

            let mut events = Vec::new();
            while let Some(pos) = byte_buf.iter().position(|&b| b == b'\n') {
                let line_bytes = byte_buf.drain(..=pos).collect::<Vec<u8>>();
                let line = String::from_utf8_lossy(&line_bytes).trim().to_string();

                if line.is_empty() {
                    continue;
                }

                // SSE lines are prefixed with "data: "
                let data = if let Some(stripped) = line.strip_prefix("data: ") {
                    stripped.to_string()
                } else if let Some(stripped) = line.strip_prefix("data:") {
                    stripped.to_string()
                } else {
                    // Skip non-data SSE lines (e.g. comments, event types)
                    continue;
                };

                // Accumulate content from OpenAI-format chunk deltas.
                if data != "[DONE]"
                    && let Ok(chunk) = serde_json::from_str::<ChatCompletionChunk>(&data)
                        && let Some(choice) = chunk.choices.first()
                            && let Some(ref content) = choice.delta.content
                                && let Ok(mut acc) = acc_clone.lock() {
                                    acc.push_str(content);
                                }

                // Pass through as-is — the upstream format is already OpenAI-compatible.
                events.push(Ok(Event::default().data(data)));

                if line.contains("[DONE]") {
                    // Send accumulated text via oneshot.
                    if let Some(tx) = completion_tx.take() {
                        let text = acc_clone.lock().map(|g| g.clone()).unwrap_or_default();
                        let _ = tx.send(text);
                    }
                    break;
                }
            }

            events
        })
        .flat_map(futures::stream::iter);

    Ok(StreamHandle { stream: event_stream, completion_rx })
}

// ─────────────────────────────────────────────────────────────────
// Non-streaming chat (OpenAI-compatible backend)
// ─────────────────────────────────────────────────────────────────

/// Generate a non-streaming chat completion from an OpenAI-compatible backend.
///
/// POSTs to `/v1/chat/completions` with `stream: false`.  The response is
/// already in OpenAI format so we deserialize directly into `ChatCompletionResponse`.
pub async fn generate_chat_openai(
    client: &reqwest::Client,
    backend_url: &str,
    request: crate::openai::ChatCompletionRequest,
) -> Result<crate::openai::ChatCompletionResponse, OdinError> {
    let url = format!("{backend_url}/v1/chat/completions");

    tracing::debug!(url = %url, "non-streaming chat request to OpenAI-compatible backend");

    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await
        .map_err(|e| OdinError::Upstream(format!("openai backend connection failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(OdinError::Upstream(format!(
            "openai backend returned {status}: {body}"
        )));
    }

    response
        .json::<crate::openai::ChatCompletionResponse>()
        .await
        .map_err(|e| OdinError::Upstream(format!("failed to parse openai response: {e}")))
}

// ─────────────────────────────────────────────────────────────────
// Model listing (OpenAI-compatible backend)
// ─────────────────────────────────────────────────────────────────

/// Fetch the model list from an OpenAI-compatible backend via `GET /v1/models`.
///
/// Returns the model IDs as a `Vec<String>`.
pub async fn list_models_openai(
    client: &reqwest::Client,
    backend_url: &str,
) -> Result<Vec<String>, OdinError> {
    let url = format!("{backend_url}/v1/models");

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| OdinError::Upstream(format!("openai /v1/models failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        return Err(OdinError::Upstream(format!(
            "openai /v1/models returned {status}"
        )));
    }

    let model_list: crate::openai::ModelList = response
        .json()
        .await
        .map_err(|e| OdinError::Upstream(format!("failed to parse /v1/models response: {e}")))?;

    Ok(model_list.data.into_iter().map(|m| m.id).collect())
}

// ─────────────────────────────────────────────────────────────────
// Model listing (Ollama)
// ─────────────────────────────────────────────────────────────────

/// Fetch the model list from an Ollama backend via `GET /api/tags`.
pub async fn list_models(
    client: &reqwest::Client,
    backend_url: &str,
) -> Result<OllamaTagsResponse, OdinError> {
    let url = format!("{backend_url}/api/tags");

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| OdinError::Upstream(format!("ollama /api/tags failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        return Err(OdinError::Upstream(format!(
            "ollama /api/tags returned {status}"
        )));
    }

    response
        .json::<OllamaTagsResponse>()
        .await
        .map_err(|e| OdinError::Upstream(format!("failed to parse /api/tags response: {e}")))
}
