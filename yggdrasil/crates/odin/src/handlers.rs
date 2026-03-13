/// Axum route handlers for all Odin HTTP endpoints.
///
/// Handler responsibilities:
///   - Deserialise HTTP requests and serialise HTTP responses.
///   - Validate inputs; reject invalid requests early.
///   - Orchestrate the routing + RAG + proxy pipeline.
///   - Spawn fire-and-forget engram store tasks after completion.
///
/// Boundary rules (sprint 005):
///   - Only `proxy` communicates with Ollama.
///   - Only `rag` communicates with Muninn/Mimir for context.
///   - Transparent proxy handlers forward directly without involving `rag`.
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, sse::Sse},
    Json,
};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use ygg_domain::config::BackendType;

use crate::context::ContextBudget;
use crate::error::OdinError;
use crate::memory_router;
use crate::openai::{
    ChatCompletionRequest, ChatMessage, ModelList, Model, OllamaChatRequest,
    OllamaOptions, Role,
};
use crate::proxy;
use crate::rag;
use crate::session::CompactMessage;
use crate::state::AppState;

// ─────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Fire-and-forget: check if a session needs rolling summarization.
///
/// When total session tokens exceed 70% of the context budget, the oldest
/// half of messages are compressed into a summary via a direct backend call.
/// The summary replaces those messages in the session store, freeing token
/// budget for future turns.
fn maybe_summarize_session(
    http_client: reqwest::Client,
    session_store: crate::session::SessionStore,
    session_id: String,
    context_budget: usize,
    backend_url: String,
    backend_type: BackendType,
    model: String,
) {
    let session = match session_store.get_session(&session_id) {
        Some(s) => s,
        None => return,
    };

    let threshold = context_budget * 7 / 10;
    if session.total_tokens_estimate() < threshold {
        return;
    }

    let half = session.messages.len() / 2;
    if half < 4 {
        return; // Not enough messages to bother summarizing.
    }

    let old_messages: Vec<CompactMessage> = session.messages[..half].to_vec();
    let messages_to_consume = half;

    tokio::spawn(async move {
        let mut conversation = String::new();
        for msg in &old_messages {
            conversation.push_str(&msg.role);
            conversation.push_str(": ");
            conversation.push_str(&msg.content);
            conversation.push_str("\n\n");
        }

        let system = "You are a precise conversation summarizer. Output only the summary. Preserve key facts, decisions, code references, and context needed to continue the conversation.".to_string();
        let prompt = format!("Summarize this conversation concisely:\n\n{conversation}");
        let completion_id = format!("summary-{}", Uuid::new_v4());

        let result = match backend_type {
            BackendType::Ollama => {
                let req = OllamaChatRequest {
                    model,
                    messages: vec![
                        crate::openai::OllamaMessage { role: "system".to_string(), content: system },
                        crate::openai::OllamaMessage { role: "user".to_string(), content: prompt },
                    ],
                    stream: false,
                    options: Some(OllamaOptions {
                        temperature: Some(0.3),
                        num_predict: Some(512),
                        num_ctx: None,
                        top_p: None,
                        stop: None,
                    }),
                };
                proxy::generate_chat(&http_client, &backend_url, req, &completion_id)
                    .await
                    .map(|r| r.choices.into_iter().next().map(|c| c.message.content).unwrap_or_default())
            }
            BackendType::Openai => {
                let req = crate::openai::ChatCompletionRequest {
                    model: None,
                    messages: vec![
                        ChatMessage { role: Role::System, content: system },
                        ChatMessage { role: Role::User, content: prompt },
                    ],
                    stream: false,
                    temperature: Some(0.3),
                    max_tokens: Some(512),
                    top_p: None,
                    stop: None,
                    session_id: None,
                    project_id: None,
                };
                proxy::generate_chat_openai(&http_client, &backend_url, req)
                    .await
                    .map(|r| r.choices.into_iter().next().map(|c| c.message.content).unwrap_or_default())
            }
        };

        match result {
            Ok(summary) if !summary.is_empty() => {
                session_store.set_summary(&session_id, summary, messages_to_consume);
                tracing::info!(
                    session_id = %session_id,
                    messages_consumed = messages_to_consume,
                    "session rolling summarization complete"
                );
            }
            Ok(_) => {
                tracing::warn!(session_id = %session_id, "summarization returned empty — skipping");
            }
            Err(e) => {
                tracing::warn!(session_id = %session_id, error = %e, "session summarization failed — will retry next turn");
            }
        }
    });
}

/// Fire-and-forget: store the completed interaction as an engram in Mimir.
///
/// Spawned as a background task so the client receives its response without
/// waiting for Mimir.  Failures are logged but not propagated.
fn spawn_engram_store(
    http_client: reqwest::Client,
    mimir_url: String,
    cause: String,
    effect: String,
) {
    tokio::spawn(async move {
        let url = format!("{mimir_url}/api/v1/store");
        let body = json!({ "cause": cause, "effect": effect });
        match http_client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!("engram stored successfully");
            }
            Ok(resp) => {
                tracing::warn!(status = %resp.status(), "mimir store returned non-success status");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to store engram in mimir");
            }
        }
    });
}

// ─────────────────────────────────────────────────────────────────
// POST /v1/chat/completions
// ─────────────────────────────────────────────────────────────────

/// Chat completion handler — the core of Odin's request pipeline.
///
/// Steps:
///   1. Validate messages are non-empty.
///   2. Resolve session (create or retrieve from SessionStore).
///   3. Merge client messages into session history.
///   4. Route: explicit model override or keyword classification.
///   5. Fetch memory events from Mimir (best-effort, 3s timeout).
///   6. Apply memory events to refine the routing decision (zero-injection).
///   7. Acquire backend semaphore (fails fast with 503 if at capacity).
///   8. Fetch RAG context (parallel, best-effort, 3s timeout each).
///   9. Pack context: system prompt + summary + recent history + RAG + older history.
///  10. Dispatch to Ollama/OpenAI backend (streaming or non-streaming).
///  11. Update session with assistant response + fire-and-forget engram store.
pub async fn chat_handler(
    State(state): State<AppState>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, OdinError> {
    // ── 1. Validate ──────────────────────────────────────────────
    if request.messages.is_empty() {
        return Err(OdinError::BadRequest("messages must not be empty".to_string()));
    }

    // ── 2. Resolve session ───────────────────────────────────────
    let session_id = state
        .session_store
        .resolve(request.session_id.as_deref(), request.project_id.as_deref());

    // ── 3. Merge client messages into session history ─────────────
    // Convert incoming messages to CompactMessages and append to the session.
    let incoming: Vec<CompactMessage> = request
        .messages
        .iter()
        .filter(|m| m.role != Role::System) // Don't store client system prompts
        .map(|m| CompactMessage::new(m.role.to_string(), m.content.clone()))
        .collect();

    if !incoming.is_empty() {
        state.session_store.append_messages(&session_id, &incoming);
    }

    // ── 4. Extract last user message for routing and RAG ─────────
    let last_user_message = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.clone())
        .unwrap_or_default();

    // ── 5. Initial routing decision ───────────────────────────────
    let mut decision = if let Some(ref model) = request.model {
        state.router.resolve_backend_for_model(model).ok_or_else(|| {
            OdinError::BadRequest(format!("model not found: {model}"))
        })?
    } else {
        state.router.classify(&last_user_message)
    };

    // ── 6. Memory-event routing refinement (Sprint 015) ───────────
    if let Some(recall) = rag::fetch_memory_events(
        &state.http_client,
        &state.mimir_url,
        &last_user_message,
        state.config.mimir.query_limit,
    )
    .await
    {
        memory_router::apply_memory_events(&recall, &mut decision);

        // ── 6b. Topic drift tracking via session SDR ─────────────
        if let Some(hex) = &recall.query_sdr_hex
            && let Some(query_sdr) = ygg_domain::sdr::from_hex(hex)
                && let Some(drift) = state.session_store.update_session_sdr(&session_id, &query_sdr) {
                    tracing::debug!(
                        session_id = %session_id,
                        drift_score = %drift,
                        "session SDR drift score"
                    );
                    if drift < 0.5 {
                        tracing::info!(
                            session_id = %session_id,
                            drift_score = %drift,
                            "topic drift detected — session SDR reset"
                        );
                    }
                }
    }

    tracing::info!(
        intent = %decision.intent,
        model = %decision.model,
        backend = %decision.backend_name,
        session_id = %session_id,
        "routing decision (post memory refinement)"
    );

    // ── 7. Acquire semaphore ─────────────────────────────────────
    let backend_state = state
        .backends
        .iter()
        .find(|b| b.name == decision.backend_name)
        .ok_or_else(|| {
            OdinError::Internal(format!(
                "backend '{}' not found in state",
                decision.backend_name
            ))
        })?;

    let _permit = backend_state
        .semaphore
        .try_acquire()
        .map_err(|_| {
            OdinError::BackendUnavailable(format!(
                "backend '{}' is at capacity — try again later",
                decision.backend_name
            ))
        })?;

    // ── 8. Fetch RAG context ─────────────────────────────────────
    let span = tracing::info_span!("rag_fetch");
    let rag_context = {
        let _guard = span.enter();
        rag::fetch_context(&state, &last_user_message, &decision.intent).await
    };

    // ── 9. Pack context with session history ──────────────────────
    let backend_context_window = backend_state.context_window;
    let system_prompt = rag::build_system_prompt(&rag_context, &decision.intent);
    let session_snapshot = state.session_store.get_session(&session_id);

    // Load previous project sessions for cross-window context (lowest priority slot).
    let previous_sessions_text = request.project_id.as_deref().and_then(|pid| {
        let hist = state.session_store.get_project_history(pid, 3);
        if hist.is_empty() { None } else { Some(hist) }
    });

    let packed_messages = if let Some(ref session) = session_snapshot {
        let budget = ContextBudget {
            total_budget: backend_context_window.saturating_sub(state.config.session.generation_reserve),
            generation_reserve: state.config.session.generation_reserve,
        };
        budget.pack(session, rag_context.code_context.as_deref(), &system_prompt, previous_sessions_text.as_deref())
    } else {
        // Fallback: no session snapshot (shouldn't happen), use raw request messages.
        let mut msgs = request.messages.clone();
        if let Some(first) = msgs.first_mut() {
            if first.role == Role::System {
                first.content.push_str("\n\n");
                first.content.push_str(&system_prompt);
            } else {
                msgs.insert(0, ChatMessage { role: Role::System, content: system_prompt });
            }
        }
        msgs
    };

    let completion_id = format!("chatcmpl-{}", Uuid::new_v4());

    // ── 10. Dispatch based on backend type ────────────────────────
    // Build a response header with the session ID for the client to reuse.
    let session_header = session_id.clone();

    match decision.backend_type {
        BackendType::Openai => {
            let mut openai_request = ChatCompletionRequest {
                model: Some(decision.model.clone()),
                messages: packed_messages.clone(),
                stream: request.stream,
                temperature: request.temperature,
                max_tokens: request.max_tokens,
                top_p: request.top_p,
                stop: request.stop.clone(),
                session_id: None, // Don't forward session_id to backend
                project_id: None, // Don't forward project_id to backend
            };

            if request.stream {
                let handle = proxy::stream_chat_openai(
                    state.http_client.clone(),
                    decision.backend_url.clone(),
                    openai_request,
                )
                .await?;

                // Spawn background task: wait for stream completion, then store
                // the real response text as an engram and update the session.
                {
                    let http = state.http_client.clone();
                    let mimir_url = state.mimir_url.clone();
                    let store_on_completion = state.config.mimir.store_on_completion;
                    let session_store = state.session_store.clone();
                    let sid = session_id.clone();
                    let cause = last_user_message.clone();
                    let completion_rx = handle.completion_rx;
                    let ctx_budget = backend_context_window;
                    let bk_url = decision.backend_url.clone();
                    let bk_type = decision.backend_type.clone();
                    let bk_model = decision.model.clone();
                    tokio::spawn(async move {
                        if let Ok(effect) = completion_rx.await {
                            session_store.append_messages(
                                &sid,
                                &[CompactMessage::new("assistant", &effect)],
                            );
                            if store_on_completion {
                                spawn_engram_store(http.clone(), mimir_url, cause, effect);
                            }
                            maybe_summarize_session(http, session_store, sid, ctx_budget, bk_url, bk_type, bk_model);
                        }
                    });
                }

                let mut resp = Sse::new(handle.stream).into_response();
                resp.headers_mut().insert(
                    "x-session-id",
                    axum::http::HeaderValue::from_str(&session_header).expect("session_id is valid ASCII"),
                );
                Ok(resp)
            } else {
                openai_request.stream = false;
                let local_result = proxy::generate_chat_openai(
                    &state.http_client,
                    &decision.backend_url,
                    openai_request,
                )
                .await;

                let response = match local_result {
                    Ok(resp) => resp,
                    Err(local_err) => {
                        // If local backend failed and cloud fallback is enabled, try cloud providers
                        if let Some(pool) = &state.cloud_pool {
                            if pool.fallback_enabled {
                                let cloud_messages: Vec<_> = packed_messages.iter().map(|m| {
                                    ygg_cloud::adapter::ChatMessage {
                                        role: m.role.to_string(),
                                        content: m.content.clone(),
                                    }
                                }).collect();

                                if let Some(cloud_content) = pool.fallback_chat(cloud_messages, Some(&decision.model)).await {
                                    tracing::info!("cloud fallback produced response for openai backend failure");
                                    crate::openai::ChatCompletionResponse {
                                        id: completion_id.clone(),
                                        object: "chat.completion".to_string(),
                                        created: unix_now(),
                                        model: format!("cloud-fallback:{}", decision.model),
                                        choices: vec![crate::openai::Choice {
                                            index: 0,
                                            message: ChatMessage {
                                                role: Role::Assistant,
                                                content: cloud_content,
                                            },
                                            finish_reason: Some("stop".to_string()),
                                        }],
                                        usage: None,
                                    }
                                } else {
                                    return Err(local_err);
                                }
                            } else {
                                return Err(local_err);
                            }
                        } else {
                            return Err(local_err);
                        }
                    }
                };

                // ── 11. Update session + engram store ─────────────────
                let effect = response
                    .choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_default();

                state.session_store.append_messages(
                    &session_id,
                    &[CompactMessage::new("assistant", &effect)],
                );

                if state.config.mimir.store_on_completion {
                    spawn_engram_store(
                        state.http_client.clone(),
                        state.mimir_url.clone(),
                        last_user_message,
                        effect,
                    );
                }

                maybe_summarize_session(
                    state.http_client.clone(),
                    state.session_store.clone(),
                    session_id.clone(),
                    backend_context_window,
                    decision.backend_url.clone(),
                    decision.backend_type.clone(),
                    decision.model.clone(),
                );

                let mut resp = Json(response).into_response();
                resp.headers_mut().insert(
                    "x-session-id",
                    axum::http::HeaderValue::from_str(&session_header).expect("session_id is valid ASCII"),
                );
                Ok(resp)
            }
        }
        BackendType::Ollama => {
            let ollama_messages: Vec<crate::openai::OllamaMessage> = packed_messages
                .iter()
                .map(|m| crate::openai::OllamaMessage {
                    role: m.role.to_string(),
                    content: m.content.clone(),
                })
                .collect();

            // Default stop sequences prevent Qwen3 MoE from self-prompting
            // (generating fake follow-up questions and answering them)
            // and from appending filler phrases.
            let stop = request.stop.clone().or_else(|| {
                Some(vec![
                    "\n\nWhat ".to_string(),
                    "\n\nHow ".to_string(),
                    "\n\nWhy ".to_string(),
                    "\n\nCan ".to_string(),
                    "\n\nIs ".to_string(),
                    "\nConclusion".to_string(),
                    "\nIn conclusion".to_string(),
                    "\nIn summary".to_string(),
                    "\n\nLet me ".to_string(),
                    "\n\nFeel free".to_string(),
                    "\n\nI hope".to_string(),
                ])
            });
            let options = Some(OllamaOptions {
                temperature: request.temperature.or(Some(0.7)),
                num_predict: request.max_tokens,
                num_ctx: Some(backend_context_window as u64),
                top_p: request.top_p,
                stop,
            });

            let ollama_request = OllamaChatRequest {
                model: decision.model.clone(),
                messages: ollama_messages,
                stream: request.stream,
                options,
            };

            if request.stream {
                let handle = proxy::stream_chat(
                    state.http_client.clone(),
                    decision.backend_url.clone(),
                    ollama_request,
                    completion_id.clone(),
                )
                .await?;

                // Spawn background task: wait for stream completion, then store
                // the real response text as an engram and update the session.
                {
                    let http = state.http_client.clone();
                    let mimir_url = state.mimir_url.clone();
                    let store_on_completion = state.config.mimir.store_on_completion;
                    let session_store = state.session_store.clone();
                    let sid = session_id.clone();
                    let cause = last_user_message.clone();
                    let completion_rx = handle.completion_rx;
                    let ctx_budget = backend_context_window;
                    let bk_url = decision.backend_url.clone();
                    let bk_type = decision.backend_type.clone();
                    let bk_model = decision.model.clone();
                    tokio::spawn(async move {
                        if let Ok(effect) = completion_rx.await {
                            session_store.append_messages(
                                &sid,
                                &[CompactMessage::new("assistant", &effect)],
                            );
                            if store_on_completion {
                                spawn_engram_store(http.clone(), mimir_url, cause, effect);
                            }
                            maybe_summarize_session(http, session_store, sid, ctx_budget, bk_url, bk_type, bk_model);
                        }
                    });
                }

                let mut resp = Sse::new(handle.stream).into_response();
                resp.headers_mut().insert(
                    "x-session-id",
                    axum::http::HeaderValue::from_str(&session_header).expect("session_id is valid ASCII"),
                );
                Ok(resp)
            } else {
                let local_result = proxy::generate_chat(
                    &state.http_client,
                    &decision.backend_url,
                    ollama_request,
                    &completion_id,
                )
                .await;

                let response = match local_result {
                    Ok(resp) => resp,
                    Err(local_err) => {
                        // If local backend failed and cloud fallback is enabled, try cloud providers
                        if let Some(pool) = &state.cloud_pool {
                            if pool.fallback_enabled {
                                let cloud_messages: Vec<_> = packed_messages.iter().map(|m| {
                                    ygg_cloud::adapter::ChatMessage {
                                        role: m.role.to_string(),
                                        content: m.content.clone(),
                                    }
                                }).collect();

                                if let Some(cloud_content) = pool.fallback_chat(cloud_messages, Some(&decision.model)).await {
                                    tracing::info!("cloud fallback produced response for ollama backend failure");
                                    crate::openai::ChatCompletionResponse {
                                        id: completion_id.clone(),
                                        object: "chat.completion".to_string(),
                                        created: unix_now(),
                                        model: format!("cloud-fallback:{}", decision.model),
                                        choices: vec![crate::openai::Choice {
                                            index: 0,
                                            message: ChatMessage {
                                                role: Role::Assistant,
                                                content: cloud_content,
                                            },
                                            finish_reason: Some("stop".to_string()),
                                        }],
                                        usage: None,
                                    }
                                } else {
                                    return Err(local_err);
                                }
                            } else {
                                return Err(local_err);
                            }
                        } else {
                            return Err(local_err);
                        }
                    }
                };

                // ── 11. Update session + engram store ─────────────────
                let effect = response
                    .choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_default();

                state.session_store.append_messages(
                    &session_id,
                    &[CompactMessage::new("assistant", &effect)],
                );

                if state.config.mimir.store_on_completion {
                    spawn_engram_store(
                        state.http_client.clone(),
                        state.mimir_url.clone(),
                        last_user_message,
                        effect,
                    );
                }

                maybe_summarize_session(
                    state.http_client.clone(),
                    state.session_store.clone(),
                    session_id.clone(),
                    backend_context_window,
                    decision.backend_url.clone(),
                    decision.backend_type.clone(),
                    decision.model.clone(),
                );

                let mut resp = Json(response).into_response();
                resp.headers_mut().insert(
                    "x-session-id",
                    axum::http::HeaderValue::from_str(&session_header).expect("session_id is valid ASCII"),
                );
                Ok(resp)
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// GET /v1/models
// ─────────────────────────────────────────────────────────────────

/// Aggregate model listing from all configured Ollama backends.
///
/// Backends that are unreachable are skipped with a warning; the endpoint
/// never fails as long as at least one backend responds.  An empty list is
/// returned if all backends are down.
pub async fn models_handler(
    State(state): State<AppState>,
) -> Result<Json<ModelList>, OdinError> {
    let mut models: Vec<Model> = Vec::new();
    let created = unix_now();

    for backend in &state.backends {
        match backend.backend_type {
            BackendType::Openai => {
                match proxy::list_models_openai(&state.http_client, &backend.url).await {
                    Ok(ids) => {
                        for id in ids {
                            models.push(Model {
                                id,
                                object: "model".to_string(),
                                created,
                                owned_by: format!("openai:{}", backend.name),
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(backend = %backend.name, error = %e, "failed to fetch models from openai backend — skipping");
                    }
                }
            }
            BackendType::Ollama => {
                match proxy::list_models(&state.http_client, &backend.url).await {
                    Ok(tags) => {
                        for model_info in tags.models {
                            models.push(Model {
                                id: model_info.name.clone(),
                                object: "model".to_string(),
                                created,
                                owned_by: format!("ollama:{}", backend.name),
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(backend = %backend.name, error = %e, "failed to fetch models from backend — skipping");
                    }
                }
            }
        }
    }

    Ok(Json(ModelList {
        object: "list".to_string(),
        data: models,
    }))
}

// ─────────────────────────────────────────────────────────────────
// GET /health
// ─────────────────────────────────────────────────────────────────

/// Backend health status.
#[derive(Debug, Clone, Serialize)]
pub struct BackendHealth {
    pub status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Full health response body.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub backends: HashMap<String, BackendHealth>,
    pub services: HashMap<String, String>,
}

/// Odin health check endpoint.
///
/// Probes each Ollama backend and each downstream service (Mimir, Muninn)
/// with a 2-second timeout.  Always returns HTTP 200 — the health state is
/// conveyed in the body.  The top-level `status` is:
///   - `"ok"` if all probes pass,
///   - `"degraded"` if some pass,
///   - `"error"` if all fail.
pub async fn health_handler(State(state): State<AppState>) -> Json<HealthResponse> {
    let mut backends: HashMap<String, BackendHealth> = HashMap::new();
    let mut services: HashMap<String, String> = HashMap::new();

    // ── Check backends ──────────────────────────────────────────────
    for backend in &state.backends {
        let health = match backend.backend_type {
            BackendType::Openai => {
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    proxy::list_models_openai(&state.http_client, &backend.url),
                )
                .await;

                match result {
                    Ok(Ok(ids)) => BackendHealth {
                        status: "ok".to_string(),
                        models: ids,
                        error: None,
                    },
                    Ok(Err(e)) => BackendHealth {
                        status: "error".to_string(),
                        models: vec![],
                        error: Some(e.to_string()),
                    },
                    Err(_) => BackendHealth {
                        status: "error".to_string(),
                        models: vec![],
                        error: Some("timeout".to_string()),
                    },
                }
            }
            BackendType::Ollama => {
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    proxy::list_models(&state.http_client, &backend.url),
                )
                .await;

                match result {
                    Ok(Ok(tags)) => BackendHealth {
                        status: "ok".to_string(),
                        models: tags.models.into_iter().map(|m| m.name).collect(),
                        error: None,
                    },
                    Ok(Err(e)) => BackendHealth {
                        status: "error".to_string(),
                        models: vec![],
                        error: Some(e.to_string()),
                    },
                    Err(_) => BackendHealth {
                        status: "error".to_string(),
                        models: vec![],
                        error: Some("timeout".to_string()),
                    },
                }
            }
        };

        backends.insert(backend.name.clone(), health);
    }

    // ── Check Mimir ───────────────────────────────────────────────
    let mimir_status = check_service_health(&state.http_client, &state.mimir_url).await;
    services.insert("mimir".to_string(), mimir_status);

    // ── Check Muninn ──────────────────────────────────────────────
    let muninn_status = check_service_health(&state.http_client, &state.muninn_url).await;
    services.insert("muninn".to_string(), muninn_status);

    // ── Determine top-level status ────────────────────────────────
    let backend_statuses: Vec<&str> = backends.values().map(|b| b.status.as_str()).collect();
    let service_statuses: Vec<&str> = services.values().map(|s| s.as_str()).collect();
    let all_statuses: Vec<&str> = backend_statuses
        .into_iter()
        .chain(service_statuses.into_iter())
        .collect();

    let ok_count = all_statuses.iter().filter(|&&s| s == "ok").count();
    let total = all_statuses.len();

    let status = if ok_count == total {
        "ok"
    } else if ok_count == 0 {
        "error"
    } else {
        "degraded"
    };

    Json(HealthResponse {
        status: status.to_string(),
        backends,
        services,
    })
}

/// Probe a service's `/health` endpoint with a 2-second timeout.
/// Returns `"ok"` or `"error"`.
async fn check_service_health(client: &reqwest::Client, base_url: &str) -> String {
    let url = format!("{base_url}/health");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.get(&url).send(),
    )
    .await;

    match result {
        Ok(Ok(resp)) if resp.status().is_success() => "ok".to_string(),
        Ok(Ok(resp)) => format!("error:{}", resp.status()),
        Ok(Err(e)) => format!("error:{e}"),
        Err(_) => "error:timeout".to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/query  (transparent Mimir proxy)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Mimir's `POST /api/v1/query`.
///
/// The request body, response body, status code, and content-type header are
/// all forwarded unchanged.  This allows the Fergus client to point its
/// `EngramClient` at Odin without modification.
pub async fn proxy_query(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/query", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "query").await
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/store  (transparent Mimir proxy)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Mimir's `POST /api/v1/store`.
///
/// See `proxy_query` for the forwarding semantics.
pub async fn proxy_store(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/store", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "store").await
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/sdr/operations  (transparent Mimir proxy)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Mimir's `POST /api/v1/sdr/operations`.
///
/// See `proxy_query` for the forwarding semantics.
pub async fn proxy_sdr_operations(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/sdr/operations", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "sdr_operations").await
}

/// Shared transparent proxy logic for Mimir endpoints.
async fn forward_to_mimir(
    client: &reqwest::Client,
    url: &str,
    body: Bytes,
    op: &str,
) -> Result<Response, OdinError> {
    let upstream = client
        .post(url)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| OdinError::Proxy(format!("mimir unreachable ({op}): {e}")))?;

    // Mirror the upstream status code.
    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    // Forward content-type header if present.
    let mut headers = HeaderMap::new();
    if let Some(ct) = upstream.headers().get("content-type") {
        headers.insert("content-type", ct.clone());
    }

    let response_body = upstream
        .bytes()
        .await
        .map_err(|e| OdinError::Proxy(format!("mimir body read error ({op}): {e}")))?;

    Ok((status, headers, response_body).into_response())
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/timeline  (transparent Mimir proxy)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Mimir's `POST /api/v1/timeline`.
pub async fn proxy_timeline(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/timeline", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "timeline").await
}

/// Transparent proxy to Mimir's `POST /api/v1/sprints/list`.
pub async fn proxy_sprint_list(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/sprints/list", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "sprint_list").await
}

// ─────────────────────────────────────────────────────────────────
// Context offload proxies (POST / GET)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Mimir's `POST /api/v1/context` (store).
pub async fn proxy_context_store(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/context", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "context_store").await
}

/// Transparent proxy to Mimir's `GET /api/v1/context` (list).
pub async fn proxy_context_list(
    State(state): State<AppState>,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/context", state.mimir_url);
    forward_get_to_mimir(&state.http_client, &url, "context_list").await
}

/// Transparent proxy to Mimir's `GET /api/v1/context/:handle` (retrieve).
pub async fn proxy_context_retrieve(
    State(state): State<AppState>,
    axum::extract::Path(handle): axum::extract::Path<String>,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/context/{}", state.mimir_url, handle);
    forward_get_to_mimir(&state.http_client, &url, "context_retrieve").await
}

// ─────────────────────────────────────────────────────────────────
// Task queue proxies (Mimir)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Mimir's `POST /api/v1/tasks/push`.
pub async fn proxy_task_push(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/tasks/push", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "task_push").await
}

/// Transparent proxy to Mimir's `POST /api/v1/tasks/pop`.
pub async fn proxy_task_pop(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/tasks/pop", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "task_pop").await
}

/// Transparent proxy to Mimir's `POST /api/v1/tasks/complete`.
pub async fn proxy_task_complete(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/tasks/complete", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "task_complete").await
}

/// Transparent proxy to Mimir's `POST /api/v1/tasks/cancel`.
pub async fn proxy_task_cancel(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/tasks/cancel", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "task_cancel").await
}

/// Transparent proxy to Mimir's `POST /api/v1/tasks/list`.
pub async fn proxy_task_list(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/tasks/list", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "task_list").await
}

// ─────────────────────────────────────────────────────────────────
// Graph proxies (Mimir)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Mimir's `POST /api/v1/graph/link`.
pub async fn proxy_graph_link(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/graph/link", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "graph_link").await
}

/// Transparent proxy to Mimir's `POST /api/v1/graph/unlink`.
pub async fn proxy_graph_unlink(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/graph/unlink", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "graph_unlink").await
}

/// Transparent proxy to Mimir's `POST /api/v1/graph/neighbors`.
pub async fn proxy_graph_neighbors(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/graph/neighbors", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "graph_neighbors").await
}

/// Transparent proxy to Mimir's `POST /api/v1/graph/traverse`.
pub async fn proxy_graph_traverse(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/graph/traverse", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "graph_traverse").await
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/symbols  (transparent Muninn proxy)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Muninn's `POST /api/v1/symbols`.
pub async fn proxy_symbols(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/symbols", state.muninn_url);
    forward_to_muninn(&state.http_client, &url, body, "symbols").await
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/references  (transparent Muninn proxy)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Muninn's `POST /api/v1/references`.
pub async fn proxy_references(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/references", state.muninn_url);
    forward_to_muninn(&state.http_client, &url, body, "references").await
}

/// Shared transparent proxy logic for Muninn endpoints.
async fn forward_to_muninn(
    client: &reqwest::Client,
    url: &str,
    body: Bytes,
    op: &str,
) -> Result<Response, OdinError> {
    let upstream = client
        .post(url)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| OdinError::Proxy(format!("muninn unreachable ({op}): {e}")))?;

    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut headers = HeaderMap::new();
    if let Some(ct) = upstream.headers().get("content-type") {
        headers.insert("content-type", ct.clone());
    }

    let response_body = upstream
        .bytes()
        .await
        .map_err(|e| OdinError::Proxy(format!("muninn body read error ({op}): {e}")))?;

    Ok((status, headers, response_body).into_response())
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/notify
// ─────────────────────────────────────────────────────────────────

/// Unified notification API — sends push notifications via HA.
///
/// Expects JSON body with `title`, `message`, and optional `target` fields.
/// If `target` is omitted, defaults to `"mobile_app_pixel"`.
pub async fn notify_handler(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let title = payload["title"].as_str().unwrap_or("Yggdrasil Alert");
    let message = payload["message"].as_str().unwrap_or("");
    let target = payload["target"].as_str();

    if let Some(ha) = &state.ha_client {
        let notify_target = target.unwrap_or("mobile_app_pixel");
        match ha.notify_simple(notify_target, title, message).await {
            Ok(()) => {
                Json(serde_json::json!({"status": "sent", "target": notify_target}))
                    .into_response()
            }
            Err(e) => {
                (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e.to_string()})))
                    .into_response()
            }
        }
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "HA not configured"})))
            .into_response()
    }
}

/// Shared GET proxy logic for Mimir endpoints.
async fn forward_get_to_mimir(
    client: &reqwest::Client,
    url: &str,
    op: &str,
) -> Result<Response, OdinError> {
    let upstream = client
        .get(url)
        .send()
        .await
        .map_err(|e| OdinError::Proxy(format!("mimir unreachable ({op}): {e}")))?;

    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut headers = HeaderMap::new();
    if let Some(ct) = upstream.headers().get("content-type") {
        headers.insert("content-type", ct.clone());
    }

    let response_body = upstream
        .bytes()
        .await
        .map_err(|e| OdinError::Proxy(format!("mimir body read error ({op}): {e}")))?;

    Ok((status, headers, response_body).into_response())
}
