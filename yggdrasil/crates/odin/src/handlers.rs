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
use crate::router::{RouterMethod, RoutingDecision};
use crate::session::CompactMessage;
use crate::state::{AppState, BackendState, CloudPool};

// ─────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────

/// Distinguishes the origin of a `process_chat_text` call so the voice
/// pipeline can override model selection and tool loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatSource {
    Http,
    Voice,
}

// ─────────────────────────────────────────────────────────────────
// Flow → SSE dispatch (Sprint 061)
// ─────────────────────────────────────────────────────────────────

/// Execute a flow, returning either a streaming SSE response (`stream=true`)
/// or a buffered `ChatCompletionResponse` JSON (`stream=false`). Non-streaming
/// clients (e.g. MCP `generate_tool`) get a single JSON blob with the
/// assistant-facing final step's text; streaming clients see per-token SSE +
/// `event: ygg_step` thinking frames. Either path drives the flow via
/// `execute_streaming`, appends the assistant message to the session, and
/// spawns the engram-store fire-and-forget task.
pub async fn dispatch_flow(
    state: AppState,
    flow: ygg_domain::config::FlowConfig,
    user_message: String,
    images: Option<Vec<String>>,
    session_id: String,
    stream: bool,
) -> Response {
    if stream {
        dispatch_flow_sse(state, flow, user_message, images, session_id).await
    } else {
        dispatch_flow_json(state, flow, user_message, images, session_id).await
    }
}

/// Non-streaming flow dispatch: drains the flow's StreamEvent channel,
/// accumulates the assistant-role content into a single string, and returns
/// a standard `ChatCompletionResponse` JSON. Intermediate "thinking" step
/// output is discarded (not visible to a non-streaming client by design).
async fn dispatch_flow_json(
    state: AppState,
    flow: ygg_domain::config::FlowConfig,
    user_message: String,
    images: Option<Vec<String>>,
    session_id: String,
) -> Response {
    let completion_id = format!("chatcmpl-{}", Uuid::new_v4());
    let model_name = flow.name.clone();

    // Drive the flow synchronously so we can inspect its FlowResult before
    // serialising the JSON response. The StreamEvent channel is kept small;
    // we spawn a drainer task to prevent the engine from blocking on send.
    let (tx, mut rx) = crate::flow_streaming::channel();
    let drainer = tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Intentionally discard — the FlowResult below is authoritative.
        }
    });

    let flow_for_engine = flow.clone();
    let user_msg_for_engine = user_message.clone();
    let imgs_for_engine = images.clone();
    let state_for_engine = state.clone();
    let result = state_for_engine
        .flow_engine
        .execute_streaming(
            &flow_for_engine,
            &user_msg_for_engine,
            imgs_for_engine.as_deref(),
            Some(&state_for_engine),
            tx,
        )
        .await;
    // Wait for the drainer to finish (channel dropped by now).
    let _ = drainer.await;

    let response_text = match result {
        Ok(flow_result) => {
            for timing in &flow_result.step_timings {
                tracing::info!(
                    flow = %flow.name,
                    step = %timing.name,
                    model = %timing.model,
                    ms = timing.elapsed_ms,
                    chars = timing.output_chars,
                    "flow step timing"
                );
            }
            flow_result.final_output().to_string()
        }
        Err(e) => {
            tracing::error!(flow = %flow.name, error = %e, "flow execution failed");
            return OdinError::Upstream(format!("flow '{}' failed: {e}", flow.name))
                .into_response();
        }
    };

    state.session_store.append_messages(
        &session_id,
        &[CompactMessage::new("assistant", &response_text)],
    );
    if state.config.mimir.store_on_completion {
        spawn_engram_store(
            state.http_client.clone(),
            state.mimir_url.clone(),
            user_message.clone(),
            response_text.clone(),
        );
    }

    let json_body = crate::openai::ChatCompletionResponse {
        id: completion_id,
        object: "chat.completion".to_string(),
        created: crate::proxy::unix_now(),
        model: model_name,
        choices: vec![crate::openai::Choice {
            index: 0,
            message: ChatMessage::new(Role::Assistant, &response_text),
            finish_reason: Some("stop".to_string()),
        }],
        usage: Some(crate::openai::Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        }),
    };
    let mut resp = axum::Json(json_body).into_response();
    if let Ok(val) = axum::http::HeaderValue::from_str(&session_id) {
        resp.headers_mut().insert("x-session-id", val);
    }
    resp
}

/// Streaming flow dispatch: spawns the flow in a background task feeding a
/// `StreamEvent` channel; the HTTP response is an SSE stream that renders
/// each event via `flow_streaming::to_sse_events`.
pub async fn dispatch_flow_sse(
    state: AppState,
    flow: ygg_domain::config::FlowConfig,
    user_message: String,
    images: Option<Vec<String>>,
    session_id: String,
) -> Response {
    use axum::response::sse::Event;
    use futures::StreamExt;
    use tokio_stream::wrappers::ReceiverStream;

    let completion_id = format!("chatcmpl-{}", Uuid::new_v4());
    let model_name = flow.name.clone();

    let (tx, rx) = crate::flow_streaming::channel();

    // Background task: drive the flow to completion and persist results.
    {
        let state = state.clone();
        let flow = flow.clone();
        let user_msg = user_message.clone();
        let imgs = images.clone();
        let sid = session_id.clone();
        let mimir_url = state.mimir_url.clone();
        let http = state.http_client.clone();
        let store_on_completion = state.config.mimir.store_on_completion;
        tokio::spawn(async move {
            let result = state
                .flow_engine
                .execute_streaming(&flow, &user_msg, imgs.as_deref(), Some(&state), tx)
                .await;

            match result {
                Ok(flow_result) => {
                    let response_text = flow_result.final_output().to_string();
                    state.session_store.append_messages(
                        &sid,
                        &[CompactMessage::new("assistant", &response_text)],
                    );
                    if store_on_completion {
                        spawn_engram_store(http, mimir_url, user_msg, response_text);
                    }
                    for timing in &flow_result.step_timings {
                        tracing::info!(
                            flow = %flow.name,
                            step = %timing.name,
                            model = %timing.model,
                            ms = timing.elapsed_ms,
                            chars = timing.output_chars,
                            "flow step timing"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(flow = %flow.name, error = %e, "flow execution failed");
                }
            }
        });
    }

    let completion_id_for_stream = completion_id.clone();
    let model_for_stream = model_name.clone();
    let sse_stream = ReceiverStream::new(rx).flat_map(move |evt| {
        let events = crate::flow_streaming::to_sse_events(
            &evt,
            &completion_id_for_stream,
            &model_for_stream,
        );
        futures::stream::iter(
            events
                .into_iter()
                .map(Ok::<Event, std::convert::Infallible>)
                .collect::<Vec<_>>(),
        )
    });

    let mut resp = Sse::new(sse_stream).into_response();
    if let Ok(val) = axum::http::HeaderValue::from_str(&session_id) {
        resp.headers_mut().insert("x-session-id", val);
    }
    resp
}

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

/// Hybrid SDR + LLM intent classification (Sprint 052).
///
/// 1. If `query_sdr` is available, run SDR prototype match (~4μs).
/// 2. If LLM router is available, send classification request with SDR hint.
/// 3. Merge: LLM wins when available; SDR-only above 0.85; keyword fallback.
/// 4. Fire-and-forget logging of the routing decision.
async fn hybrid_classify(
    state: &AppState,
    message: &str,
    query_sdr: Option<&ygg_domain::sdr::Sdr>,
) -> RoutingDecision {
    let start = std::time::Instant::now();

    // SDR classification (~4μs).
    let sdr_result = if let Some(sdr) = query_sdr {
        state.sdr_router.classify(sdr).await
    } else {
        None
    };

    if let Some(ref cls) = sdr_result {
        crate::metrics::record_sdr_classification(&cls.intent, cls.confidence);
    }

    // LLM classification (<500ms) — uses queue if available, direct call otherwise.
    let llm_result = if let Some(ref client) = state.llm_router {
        if let Some(ref queue) = state.router_queue {
            let priority = crate::request_queue::RequestPriority::Interactive;
            let rx = queue.submit(
                message.to_string(),
                query_sdr.copied(),
                sdr_result.clone(),
                priority,
            );
            // Await with a generous timeout (the queue + LLM timeout handle the real limit).
            tokio::time::timeout(
                std::time::Duration::from_millis(2000),
                rx,
            )
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
        } else {
            client.classify(message, sdr_result.as_ref()).await
        }
    } else {
        None
    };

    let router_latency = start.elapsed();
    if llm_result.is_some() {
        crate::metrics::record_llm_classification_latency(router_latency.as_secs_f64());
    }

    // Merge decisions: LLM wins → SDR-only → keyword fallback.
    let (mut decision, method) = match (&llm_result, &sdr_result) {
        (Some(llm), Some(sdr)) if llm.agrees_with_sdr => {
            // LLM confirmed SDR — reinforce the prototype.
            if let Some(qsdr) = query_sdr {
                state.sdr_router.reinforce(&llm.intent, qsdr).await;
            }
            crate::metrics::record_router_agreement(true);
            let d = state.router.resolve_intent(&llm.intent)
                .unwrap_or_else(|| state.router.classify(message));
            (d, RouterMethod::LlmConfirmed)
        }
        (Some(llm), _) => {
            // LLM overrode SDR (or SDR had no suggestion).
            if sdr_result.is_some() {
                crate::metrics::record_router_agreement(false);
            }
            let d = state.router.resolve_intent(&llm.intent)
                .unwrap_or_else(|| state.router.classify(message));
            (d, RouterMethod::LlmOverride)
        }
        (None, Some(sdr)) if sdr.confidence > 0.85 => {
            // LLM unavailable but SDR is very confident.
            crate::metrics::record_router_fallback("llm_unavailable");
            let d = state.router.resolve_intent(&sdr.intent)
                .unwrap_or_else(|| state.router.classify(message));
            (d, RouterMethod::SdrOnly)
        }
        _ => {
            // Both unavailable or low confidence — Sprint 061: run keyword
            // classification FIRST. If it matches a real intent (coding,
            // reasoning, etc.), route there. Only fall back to the configured
            // `intent_default` (e.g. "chat" → swarm_chat) when the keyword
            // classifier produced a generic/unmatched intent.
            //
            // Sprint 062 P1a: the previous `is_generic` check collapsed three
            // distinct origins of `intent="default"` (no-match, gaming-
            // suppressed, and strong-match-but-low-LLM-confidence) into one
            // bucket, which made `intent_default` clobber real HA matches
            // whenever the LLM wasn't confident. Use the new
            // `keyword_match_kind` + `keyword_match_count` signal to gate the
            // override precisely:
            //   - Matched with count >= 2: trust the keyword classifier, never
            //     override (HA and similar high-signal intents win).
            //   - Suppressed: keep the suppression — the whole point of the
            //     gaming override is to fall through to the LLM/default, so
            //     we do NOT apply `intent_default` here either (it would
            //     defeat the suppression by re-routing into chat).
            //   - None (truly no keyword match) AND LLM was absent or below
            //     0.4 confidence: apply the configured `intent_default`.
            if state.llm_router.is_some() {
                crate::metrics::record_router_fallback("low_confidence");
            }
            let keyword_decision = state.router.classify(message);

            let strong_keyword_match = matches!(
                keyword_decision.keyword_match_kind,
                crate::router::KeywordMatchKind::Matched
            ) && keyword_decision.keyword_match_count >= 2;

            // Only consider intent_default override when NO keyword rule
            // matched at all AND the LLM has no usable confidence signal.
            let llm_confidence = llm_result.as_ref().map(|l| l.confidence);
            let llm_too_weak = llm_confidence
                .map(|c| c < 0.4)
                .unwrap_or(true);
            let should_apply_intent_default = !strong_keyword_match
                && keyword_decision.keyword_match_count == 0
                && matches!(
                    keyword_decision.keyword_match_kind,
                    crate::router::KeywordMatchKind::None
                )
                && llm_too_weak;

            let decision = if should_apply_intent_default {
                match state.config.routing.intent_default.as_deref() {
                    Some(default_intent) => state
                        .router
                        .resolve_intent(default_intent)
                        .unwrap_or(keyword_decision),
                    None => keyword_decision,
                }
            } else {
                keyword_decision
            };
            (decision, RouterMethod::Fallback)
        }
    };

    // Set confidence and method on the decision.
    decision.confidence = llm_result.as_ref().map(|l| l.confidence)
        .or(sdr_result.as_ref().map(|s| s.confidence));
    decision.router_method = method;

    if let Some(confidence) = decision.confidence {
        crate::metrics::record_routing_confidence(
            &decision.intent,
            &format!("{:?}", method),
            confidence,
        );
    }

    decision
}

/// Acquire a backend semaphore, rerouting to a fallback if the primary is at capacity.
///
/// Returns a reference to the chosen backend and its semaphore permit.
/// Mutates `decision` in place if a fallback is selected so downstream
/// dispatch uses the correct URL/model.
fn acquire_with_fallback<'a>(
    state: &'a AppState,
    decision: &mut crate::router::RoutingDecision,
) -> Result<(&'a BackendState, tokio::sync::SemaphorePermit<'a>), OdinError> {
    let backend = state
        .backends
        .iter()
        .find(|b| b.name == decision.backend_name)
        .ok_or_else(|| {
            OdinError::Internal(format!(
                "backend '{}' not found in state",
                decision.backend_name
            ))
        })?;

    // Happy path: primary backend has capacity.
    if let Ok(permit) = backend.semaphore.try_acquire() {
        return Ok((backend, permit));
    }

    // Primary at capacity — try fallback.
    tracing::warn!(
        primary = %decision.backend_name,
        model = %decision.model,
        "backend at capacity — attempting fallback reroute"
    );

    if let Some(fb) = state.find_fallback_backend(&decision.backend_name, &decision.model)
        && let Ok(permit) = fb.semaphore.try_acquire()
    {
        tracing::info!(
            from = %decision.backend_name,
            to = %fb.name,
            "rerouted to fallback backend"
        );
        decision.backend_name = fb.name.clone();
        decision.backend_url = fb.url.clone();
        if let Some(m) = fb.models.first() {
            decision.model = m.clone();
        }
        decision.backend_type = fb.backend_type.clone();
        return Ok((fb, permit));
    }

    Err(OdinError::BackendUnavailable(format!(
        "backend '{}' is at capacity and no fallback available",
        decision.backend_name
    )))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn to_cloud_messages(packed: &[ChatMessage]) -> Vec<ygg_cloud::adapter::ChatMessage> {
    packed
        .iter()
        .map(|m| ygg_cloud::adapter::ChatMessage {
            role: m.role.to_string(),
            content: m.content.clone(),
        })
        .collect()
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
        let cloud_messages = to_cloud_messages(packed_messages);
        if let Some(cloud_content) = pool.fallback_chat(cloud_messages, Some(model)).await {
            tracing::info!("cloud fallback produced response for voice pipeline");
            return Ok(cloud_content);
        }
    }
    Err(local_err)
}

/// Try cloud fallback, returning a full `ChatCompletionResponse`.
///
/// Used by `chat_handler` paths where the caller needs a structured response
/// rather than just a plain string.
async fn try_cloud_or_fail(
    cloud_pool: &Option<CloudPool>,
    packed_messages: &[ChatMessage],
    model: &str,
    completion_id: &str,
    local_err: OdinError,
) -> Result<crate::openai::ChatCompletionResponse, OdinError> {
    if let Some(pool) = cloud_pool
        && pool.fallback_enabled
    {
        let cloud_messages = to_cloud_messages(packed_messages);
        if let Some(cloud_content) = pool.fallback_chat(cloud_messages, Some(model)).await {
            tracing::info!("cloud fallback produced response");
            return Ok(crate::openai::ChatCompletionResponse {
                id: completion_id.to_string(),
                object: "chat.completion".to_string(),
                created: unix_now(),
                model: format!("cloud-fallback:{model}"),
                choices: vec![crate::openai::Choice {
                    index: 0,
                    message: ChatMessage::new(Role::Assistant, cloud_content),
                    finish_reason: Some("stop".to_string()),
                }],
                usage: None,
            });
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
                    flow: None,
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
    source: ChatSource,
) -> Result<String, OdinError> {
    // ── 1. Append user message to session ─────────────────────────
    state
        .session_store
        .append_messages(session_id, &[CompactMessage::new("user", text)]);

    let user_text = text.to_string();

    // ── 2. Fetch memory events (used for routing + SDR) ─────────
    let recall = rag::fetch_memory_events(
        &state.http_client,
        &state.mimir_url,
        &user_text,
        state.config.mimir.query_limit,
    )
    .await;

    let query_sdr = recall.as_ref()
        .and_then(|r| r.query_sdr_hex.as_ref())
        .and_then(|hex| ygg_domain::sdr::from_hex(hex));

    // ── 2b. Hybrid SDR + LLM routing (Sprint 052) ────────────────
    let mut decision = if state.llm_router.is_some() {
        hybrid_classify(state, &user_text, query_sdr.as_ref()).await
    } else {
        state.router.classify(&user_text)
    };

    // ── 3. Memory-event routing refinement ────────────────────────
    if let Some(ref recall) = recall {
        memory_router::apply_memory_events(recall, &mut decision);

        if let Some(ref sdr) = query_sdr
            && let Some(drift) = state.session_store.update_session_sdr(session_id, sdr) {
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

    // ── 3b. Voice model override ──────────────────────────────────
    if source == ChatSource::Voice {
        if let Some(ref voice_cfg) = state.config.voice {
            if let Some(ref voice_model) = voice_cfg.model {
                if let Some(resolved) = state.router.resolve_backend_for_model(voice_model) {
                    tracing::info!(
                        old_model = %decision.model,
                        new_model = %voice_model,
                        "voice: overriding model to voice-specific model"
                    );
                    decision.model = resolved.model;
                    decision.backend_url = resolved.backend_url;
                    decision.backend_name = resolved.backend_name;
                    decision.backend_type = resolved.backend_type;
                } else {
                    tracing::warn!(
                        voice_model = %voice_model,
                        "voice model not found in any backend — falling back to routed model"
                    );
                }
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

    // ── 4. Acquire semaphore (with fallback reroute) ──────────────
    let (backend_state, _permit) = acquire_with_fallback(state, &mut decision)?;
    let _backend_guard = BackendActiveGuard::new(&decision.backend_name);

    // ── 5. Fetch RAG context ──────────────────────────────────────
    let span = tracing::info_span!("rag_fetch_voice");
    let rag_context = {
        let _guard = span.enter();
        rag::fetch_context(state, &user_text, &decision.intent).await
    };

    // ── 6. Pack context with session history ──────────────────────
    let backend_context_window = backend_state.context_window;
    // Voice pipeline uses the "voice" persona (Fergus) regardless of routed intent.
    // Inject gaming VM context so the LLM knows to use the `gaming` tool.
    let gaming_ctx = state.gaming_config.as_ref().map(|gc| {
        let names: Vec<String> = gc.hosts.iter().flat_map(|h| {
            let vms = h.vms.iter().map(|v| v.name.as_str());
            let cts = h.containers.iter().map(|c| c.name.as_str());
            vms.chain(cts).map(|n| format!("{}/{}", h.name, n))
        }).collect();
        format!("Managed VMs/containers: {}", names.join(", "))
    });
    let voice_role = if source == ChatSource::Voice { "voice_split" } else { "voice" };
    let system_prompt = rag::build_system_prompt(&rag_context, voice_role, gaming_ctx.as_deref());
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
                flow: None,
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

            // Voice pipeline: keyword-based tool selection from the user's query.
            // Only tools whose keywords match the transcript (+ core tools) are loaded,
            // keeping context overhead minimal for smaller voice models.
            // Text/API pipeline: load all tools as before.
            let tool_defs: Vec<_> = if source == ChatSource::Voice {
                if let Some(ref names) = state.config.voice.as_ref().and_then(|v| v.tools.as_ref()) {
                    // Explicit allowlist in config takes priority.
                    crate::tool_registry::to_tool_definitions_filtered(
                        &state.tool_registry,
                        &allowed_tiers,
                        names,
                    )
                } else {
                    let selected = crate::tool_registry::select_tools_for_query(
                        &state.tool_registry,
                        &user_text,
                        &allowed_tiers,
                    );
                    tracing::info!(
                        query = %user_text,
                        tools_selected = selected.len(),
                        tools = ?selected.iter().map(|t| t.function.name.as_str()).collect::<Vec<_>>(),
                        "voice: keyword tool selection"
                    );
                    selected
                }
            } else {
                crate::tool_registry::to_tool_definitions(&state.tool_registry, &allowed_tiers)
            };

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
    // Skip storing [NOT_ADDRESSED] responses — they poison session context
    // and teach the model to keep responding with [NOT_ADDRESSED].
    let dominated_by_tag = effect.trim() == "[NOT_ADDRESSED]"
        || effect.trim() == "[DISMISS]"
        || effect.trim().is_empty();
    if !dominated_by_tag {
        state
            .session_store
            .append_messages(session_id, &[CompactMessage::new("assistant", &effect)]);
    }

    if state.config.mimir.store_on_completion && !dominated_by_tag {
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
    let e2e_start = std::time::Instant::now();

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

    // ── 5. Fetch memory events (used for routing refinement + SDR) ──
    let recall = rag::fetch_memory_events(
        &state.http_client,
        &state.mimir_url,
        &last_user_message,
        state.config.mimir.query_limit,
    )
    .await;

    // Extract query SDR from the recall response (free — already computed by Mimir).
    let query_sdr = recall.as_ref()
        .and_then(|r| r.query_sdr_hex.as_ref())
        .and_then(|hex| ygg_domain::sdr::from_hex(hex));

    // ── 5a. Sprint 063 P1: explicit flow pin ────────────────────
    // When the caller sets `ChatCompletionRequest.flow`, bypass intent
    // classification entirely and dispatch directly to the named flow
    // (provided its trigger is `Manual` or `Intent(_)` — cron-only flows
    // are rejected because they must not be user-invocable).
    if let Some(ref flow_name) = request.flow {
        let flows_snapshot = state.flows.read().unwrap().clone();
        match crate::flow::classify_explicit_flow(&flows_snapshot, flow_name) {
            crate::flow::ExplicitFlowVerdict::NotFound => {
                return Err(OdinError::BadRequest(format!("flow not found: {flow_name}")));
            }
            crate::flow::ExplicitFlowVerdict::CronOnly => {
                return Err(OdinError::BadRequest(format!(
                    "flow '{flow_name}' is cron-only, not user-invocable"
                )));
            }
            crate::flow::ExplicitFlowVerdict::NotInvocable => {
                return Err(OdinError::BadRequest(format!(
                    "flow '{flow_name}' is not user-invocable (trigger type cannot be pinned)"
                )));
            }
            crate::flow::ExplicitFlowVerdict::Dispatch => {
                // Safe to unwrap — classify_explicit_flow already verified it exists.
                let flow = state
                    .flow_engine
                    .find_by_name(&flows_snapshot, flow_name)
                    .expect("flow existence was verified by classify_explicit_flow");
                tracing::info!(flow = %flow_name, "explicit flow invocation");
                crate::metrics::record_explicit_flow_invocation(flow_name);
                let flow_cloned = flow.clone();
                let pinned_images: Option<Vec<String>> = request
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == Role::User)
                    .and_then(|m| m.images.clone())
                    .filter(|imgs| !imgs.is_empty());
                return Ok(dispatch_flow(
                    state.clone(),
                    flow_cloned,
                    last_user_message.clone(),
                    pinned_images,
                    session_id.clone(),
                    request.stream,
                )
                .await);
            }
        }
    }

    // ── 5b. Hybrid SDR + LLM routing (Sprint 052) ────────────────
    let mut decision = if let Some(ref model) = request.model {
        state.router.resolve_backend_for_model(model).ok_or_else(|| {
            OdinError::BadRequest(format!("model not found: {model}"))
        })?
    } else if state.llm_router.is_some() {
        hybrid_classify(&state, &last_user_message, query_sdr.as_ref()).await
    } else {
        state.router.classify(&last_user_message)
    };

    // ── 6. Memory-event routing refinement (Sprint 015) ───────────
    if let Some(ref recall) = recall {
        memory_router::apply_memory_events(recall, &mut decision);

        // ── 6b. Topic drift tracking via session SDR ─────────────
        if let Some(ref sdr) = query_sdr
            && let Some(drift) = state.session_store.update_session_sdr(&session_id, sdr) {
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
        confidence = ?decision.confidence,
        method = ?decision.router_method,
        model = %decision.model,
        backend = %decision.backend_name,
        session_id = %session_id,
        "routing decision (post memory refinement)"
    );

    crate::metrics::record_routing_intent(&decision.intent);

    // Non-blocking request log (Sprint 052).
    if let Some(ref log_writer) = state.request_log {
        let entry = crate::request_log::RequestLogEntry {
            request_id: format!("chatcmpl-{}", Uuid::new_v4()),
            timestamp: chrono::Utc::now(),
            source: "http".into(),
            user_message: last_user_message.clone(),
            sdr_intent: None, // TODO: pass through from hybrid_classify
            sdr_confidence: None,
            llm_intent: None,
            llm_confidence: None,
            llm_agrees_with_sdr: None,
            final_intent: decision.intent.clone(),
            router_method: format!("{:?}", decision.router_method),
            model: decision.model.clone(),
            backend: decision.backend_name.clone(),
            e2e_latency_ms: e2e_start.elapsed().as_millis() as u64,
            router_latency_ms: decision.confidence.map(|_| e2e_start.elapsed().as_millis() as u64),
            tokens_in: None,
            tokens_out: None,
            session_id: session_id.clone(),
        };
        let writer = log_writer.clone();
        tokio::spawn(async move { writer.log(&entry).await });
    }

    // ── 6c. Multimodal flow dispatch (Sprint 057) ──────────────────
    // If the request contains images/audio, route to the "omni" modality flow.
    let request_images: Option<Vec<String>> = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .and_then(|m| m.images.clone())
        .filter(|imgs| !imgs.is_empty());

    if let Some(ref imgs) = request_images {
        let flows_snapshot = state.flows.read().unwrap().clone();
        if let Some(flow) = state.flow_engine.find_by_modality(&flows_snapshot, "omni") {
            tracing::info!(
                flow = %flow.name,
                images = imgs.len(),
                "dispatching multimodal request to perceive flow (SSE)"
            );
            return Ok(dispatch_flow(
                state.clone(),
                flow.clone(),
                last_user_message.clone(),
                Some(imgs.clone()),
                session_id.clone(),
                request.stream,
            )
            .await);
        }
    }

    // ── 6d. Flow dispatch (Sprint 055 → Sprint 061: SSE-only) ──────
    // If a flow is configured for this intent, execute it via SSE streaming.
    // Sprint 061: all flow dispatches return SSE; the JSON response path for
    // flows is removed (single-model dispatch still supports stream=false).
    let flows_snapshot = state.flows.read().unwrap().clone();
    if let Some(flow) = state.flow_engine.find_by_intent(&flows_snapshot, &decision.intent) {
        tracing::info!(
            flow = %flow.name,
            intent = %decision.intent,
            "dispatching to multi-model flow (SSE)"
        );
        return Ok(dispatch_flow(
            state.clone(),
            flow.clone(),
            last_user_message.clone(),
            None,
            session_id.clone(),
            request.stream,
        )
        .await);
    }

    // ── 7. Acquire semaphore (with fallback reroute) ─────────────
    let (backend_state, _permit) = acquire_with_fallback(&state, &mut decision)?;
    let _backend_guard = BackendActiveGuard::new(&decision.backend_name);

    // ── 8. Fetch RAG context ─────────────────────────────────────
    let span = tracing::info_span!("rag_fetch");
    let rag_context = {
        let _guard = span.enter();
        rag::fetch_context(&state, &last_user_message, &decision.intent).await
    };

    // ── 9. Pack context with session history ──────────────────────
    let backend_context_window = backend_state.context_window;
    let system_prompt = rag::build_system_prompt(&rag_context, &decision.intent, None);
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
                flow: None,
            };

            if request.stream {
                let openai_request_clone = openai_request.clone();
                let handle = match proxy::stream_chat_openai(
                    state.http_client.clone(),
                    decision.backend_url.clone(),
                    openai_request,
                )
                .await
                {
                    Ok(h) => h,
                    Err(e) if e.is_connection_error() => {
                        if let Some(fb) = state.find_fallback_backend(&decision.backend_name, &decision.model) {
                            tracing::warn!(
                                from = %decision.backend_name,
                                to = %fb.name,
                                "stream connection error — failing over (openai)"
                            );
                            decision.backend_url = fb.url.clone();
                            decision.backend_name = fb.name.clone();
                            proxy::stream_chat_openai(
                                state.http_client.clone(),
                                fb.url.clone(),
                                openai_request_clone,
                            )
                            .await?
                        } else {
                            return Err(e);
                        }
                    }
                    Err(e) => return Err(e),
                };

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
                        try_cloud_or_fail(&state.cloud_pool, &packed_messages, &decision.model, &completion_id, local_err).await?
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
                let ollama_request_clone = ollama_request.clone();
                let handle = match proxy::stream_chat(
                    state.http_client.clone(),
                    decision.backend_url.clone(),
                    ollama_request,
                    completion_id.clone(),
                )
                .await
                {
                    Ok(h) => h,
                    Err(e) if e.is_connection_error() => {
                        if let Some(fb) = state.find_fallback_backend(&decision.backend_name, &decision.model) {
                            tracing::warn!(
                                from = %decision.backend_name,
                                to = %fb.name,
                                "stream connection error — failing over"
                            );
                            let mut retry_req = ollama_request_clone;
                            if !fb.models.iter().any(|m| m == &decision.model)
                                && let Some(fb_model) = fb.models.first()
                            {
                                retry_req.model = fb_model.clone();
                            }
                            // Update decision fields for the background task.
                            decision.backend_url = fb.url.clone();
                            decision.backend_name = fb.name.clone();
                            decision.model = retry_req.model.clone();
                            proxy::stream_chat(
                                state.http_client.clone(),
                                fb.url.clone(),
                                retry_req,
                                completion_id.clone(),
                            )
                            .await?
                        } else {
                            return Err(e);
                        }
                    }
                    Err(e) => return Err(e),
                };

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
                let ollama_request_clone = ollama_request.clone();
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
                    Err(local_err) if local_err.is_connection_error() => {
                        // ── Connection-error failover: try alternate backend ──
                        if let Some(fb) = state.find_fallback_backend(&decision.backend_name, &decision.model) {
                            tracing::warn!(
                                from = %decision.backend_name,
                                to = %fb.name,
                                "connection error — failing over to alternate backend"
                            );
                            let fb_permit = fb.semaphore.try_acquire().map_err(|_| {
                                OdinError::BackendUnavailable(format!(
                                    "fallback backend '{}' also at capacity", fb.name
                                ))
                            })?;
                            let mut retry_req = ollama_request_clone;
                            // Use fallback's first model if original model isn't available there.
                            if !fb.models.iter().any(|m| m == &decision.model)
                                && let Some(fb_model) = fb.models.first()
                            {
                                retry_req.model = fb_model.clone();
                            }
                            let retry_result = proxy::generate_chat(
                                &state.http_client,
                                &fb.url,
                                retry_req,
                                &completion_id,
                            )
                            .await;
                            drop(fb_permit);
                            match retry_result {
                                Ok(resp) => resp,
                                Err(retry_err) => return Err(retry_err),
                            }
                        } else {
                            // No alternate backend — try cloud fallback
                            try_cloud_or_fail(&state.cloud_pool, &packed_messages, &decision.model, &completion_id, local_err).await?
                        }
                    }
                    Err(local_err) => {
                        // Non-connection error — try cloud fallback directly
                        try_cloud_or_fail(&state.cloud_pool, &packed_messages, &decision.model, &completion_id, local_err).await?
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

/// Sprint 065 C·P2: internal activity exposure for ygg-dreamer.
///
/// Returns `{"idle_secs": N}` where N is the number of seconds since the
/// last chat request was processed. Consumed by the dreamer's activity
/// poller to decide whether to fire warmup / dream flows. Bound to an
/// `/internal/` path prefix so it's never mistaken for a user-facing API.
pub async fn internal_activity(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let idle_secs = state.activity_tracker.idle_duration().as_secs();
    Json(serde_json::json!({ "idle_secs": idle_secs }))
}

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
// GET /api/v1/mesh/nodes  and  /api/v1/mesh/services
// ─────────────────────────────────────────────────────────────────

/// Return known mesh nodes compiled from Odin's own config.
///
/// This provides a consistent topology view without requiring ygg-node.
/// Each node entry lists name, URL, type, and model availability.
pub async fn mesh_nodes_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut nodes = vec![];

    // Odin itself
    nodes.push(json!({
        "name": state.config.node_name,
        "role": "orchestrator",
        "url": format!("http://{}", state.config.listen_addr),
        "status": "ok",
    }));

    // LLM backends
    for backend in &state.backends {
        nodes.push(json!({
            "name": backend.name,
            "role": "llm_backend",
            "url": backend.url,
            "type": format!("{:?}", backend.backend_type),
            "models": backend.models,
            "available_permits": backend.semaphore.available_permits(),
            "context_window": backend.context_window,
        }));
    }

    Json(json!({ "nodes": nodes }))
}

/// Return known services and their health status.
///
/// Probes Mimir, Muninn, and all backends with a 2-second timeout each.
pub async fn mesh_services_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut services = vec![];

    // Mimir
    let mimir_status = check_service_health(&state.http_client, &state.mimir_url).await;
    services.push(json!({
        "name": "mimir",
        "role": "memory",
        "url": state.mimir_url,
        "status": mimir_status,
    }));

    // Muninn
    let muninn_status = check_service_health(&state.http_client, &state.muninn_url).await;
    services.push(json!({
        "name": "muninn",
        "role": "code_search",
        "url": state.muninn_url,
        "status": muninn_status,
    }));

    // HA
    if state.ha_client.is_some() {
        services.push(json!({
            "name": "home_assistant",
            "role": "device_control",
            "status": "configured",
        }));
    }

    // Backends
    for backend in &state.backends {
        let status = check_service_health(&state.http_client, &backend.url).await;
        services.push(json!({
            "name": backend.name,
            "role": "llm_backend",
            "url": backend.url,
            "status": status,
        }));
    }

    Json(json!({ "services": services }))
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
    pin: Option<String>,
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

    // Helper to serialize Ok results or return 500
    macro_rules! json_result {
        ($result:expr) => {
            match $result {
                Ok(val) => match serde_json::to_value(&val) {
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
        };
    }

    macro_rules! require_vm_name {
        ($req:expr, $action:expr) => {
            match $req.vm_name.as_deref() {
                Some(name) => name,
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("vm_name is required for {}", $action)})),
                    )
                        .into_response();
                }
            }
        };
    }

    match req.action.as_str() {
        "status" => json_result!(ygg_gaming::orchestrator::status_all(config).await),
        "list-gpus" => json_result!(ygg_gaming::orchestrator::list_gpus(config).await),
        "launch" => {
            let vm_name = require_vm_name!(req, "launch");
            json_result!(ygg_gaming::orchestrator::launch(config, vm_name).await)
        }
        "stop" => {
            let name = require_vm_name!(req, "stop");
            // Try VM first, fall back to container
            if config.find_vm(name).is_some() {
                match ygg_gaming::orchestrator::stop(config, name).await {
                    Ok(()) => Json(json!({"status": "stopped", "name": name})).into_response(),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": e.to_string()})),
                    )
                        .into_response(),
                }
            } else if config.find_container(name).is_some() {
                match ygg_gaming::orchestrator::stop_container(config, name).await {
                    Ok(()) => Json(json!({"status": "stopped", "name": name})).into_response(),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": e.to_string()})),
                    )
                        .into_response(),
                }
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": format!("'{}' not found in any host", name)})),
                )
                    .into_response()
            }
        }
        "start" => {
            let name = require_vm_name!(req, "start");
            if config.find_vm(name).is_some() {
                json_result!(ygg_gaming::orchestrator::launch(config, name).await)
            } else if config.find_container(name).is_some() {
                match ygg_gaming::orchestrator::start_container(config, name).await {
                    Ok(()) => Json(json!({"status": "started", "name": name})).into_response(),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": e.to_string()})),
                    )
                        .into_response(),
                }
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": format!("'{}' not found in any host", name)})),
                )
                    .into_response()
            }
        }
        "pair" => {
            let vm_name = require_vm_name!(req, "pair");
            let Some(pin) = req.pin.as_deref() else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "pin is required for pair (4-digit Moonlight PIN)"})),
                )
                    .into_response();
            };
            match ygg_gaming::orchestrator::pair(config, vm_name, pin).await {
                Ok(msg) => Json(json!({"status": "paired", "message": msg})).into_response(),
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

// ─────────────────────────────────────────────────────────────────
// Web search (Brave Search API)
// ─────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct WebSearchRequest {
    query: String,
    count: Option<usize>,
}

pub async fn web_search_handler(
    State(state): State<AppState>,
    Json(req): Json<WebSearchRequest>,
) -> impl IntoResponse {
    let config = match state.web_search_config.as_ref() {
        Some(c) => c,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Web search is not configured"})),
            )
                .into_response();
        }
    };

    let count = req.count.unwrap_or(config.max_results).min(10);

    let resp = state
        .http_client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", &config.api_key)
        .header("Accept", "application/json")
        .query(&[("q", &req.query), ("count", &count.to_string())])
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
            Ok(body) => {
                let results = body
                    .get("web")
                    .and_then(|w| w.get("results"))
                    .and_then(|r| r.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|item| {
                                json!({
                                    "title": item.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                                    "url": item.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                                    "description": item.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                Json(json!({ "results": results })).into_response()
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to parse search response: {e}")})),
            )
                .into_response(),
        },
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            tracing::warn!(%status, body = %text, "Brave Search API error");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("Brave Search API returned {status}: {text}")})),
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "Brave Search API request failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("Brave Search API request failed: {e}")})),
            )
                .into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Wake word enrollment
// ─────────────────────────────────────────────────────────────────

/// Enroll a wake word sample for a user.
///
/// `POST /api/v1/voice/enroll/:user_id`
///
/// Body: raw PCM s16le 16kHz mono bytes (application/octet-stream).
pub async fn wake_word_enroll(
    State(state): State<AppState>,
    axum::extract::Path(user_id): axum::extract::Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, OdinError> {
    if body.len() < 2 || body.len() % 2 != 0 {
        return Err(OdinError::BadRequest("body must be s16le PCM (even byte count)".into()));
    }

    let samples: Vec<i16> = body
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    let sdr_hex = state
        .wake_word_registry
        .enroll(&user_id, &samples, &state.skill_cache)
        .await;

    Ok(Json(serde_json::json!({
        "user_id": user_id,
        "sdr_hex": sdr_hex,
        "samples_bytes": body.len(),
    })))
}

/// List enrolled wake word users.
///
/// `GET /api/v1/voice/enroll`
pub async fn wake_word_list(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let users = state.wake_word_registry.list_users().await;
    let list: Vec<serde_json::Value> = users
        .into_iter()
        .map(|(id, count)| serde_json::json!({"user_id": id, "samples": count}))
        .collect();
    Json(serde_json::json!({"users": list}))
}

/// Remove all wake word samples for a user.
///
/// `DELETE /api/v1/voice/enroll/:user_id`
pub async fn wake_word_remove(
    State(state): State<AppState>,
    axum::extract::Path(user_id): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    let removed = state.wake_word_registry.remove_user(&user_id).await;
    Json(serde_json::json!({"user_id": user_id, "removed": removed}))
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/vault  (transparent Mimir proxy)
// ─────────────────────────────────────────────────────────────────

/// Transparent proxy to Mimir's `POST /api/v1/vault`.
pub async fn proxy_vault(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, OdinError> {
    let url = format!("{}/api/v1/vault", state.mimir_url);
    forward_to_mimir(&state.http_client, &url, body, "vault").await
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/build_check  (local cargo commands)
// ─────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct BuildCheckRequest {
    #[serde(default = "default_build_mode")]
    mode: String,
    package: Option<String>,
}

fn default_build_mode() -> String { "check".to_string() }

/// Run cargo check/build/clippy/test locally.
///
/// Returns the command output. Requires cargo on the host.
pub async fn build_check_handler(
    Json(req): Json<BuildCheckRequest>,
) -> impl IntoResponse {
    let mode = req.mode.as_str();
    let allowed = ["check", "build", "clippy", "test"];
    if !allowed.contains(&mode) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid mode '{}', allowed: {:?}", mode, allowed)})),
        ).into_response();
    }

    let mut args: Vec<&str> = vec![mode];
    let pkg;
    if let Some(ref p) = req.package {
        args.push("-p");
        pkg = p.clone();
        args.push(&pkg);
    }
    if mode == "clippy" {
        args.extend_from_slice(&["--", "-W", "clippy::all"]);
    }

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::process::Command::new("cargo")
            .args(&args)
            .output(),
    ).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let success = output.status.success();
            Json(json!({
                "success": success,
                "exit_code": output.status.code(),
                "stdout": stdout.chars().take(8000).collect::<String>(),
                "stderr": stderr.chars().take(8000).collect::<String>(),
            })).into_response()
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to run cargo: {e}")})),
        ).into_response(),
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({"error": "cargo command timed out after 120s"})),
        ).into_response(),
    }
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/deploy  (cargo build + rsync)
// ─────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct DeployRequest {
    action: String,
    service: String,
    target: Option<String>,
}

/// Build and/or deploy a Yggdrasil binary.
///
/// Actions: "build" (cargo build --release), "deploy" (rsync to target node),
/// "build_and_deploy" (both sequentially).
pub async fn deploy_handler(
    Json(req): Json<DeployRequest>,
) -> impl IntoResponse {
    let deploy_user = std::env::var("DEPLOY_USER").unwrap_or_else(|_| "yggdrasil".to_string());

    let allowed_actions = ["build", "deploy", "build_and_deploy", "status"];
    if !allowed_actions.contains(&req.action.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid action '{}', allowed: {:?}", req.action, allowed_actions)})),
        ).into_response();
    }

    match req.action.as_str() {
        "build" | "build_and_deploy" => {
            let build_result = tokio::time::timeout(
                std::time::Duration::from_secs(300),
                tokio::process::Command::new("cargo")
                    .args(["build", "--release", "--bin", &req.service])
                    .output(),
            ).await;

            match build_result {
                Ok(Ok(output)) if output.status.success() => {
                    if req.action == "build" {
                        return Json(json!({
                            "success": true,
                            "action": "build",
                            "service": req.service,
                        })).into_response();
                    }
                    // Fall through to deploy for build_and_deploy
                }
                Ok(Ok(output)) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                        "success": false,
                        "action": "build",
                        "error": stderr.chars().take(4000).collect::<String>(),
                    }))).into_response();
                }
                Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                    "error": format!("failed to run cargo: {e}")
                }))).into_response(),
                Err(_) => return (StatusCode::GATEWAY_TIMEOUT, Json(json!({
                    "error": "cargo build timed out after 300s"
                }))).into_response(),
            }

            // Deploy step (for build_and_deploy fallthrough or direct deploy)
            let target = req.target.as_deref().unwrap_or("munin");
            let binary = format!("target/release/{}", req.service);
            let dest = format!("/tmp/{}", req.service);

            let rsync_result = tokio::time::timeout(
                std::time::Duration::from_secs(60),
                tokio::process::Command::new("rsync")
                    .args(["-az", &binary, &format!("{deploy_user}@{target}:{dest}")])
                    .output(),
            ).await;

            match rsync_result {
                Ok(Ok(output)) if output.status.success() => {
                    Json(json!({
                        "success": true,
                        "action": "build_and_deploy",
                        "service": req.service,
                        "target": target,
                        "note": format!("Binary at {dest} on {target}. Run: sudo cp {dest} /opt/yggdrasil/bin/ && sudo systemctl restart yggdrasil-{}", req.service),
                    })).into_response()
                }
                Ok(Ok(output)) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                        "success": false,
                        "action": "deploy",
                        "error": stderr.chars().take(2000).collect::<String>(),
                    }))).into_response()
                }
                _ => (StatusCode::GATEWAY_TIMEOUT, Json(json!({
                    "error": "rsync timed out"
                }))).into_response(),
            }
        }
        "deploy" => {
            let target = req.target.as_deref().unwrap_or("munin");
            let binary = format!("target/release/{}", req.service);
            let dest = format!("/tmp/{}", req.service);

            let result = tokio::time::timeout(
                std::time::Duration::from_secs(60),
                tokio::process::Command::new("rsync")
                    .args(["-az", &binary, &format!("{deploy_user}@{target}:{dest}")])
                    .output(),
            ).await;

            match result {
                Ok(Ok(output)) if output.status.success() => {
                    Json(json!({
                        "success": true,
                        "action": "deploy",
                        "service": req.service,
                        "target": target,
                        "note": format!("Binary at {dest}. Run: sudo cp {dest} /opt/yggdrasil/bin/ && sudo systemctl restart yggdrasil-{}", req.service),
                    })).into_response()
                }
                Ok(Ok(output)) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                        "success": false,
                        "error": stderr.chars().take(2000).collect::<String>(),
                    }))).into_response()
                }
                _ => (StatusCode::GATEWAY_TIMEOUT, Json(json!({"error": "rsync timed out"}))).into_response(),
            }
        }
        "status" => {
            Json(json!({"action": "status", "note": "Use service_health tool to check service status"})).into_response()
        }
        _ => unreachable!(),
    }
}

// ─────────────────────────────────────────────────────────────────
// Agent streaming SSE endpoint
// ─────────────────────────────────────────────────────────────────

/// POST /v1/agent/stream — runs the agent loop with real-time SSE step events.
///
/// Accepts the same body as `/v1/chat/completions` (with `tools` array).
/// Returns an SSE stream of `AgentStepEvent` JSON objects, with the final
/// event containing the complete `ChatCompletionResponse`.
pub async fn agent_stream_handler(
    State(state): State<crate::state::AppState>,
    Json(request): Json<crate::openai::ChatCompletionRequest>,
) -> Sse<impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>> {
    use axum::response::sse::Event;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_stream::StreamExt;

    let (step_tx, step_rx) = mpsc::channel::<crate::agent::AgentStepEvent>(64);
    let (result_tx, result_rx) = mpsc::channel::<serde_json::Value>(1);

    // Spawn the agent loop in a background task.
    let state_clone = state.clone();
    tokio::spawn(async move {
        let registry = state_clone.tool_registry.clone();
        let config = state_clone.config.agent.clone().unwrap_or_default();
        let tools = crate::tool_registry::to_tool_definitions(&registry, &[
            crate::tool_registry::ToolTier::Safe,
            crate::tool_registry::ToolTier::Restricted,
        ]);
        let tiers = [crate::tool_registry::ToolTier::Safe, crate::tool_registry::ToolTier::Restricted];

        // Route to first Ollama backend.
        let decision = crate::router::RoutingDecision {
            intent: "agent".to_string(),
            confidence: None,
            router_method: crate::router::RouterMethod::Explicit,
            model: request.model.unwrap_or_else(|| state_clone.config.routing.default_model.clone()),
            backend_url: state_clone.backends.first().map(|b| b.url.clone()).unwrap_or_default(),
            backend_name: "default".to_string(),
            backend_type: BackendType::Ollama,
            keyword_match_count: 0,
            keyword_match_kind: crate::router::KeywordMatchKind::None,
            explicit_flow: None,
        };

        let completion_id = format!("agent-stream-{}", Uuid::new_v4());
        let result = crate::agent::run_agent_loop_inner(
            &state_clone,
            &request.messages,
            &tools,
            &registry,
            &tiers,
            &decision,
            &completion_id,
            &config,
            16384,
            Some(step_tx),
        )
        .await;

        match result {
            Ok(resp) => {
                let _ = result_tx.send(serde_json::to_value(resp).unwrap_or_default()).await;
            }
            Err(e) => {
                let _ = result_tx.send(json!({"error": e.to_string()})).await;
            }
        }
    });

    // Merge step events and final result into a single SSE stream.
    let step_stream = ReceiverStream::new(step_rx).map(|event| {
        Ok(Event::default()
            .event("step")
            .json_data(event)
            .unwrap_or_else(|_| Event::default().data("error")))
    });

    let result_stream = ReceiverStream::new(result_rx).map(|result| {
        Ok(Event::default()
            .event("result")
            .json_data(result)
            .unwrap_or_else(|_| Event::default().data("error")))
    });

    Sse::new(step_stream.chain(result_stream))
}

// ─────────────────────────────────────────────────────────────────
// Sprint 052: Request feedback + log query endpoints
// ─────────────────────────────────────────────────────────────────

/// `POST /api/v1/request/feedback` — AI-driven quality feedback on a routing decision.
pub async fn request_feedback_handler(
    State(state): State<AppState>,
    Json(req): Json<crate::request_log::FeedbackRequest>,
) -> Result<StatusCode, OdinError> {
    let writer = state.request_log.as_ref().ok_or_else(|| {
        OdinError::BadRequest("request logging is not enabled".to_string())
    })?;

    let entry = crate::request_log::FeedbackEntry {
        request_id: req.request_id,
        timestamp: chrono::Utc::now(),
        accuracy_rating: req.accuracy_rating,
        redo_requested: req.redo_requested,
        feedback_notes: req.feedback_notes,
    };

    writer.log_feedback(&entry).await;
    Ok(StatusCode::OK)
}

/// `GET /api/v1/request/log` — Query recent request log entries.
pub async fn request_log_query_handler(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<crate::request_log::LogQueryParams>,
) -> Result<Json<Vec<serde_json::Value>>, OdinError> {
    let writer = state.request_log.as_ref().ok_or_else(|| {
        OdinError::BadRequest("request logging is not enabled".to_string())
    })?;

    let results = writer.query_recent(&params).await;
    Ok(Json(results))
}

// ─── Webhook handler (Sprint 057) ────────────────────────────────────

/// HA webhook handler with AppState access for camera motion dispatch.
/// Sprint 064 P8 — daily E2E hit counter. The cron wrapper
/// (`scripts/smoke/e2e-cron-wrapper.sh`) pings this once per run; Prometheus
/// scrapes `odin_e2e_hits_total` to confirm the timer is firing.
pub async fn e2e_hit_handler() -> (axum::http::StatusCode, Json<serde_json::Value>) {
    crate::metrics::record_e2e_hit();
    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({ "ok": true })),
    )
}

pub async fn webhook_handler(
    State(state): State<AppState>,
    Json(payload): Json<ygg_ha::webhook::WebhookPayload>,
) -> (axum::http::StatusCode, Json<ygg_ha::webhook::WebhookResponse>) {
    tracing::info!(
        trigger_id = ?payload.trigger_id,
        entity_id = ?payload.entity_id,
        new_state = ?payload.new_state,
        "received HA webhook"
    );

    match payload.trigger_id.as_deref() {
        Some("motion_detected") => {
            let camera_name = payload
                .data
                .get("camera")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            tracing::info!(camera = camera_name, "motion detected — dispatching camera analysis");

            // Run analysis in background so the webhook returns immediately.
            let state_clone = state.clone();
            let cam = camera_name.to_string();
            tokio::spawn(async move {
                match crate::camera::handle_motion_event(&state_clone, &cam).await {
                    Ok(analysis) => {
                        tracing::info!(
                            camera = %analysis.camera,
                            important = analysis.is_important,
                            description = %analysis.description,
                            "camera analysis complete"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(camera = %cam, error = %e, "camera analysis failed");
                    }
                }
            });
        }
        Some("door_opened") => {
            tracing::info!(entity = ?payload.entity_id, "door opened event");
        }
        Some(trigger) => {
            tracing::info!(trigger, "unhandled webhook trigger");
        }
        None => {
            tracing::debug!("webhook with no trigger_id");
        }
    }

    (axum::http::StatusCode::OK, Json(ygg_ha::webhook::WebhookResponse::ok()))
}

// ─────────────────────────────────────────────────────────────────
// Flow CRUD (Sprint 059)
// ─────────────────────────────────────────────────────────────────
//
// Endpoints consumed by the VS Code extension Settings → Flows editor.
// Mutations are persisted to the backing config file via atomic
// tempfile-rename so they survive service restarts, and the in-memory
// `state.flows` snapshot is hot-swapped so the next request sees the
// new flow without a service restart.

/// `GET /api/flows` — list every configured flow.
pub async fn flows_list_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<Vec<ygg_domain::config::FlowConfig>> {
    let snapshot = state.flows.read().unwrap().clone();
    Json((*snapshot).clone())
}

/// `GET /api/flows/:id` — fetch a single flow by name.
pub async fn flow_get_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ygg_domain::config::FlowConfig>, (StatusCode, String)> {
    let snapshot = state.flows.read().unwrap().clone();
    snapshot
        .iter()
        .find(|f| f.name == id)
        .cloned()
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, format!("flow '{id}' not found")))
}

/// `PUT /api/flows/:id` — create or replace a flow. Validates each step's
/// backend against the live backend registry, persists the full config
/// atomically (tempfile + rename), then hot-swaps the in-memory flow list.
///
/// The on-disk config is patched as raw JSON so `${ENV_VAR}` placeholders
/// elsewhere in the file are preserved (we never round-trip the whole
/// `OdinConfig` through serde after env expansion).
pub async fn flow_save_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(flow): Json<ygg_domain::config::FlowConfig>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if flow.name != id {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("flow.name '{}' does not match path id '{}'", flow.name, id),
        ));
    }

    let backend_names: std::collections::HashSet<&str> =
        state.backends.iter().map(|b| b.name.as_str()).collect();
    for step in &flow.steps {
        if !backend_names.contains(step.backend.as_str()) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "step '{}' references unknown backend '{}'",
                    step.name, step.backend
                ),
            ));
        }
    }

    let mut new_flows: Vec<ygg_domain::config::FlowConfig> = {
        let snapshot = state.flows.read().unwrap().clone();
        (*snapshot).clone()
    };
    match new_flows.iter().position(|f| f.name == id) {
        Some(pos) => new_flows[pos] = flow.clone(),
        None => new_flows.push(flow.clone()),
    }

    persist_flows_patch(&state.config_path, &new_flows).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to persist config: {e}"),
        )
    })?;

    {
        let mut guard = state.flows.write().unwrap();
        *guard = std::sync::Arc::new(new_flows);
    }

    tracing::info!(flow = %id, "flow updated via CRUD");
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/backends` — list configured backends (name, url, type, models,
/// context window). Everything in `BackendConfig` is non-sensitive, so this
/// is a direct clone of `state.config.backends`.
pub async fn backends_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<Vec<ygg_domain::config::BackendConfig>> {
    Json(state.config.backends.clone())
}

/// Atomic write of the full config file with only the `flows` array replaced.
/// Parses existing JSON into `serde_json::Value`, swaps in the new `flows`
/// field (preserving every other field verbatim including `${ENV_VAR}`
/// placeholders), serialises pretty, writes to a sibling tempfile, then
/// renames into place. Rename on the same filesystem is atomic on Linux.
fn persist_flows_patch(
    path: &std::path::Path,
    flows: &[ygg_domain::config::FlowConfig],
) -> Result<(), String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let mut root: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    match root.as_object_mut() {
        Some(obj) => {
            obj.insert(
                "flows".to_string(),
                serde_json::to_value(flows).map_err(|e| format!("serialize flows: {e}"))?,
            );
        }
        None => return Err("root config is not a JSON object".into()),
    }
    let serialized =
        serde_json::to_string_pretty(&root).map_err(|e| format!("serialize config: {e}"))?;

    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("config path has no filename: {}", path.display()))?;
    let tmp_path = parent.join(format!(".{filename}.tmp.{}", std::process::id()));

    std::fs::write(&tmp_path, &serialized).map_err(|e| format!("tmp write: {e}"))?;
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("rename: {e}"));
    }
    Ok(())
}

#[cfg(test)]
mod flow_crud_tests {
    use super::*;
    use ygg_domain::config::{FlowConfig, FlowInput, FlowStep, FlowTrigger};

    fn sample_flow(name: &str) -> FlowConfig {
        FlowConfig {
            name: name.to_string(),
            trigger: FlowTrigger::Manual,
            steps: vec![FlowStep {
                name: "only".to_string(),
                backend: "mock".to_string(),
                model: "mock-model".to_string(),
                system_prompt: None,
                input: FlowInput::UserMessage,
                output_key: "out".to_string(),
                max_tokens: 256,
                temperature: 0.3,
                tools: None,
                think: None,
                agent_config: None,
                stream_role: None,
                stream_label: None,
                parallel_with: None,
                watches: None,
                sentinel: None,
                sentinel_skips: None,
                secrets: vec![],
            }],
            timeout_secs: 30,
            max_step_output_chars: 4000,
            loop_config: None,
            secrets: vec![],
        }
    }

    #[test]
    fn persist_patches_only_flows_field_preserving_env_placeholders() {
        let dir = std::env::temp_dir().join(format!("odin_flow_crud_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");

        // Minimal config with a ${VAR} placeholder elsewhere that MUST survive.
        let initial = r#"{
  "node_name": "test",
  "listen_addr": "${LISTEN_ADDR}",
  "backends": [],
  "flows": []
}"#;
        std::fs::write(&path, initial).unwrap();

        let flow = sample_flow("coding_swarm");
        persist_flows_patch(&path, std::slice::from_ref(&flow)).unwrap();

        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk["listen_addr"], "${LISTEN_ADDR}",
            "env placeholder must be preserved verbatim"
        );
        let flows = on_disk["flows"].as_array().unwrap();
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0]["name"], "coding_swarm");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persist_replaces_existing_flow_by_name() {
        let dir = std::env::temp_dir().join(format!("odin_flow_crud_replace_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"flows": []}"#).unwrap();

        let original = sample_flow("coding_swarm");
        persist_flows_patch(&path, std::slice::from_ref(&original)).unwrap();

        // Replace: same name, different step model.
        let mut updated = original.clone();
        updated.steps[0].model = "replaced-model".into();
        persist_flows_patch(&path, std::slice::from_ref(&updated)).unwrap();

        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let flows = on_disk["flows"].as_array().unwrap();
        assert_eq!(flows.len(), 1, "replace must not duplicate by-name");
        assert_eq!(flows[0]["steps"][0]["model"], "replaced-model");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persist_rejects_non_object_root() {
        let dir = std::env::temp_dir()
            .join(format!("odin_flow_crud_non_object_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"["not", "an", "object"]"#).unwrap();

        let err = persist_flows_patch(&path, &[sample_flow("x")]).unwrap_err();
        assert!(err.contains("not a JSON object"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
