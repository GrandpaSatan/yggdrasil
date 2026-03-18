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
use crate::state::{AppState, CloudPool};

// ─────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────

/// RAII guard that decrements the backend active requests gauge on drop.
struct BackendActiveGuard {
    backend: String,
}

impl BackendActiveGuard {
    fn new(backend: &str) -> Self {
        crate::metrics::adjust_backend_active(backend, 1.0);
        Self { backend: backend.to_string() }
    }
}

impl Drop for BackendActiveGuard {
    fn drop(&mut self) {
        crate::metrics::adjust_backend_active(&self.backend, -1.0);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Attempt cloud fallback when a local backend fails.
///
/// Converts `packed_messages` to cloud format and tries each adapter in order.
/// Returns the cloud response text on success, or propagates the original error.
async fn try_cloud_fallback(
    cloud_pool: &Option<CloudPool>,
    packed_messages: &[ChatMessage],
    model: &str,
    local_err: OdinError,
) -> Result<String, OdinError> {
    if let Some(pool) = cloud_pool
        && pool.fallback_enabled
    {
        let cloud_messages: Vec<_> = packed_messages
            .iter()
            .map(|m| ygg_cloud::adapter::ChatMessage {
                role: m.role.to_string(),
                content: m.content.clone(),
            })
            .collect();

        if let Some(cloud_content) = pool.fallback_chat(cloud_messages, Some(model)).await {
            tracing::info!("cloud fallback produced response for voice pipeline");
            return Ok(cloud_content);
        }
    }
    Err(local_err)
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
                        crate::openai::OllamaMessage::new("system", system),
                        crate::openai::OllamaMessage::new("user", prompt),
                    ],
                    stream: false,
                    options: Some(OllamaOptions {
                        temperature: Some(0.3),
                        num_predict: Some(512),
                        num_ctx: Some(context_budget as u64),
                        top_p: None,
                        stop: None,
                    }),
                    think: Some(false),
                    tools: None,
                };
                proxy::generate_chat(&http_client, &backend_url, req, &completion_id)
                    .await
                    .map(|r| r.choices.into_iter().next().map(|c| c.message.content).unwrap_or_default())
            }
            BackendType::Openai => {
                let req = crate::openai::ChatCompletionRequest {
                    model: None,
                    messages: vec![
                        ChatMessage::new(Role::System, system),
                        ChatMessage::new(Role::User, prompt),
                    ],
                    stream: false,
                    temperature: Some(0.3),
                    max_tokens: Some(512),
                    top_p: None,
                    stop: None,
                    session_id: None,
                    project_id: None,
                    tools: None,
                    tool_choice: None,
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
// Reusable text pipeline (used by chat_handler and voice_ws)
// ─────────────────────────────────────────────────────────────────

/// Process a text message through the full Odin pipeline (routing, RAG, LLM,
/// session update).  Used by both the HTTP `chat_handler` (non-streaming path)
/// and the WebSocket voice handler.
///
/// Returns the assistant's response text on success.
pub async fn process_chat_text(
    state: &AppState,
    text: &str,
    session_id: &str,
) -> Result<String, OdinError> {
    // ── 1. Append user message to session ─────────────────────────
    state
        .session_store
        .append_messages(session_id, &[CompactMessage::new("user", text)]);

    let user_text = text.to_string();

    // ── 2. Route: classify intent via semantic router ─────────────
    let mut decision = state.router.classify(&user_text);

    // ── 3. Memory-event routing refinement ────────────────────────
    if let Some(recall) = rag::fetch_memory_events(
        &state.http_client,
        &state.mimir_url,
        &user_text,
        state.config.mimir.query_limit,
    )
    .await
    {
        memory_router::apply_memory_events(&recall, &mut decision);

        if let Some(hex) = &recall.query_sdr_hex
            && let Some(query_sdr) = ygg_domain::sdr::from_hex(hex)
                && let Some(drift) = state.session_store.update_session_sdr(session_id, &query_sdr) {
                    tracing::debug!(
                        session_id = %session_id,
                        drift_score = %drift,
                        "session SDR drift score (voice)"
                    );
                    if drift < 0.5 {
                        tracing::info!(
                            session_id = %session_id,
                            drift_score = %drift,
                            "topic drift detected — session SDR reset (voice)"
                        );
                    }
                }
    }

    tracing::info!(
        intent = %decision.intent,
        model = %decision.model,
        backend = %decision.backend_name,
        session_id = %session_id,
        "voice routing decision (post memory refinement)"
    );

    crate::metrics::record_routing_intent(&decision.intent);

    // ── 4. Acquire semaphore ──────────────────────────────────────
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

    let _backend_guard = BackendActiveGuard::new(&decision.backend_name);

    // ── 5. Fetch RAG context ──────────────────────────────────────
    let span = tracing::info_span!("rag_fetch_voice");
    let rag_context = {
        let _guard = span.enter();
        rag::fetch_context(state, &user_text, &decision.intent).await
    };

    // ── 6. Pack context with session history ──────────────────────
    let backend_context_window = backend_state.context_window;
    // Voice pipeline uses the "voice" persona (Alfred) regardless of routed intent.
    let system_prompt = rag::build_system_prompt(&rag_context, "voice");
    let session_snapshot = state.session_store.get_session(session_id);

    let packed_messages = if let Some(ref session) = session_snapshot {
        let budget = ContextBudget {
            total_budget: backend_context_window,
            generation_reserve: state.config.session.generation_reserve,
        };
        budget.pack(
            session,
            rag_context.code_context.as_deref(),
            &system_prompt,
            None, // No project-scoped cross-window context for voice
        )
    } else {
        // Fallback: no session yet — construct a minimal message list.
        vec![
            ChatMessage::new(Role::System, &system_prompt),
            ChatMessage::new(Role::User, user_text.clone()),
        ]
    };

    let completion_id = format!("chatcmpl-{}", Uuid::new_v4());

    // ── 7. Dispatch to LLM (non-streaming only) ──────────────────
    let effect = match decision.backend_type {
        BackendType::Openai => {
            let openai_request = ChatCompletionRequest {
                model: Some(decision.model.clone()),
                messages: packed_messages.clone(),
                stream: false,
                temperature: Some(0.7),
                max_tokens: None,
                top_p: None,
                stop: None,
                session_id: None,
                project_id: None,
                tools: None,
                tool_choice: None,
            };

            let gen_start = std::time::Instant::now();
            let local_result = proxy::generate_chat_openai(
                &state.http_client,
                &decision.backend_url,
                openai_request,
            )
            .await;
            crate::metrics::record_llm_generation(
                &decision.model,
                gen_start.elapsed().as_secs_f64(),
            );

            match local_result {
                Ok(resp) => resp
                    .choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_default(),
                Err(local_err) => try_cloud_fallback(
                    &state.cloud_pool,
                    &packed_messages,
                    &decision.model,
                    local_err,
                )
                .await?,
            }
        }
        BackendType::Ollama => {
            // ── Voice agent loop (tool-use) ───────────────────────
            // Build tool definitions from the registry so the voice LLM
            // can call ha_call_service, gaming, query_memory, etc.
            let agent_config = state
                .config
                .agent
                .clone()
                .unwrap_or_default();

            let allowed_tiers: Vec<crate::tool_registry::ToolTier> = agent_config
                .default_tiers
                .iter()
                .filter_map(|t| match t.as_str() {
                    "safe" => Some(crate::tool_registry::ToolTier::Safe),
                    "restricted" => Some(crate::tool_registry::ToolTier::Restricted),
                    _ => None,
                })
                .collect();

            // All tools available — Odin is the single gateway for all LLM traffic.
            let tool_defs: Vec<_> = crate::tool_registry::to_tool_definitions(
                &state.tool_registry,
                &allowed_tiers,
            );

            if !tool_defs.is_empty() {
                // Repack with tool token reserve — tool schemas consume context
                // window space in Ollama but aren't tracked by ContextBudget.
                // Estimate dynamically from the actual tool definitions.
                let tool_token_reserve = serde_json::to_string(&tool_defs)
                    .map(|s| s.len() / 4)
                    .unwrap_or(2000);
                let packed_messages = if let Some(ref session) = session_snapshot {
                    let budget = ContextBudget {
                        total_budget: backend_context_window
                            .saturating_sub(tool_token_reserve),
                        generation_reserve: state.config.session.generation_reserve,
                    };
                    budget.pack(
                        session,
                        rag_context.code_context.as_deref(),
                        &system_prompt,
                        None,
                    )
                } else {
                    packed_messages.clone()
                };

                // Agent loop path: LLM can call tools autonomously.
                let gen_start = std::time::Instant::now();
                let response = crate::agent::run_agent_loop(
                    state,
                    &packed_messages,
                    &tool_defs,
                    &state.tool_registry,
                    &allowed_tiers,
                    &decision,
                    &completion_id,
                    &agent_config,
                    backend_context_window,
                )
                .await;
                crate::metrics::record_llm_generation(
                    &decision.model,
                    gen_start.elapsed().as_secs_f64(),
                );

                match response {
                    Ok(resp) => resp
                        .choices
                        .first()
                        .map(|c| c.message.content.clone())
                        .unwrap_or_default(),
                    Err(e) => {
                        tracing::warn!(error = %e, "voice agent loop failed — falling back to plain chat");
                        // Fall through to plain chat below on error.
                        voice_plain_chat(state, &packed_messages, &decision, backend_context_window, &completion_id).await?
                    }
                }
            } else {
                // No tools configured — plain single-shot chat.
                voice_plain_chat(state, &packed_messages, &decision, backend_context_window, &completion_id).await?
            }
        }
    };

    // ── 8. Update session + engram store ──────────────────────────
    state
        .session_store
        .append_messages(session_id, &[CompactMessage::new("assistant", &effect)]);

    if state.config.mimir.store_on_completion {
        spawn_engram_store(
            state.http_client.clone(),
            state.mimir_url.clone(),
            user_text,
            effect.clone(),
        );
    }

    maybe_summarize_session(
        state.http_client.clone(),
        state.session_store.clone(),
        session_id.to_string(),
        backend_context_window,
        decision.backend_url.clone(),
        decision.backend_type.clone(),
        decision.model.clone(),
    );

    Ok(effect)
}

// ─────────────────────────────────────────────────────────────────
// Multimodal audio pipeline (used by voice_ws for Qwen3-Omni)
// ─────────────────────────────────────────────────────────────────

/// Process audio input through Qwen3-Omni via the OpenAI multimodal API.
///
/// Sends base64-encoded WAV audio directly to the Omni model, which handles
/// both transcription and reasoning in a single pass. RAG context and session
/// history are included as text messages alongside the audio content part.
///
/// Returns the assistant's response text on success.
pub async fn process_chat_audio(
    state: &AppState,
    audio_b64: &str,
    omni_url: &str,
    omni_model: &str,
    session_id: &str,
) -> Result<String, OdinError> {
    use crate::openai::{
        AudioInputData, MultimodalChatCompletionRequest, MultimodalChatMessage,
        MultimodalContent, MultimodalContentPart,
    };

    // ── 1. Append placeholder to session history ─────────────────
    state
        .session_store
        .append_messages(session_id, &[CompactMessage::new("user", "[audio message]")]);

    // ── 2. Fetch RAG context (best-effort) ───────────────────────
    let rag_context =
        rag::fetch_context(state, "[voice audio input]", "voice").await;

    // ── 3. Build system prompt ───────────────────────────────────
    let system_prompt = rag::build_system_prompt(&rag_context, "voice");

    // ── 4. Gather session history as text messages ───────────────
    let session_snapshot = state.session_store.get_session(session_id);

    // ── 5. Build multimodal message list ─────────────────────────
    let mut messages: Vec<MultimodalChatMessage> = Vec::new();

    // System prompt (text).
    messages.push(MultimodalChatMessage {
        role: Role::System,
        content: MultimodalContent::Text(system_prompt),
    });

    // Session history (text messages only — prior turns).
    if let Some(ref session) = session_snapshot {
        for msg in &session.messages {
            // Skip the placeholder we just appended.
            if msg.content == "[audio message]" && msg.role == "user" {
                continue;
            }
            let role = match msg.role.as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                _ => Role::User,
            };
            messages.push(MultimodalChatMessage {
                role,
                content: MultimodalContent::Text(msg.content.clone()),
            });
        }
    }

    // Current user message: audio content part.
    messages.push(MultimodalChatMessage {
        role: Role::User,
        content: MultimodalContent::Parts(vec![MultimodalContentPart::InputAudio {
            input_audio: AudioInputData {
                data: audio_b64.to_string(),
                format: "wav".to_string(),
            },
        }]),
    });

    // ── 6. Dispatch to Omni backend ──────────────────────────────
    let request = MultimodalChatCompletionRequest {
        model: omni_model.to_string(),
        messages,
        temperature: Some(0.7),
        stream: false,
    };

    let gen_start = std::time::Instant::now();
    let result = proxy::generate_chat_multimodal(
        &state.http_client,
        omni_url,
        request,
    )
    .await;
    crate::metrics::record_llm_generation(omni_model, gen_start.elapsed().as_secs_f64());

    let effect = match result {
        Ok(resp) => resp
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default(),
        Err(e) => return Err(e),
    };

    // ── 7. Update session + engram store ──────────────────────────
    state
        .session_store
        .append_messages(session_id, &[CompactMessage::new("assistant", &effect)]);

    if state.config.mimir.store_on_completion {
        spawn_engram_store(
            state.http_client.clone(),
            state.mimir_url.clone(),
            "[voice audio input]".to_string(),
            effect.clone(),
        );
    }

    Ok(effect)
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

    crate::metrics::record_routing_intent(&decision.intent);

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

    // RAII guard: +1 on creation, -1 on drop (covers all return paths)
    let _backend_guard = BackendActiveGuard::new(&decision.backend_name);

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
            total_budget: backend_context_window,
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
                msgs.insert(0, ChatMessage::new(Role::System, system_prompt));
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
                tools: None,
                tool_choice: None,
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
                let gen_start = std::time::Instant::now();
                let local_result = proxy::generate_chat_openai(
                    &state.http_client,
                    &decision.backend_url,
                    openai_request,
                )
                .await;
                crate::metrics::record_llm_generation(&decision.model, gen_start.elapsed().as_secs_f64());

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
                                            message: ChatMessage::new(Role::Assistant, cloud_content),
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
            // ── 10a. Agent loop (tool-use mode) ──────────────────
            // When the request includes tool definitions, enter the autonomous
            // agent loop instead of the standard single-shot dispatch.
            if let Some(ref tools) = request.tools {
                let agent_config = state
                    .config
                    .agent
                    .clone()
                    .unwrap_or_default();

                // Parse allowed tiers from config.
                let allowed_tiers: Vec<crate::tool_registry::ToolTier> = agent_config
                    .default_tiers
                    .iter()
                    .filter_map(|t| match t.as_str() {
                        "safe" => Some(crate::tool_registry::ToolTier::Safe),
                        "restricted" => Some(crate::tool_registry::ToolTier::Restricted),
                        _ => None,
                    })
                    .collect();

                let registry = &state.tool_registry;

                if request.stream {
                    tracing::warn!("agent loop does not support streaming — falling back to non-streaming");
                }

                let response = crate::agent::run_agent_loop(
                    &state,
                    &packed_messages,
                    tools,
                    registry,
                    &allowed_tiers,
                    &decision,
                    &completion_id,
                    &agent_config,
                    backend_context_window,
                )
                .await?;

                // Store final response in session + engram (reuse step 11 logic).
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

                let mut resp = Json(response).into_response();
                resp.headers_mut().insert(
                    "x-session-id",
                    axum::http::HeaderValue::from_str(&session_header)
                        .expect("session_id is valid ASCII"),
                );
                return Ok(resp);
            }

            // ── 10b. Standard Ollama dispatch (no tools) ─────────
            let ollama_messages: Vec<crate::openai::OllamaMessage> = packed_messages
                .iter()
                .map(|m| crate::openai::OllamaMessage::new(m.role.to_string(), m.content.clone()))
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
                think: None,
                tools: None,
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
                let gen_start = std::time::Instant::now();
                let local_result = proxy::generate_chat(
                    &state.http_client,
                    &decision.backend_url,
                    ollama_request,
                    &completion_id,
                )
                .await;
                crate::metrics::record_llm_generation(&decision.model, gen_start.elapsed().as_secs_f64());

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
                                            message: ChatMessage::new(Role::Assistant, cloud_content),
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

/// Transparent proxy to Mimir's `GET /api/v1/engrams/:id`.
pub async fn proxy_engram_by_id(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/engrams/{}", state.mimir_url, id);
    forward_get_to_mimir(&state.http_client, &url, "engram_by_id").await
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/embed  (transparent Mimir proxy)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Mimir's `POST /api/v1/embed`.
///
/// Returns raw ONNX embedding for text. Used by ygg-sentinel for SDR encoding.
pub async fn proxy_embed(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/embed", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "embed").await
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

// ─────────────────────────────────────────────────────────────────
// Voice plain chat fallback
// ─────────────────────────────────────────────────────────────────

/// Single-shot Ollama chat without tools — used as fallback for voice when
/// the agent loop is unavailable or fails.
async fn voice_plain_chat(
    state: &AppState,
    packed_messages: &[ChatMessage],
    decision: &crate::router::RoutingDecision,
    backend_context_window: usize,
    completion_id: &str,
) -> Result<String, OdinError> {
    let ollama_messages: Vec<crate::openai::OllamaMessage> = packed_messages
        .iter()
        .map(|m| crate::openai::OllamaMessage::new(m.role.to_string(), m.content.clone()))
        .collect();

    let stop = Some(vec![
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
    ]);

    let options = Some(OllamaOptions {
        temperature: Some(0.7),
        num_predict: None,
        num_ctx: Some(backend_context_window as u64),
        top_p: None,
        stop,
    });

    let ollama_request = OllamaChatRequest {
        model: decision.model.clone(),
        messages: ollama_messages,
        stream: false,
        options,
        think: Some(false),
        tools: None,
    };

    let gen_start = std::time::Instant::now();
    let local_result = proxy::generate_chat(
        &state.http_client,
        &decision.backend_url,
        ollama_request,
        completion_id,
    )
    .await;
    crate::metrics::record_llm_generation(
        &decision.model,
        gen_start.elapsed().as_secs_f64(),
    );

    match local_result {
        Ok(resp) => Ok(resp
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default()),
        Err(local_err) => try_cloud_fallback(
            &state.cloud_pool,
            packed_messages,
            &decision.model,
            local_err,
        )
        .await,
    }
}

// ─────────────────────────────────────────────────────────────────
// Gaming VM orchestration
// ─────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct GamingRequest {
    action: String,
    vm_name: Option<String>,
}

pub async fn gaming_handler(
    State(state): State<AppState>,
    Json(req): Json<GamingRequest>,
) -> impl IntoResponse {
    let config = match state.gaming_config.as_ref() {
        Some(c) => c,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Gaming is not configured"})),
            )
                .into_response();
        }
    };

    match req.action.as_str() {
        "status" | "list-gpus" => {
            match ygg_gaming::orchestrator::status_all(config).await {
                Ok(status) => match serde_json::to_value(&status) {
                    Ok(v) => Json(v).into_response(),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": e.to_string()})),
                    )
                        .into_response(),
                },
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response(),
            }
        }
        "launch" => {
            let Some(vm_name) = req.vm_name.as_deref() else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "vm_name is required for launch"})),
                )
                    .into_response();
            };
            match ygg_gaming::orchestrator::launch(config, vm_name).await {
                Ok(result) => match serde_json::to_value(&result) {
                    Ok(v) => Json(v).into_response(),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": e.to_string()})),
                    )
                        .into_response(),
                },
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response(),
            }
        }
        "stop" => {
            let Some(vm_name) = req.vm_name.as_deref() else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "vm_name is required for stop"})),
                )
                    .into_response();
            };
            match ygg_gaming::orchestrator::stop(config, vm_name).await {
                Ok(()) => Json(json!({"status": "stopped", "vm_name": vm_name})).into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response(),
            }
        }
        other => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Unknown action: {other}")})),
        )
            .into_response(),
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
