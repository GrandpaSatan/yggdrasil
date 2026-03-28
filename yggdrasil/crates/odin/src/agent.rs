/// Autonomous agent loop for local LLM tool-use.
///
/// When a `/v1/chat/completions` request includes a `tools` array and the
/// backend is Ollama, this module takes over.  It sends the prompt with tool
/// definitions, executes any tool calls the model makes, feeds results back,
/// and repeats until the model produces a text response or limits are hit.
///
/// This runs entirely on-premise — no internet required.
use std::time::Duration;

use futures::future::join_all;
use ygg_domain::config::AgentLoopConfig;

use crate::error::OdinError;
use crate::openai::{
    ChatCompletionResponse, ChatMessage, Choice, OllamaChatRequest, OllamaMessage, OllamaOptions,
    Role, ToolDefinition, Usage,
};
use crate::proxy;
use crate::router::RoutingDecision;
use crate::state::AppState;
use crate::tool_registry::{self, ToolSpec, ToolTier};

/// Run the agent loop: LLM → tool calls → execute → feed back → repeat.
///
/// Returns a standard `ChatCompletionResponse` with the model's final text
/// answer.  Tool call history is ephemeral — only the final response is
/// returned to the caller.
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop(
    state: &AppState,
    messages: &[ChatMessage],
    tool_defs: &[ToolDefinition],
    registry: &[ToolSpec],
    allowed_tiers: &[ToolTier],
    decision: &RoutingDecision,
    completion_id: &str,
    config: &AgentLoopConfig,
    backend_context_window: usize,
) -> Result<ChatCompletionResponse, OdinError> {
    // Convert input ChatMessages → OllamaMessages for the conversation.
    let mut conversation: Vec<OllamaMessage> = messages
        .iter()
        .map(|m| OllamaMessage::new(m.role.to_string(), &m.content))
        .collect();

    // Filter tool definitions to only those allowed by tier.
    let filtered_tools: Vec<ToolDefinition> = tool_defs
        .iter()
        .filter(|td| tool_registry::is_tool_allowed(registry, &td.function.name, allowed_tiers))
        .cloned()
        .collect();

    if filtered_tools.is_empty() {
        return Err(OdinError::BadRequest(
            "No tools passed the tier filter — nothing for the agent to call".to_string(),
        ));
    }

    tracing::info!(
        tools = filtered_tools.len(),
        max_iterations = config.max_iterations,
        "starting agent loop"
    );

    let tool_timeout = Duration::from_secs(config.tool_timeout_secs);
    let total_deadline = tokio::time::Instant::now() + Duration::from_secs(config.total_timeout_secs);
    let loop_start = std::time::Instant::now();
    let mut total_tool_calls: usize = 0;
    let mut accumulated_usage = AccumulatedUsage::default();

    for iteration in 0..config.max_iterations {
        // Check total timeout.
        if tokio::time::Instant::now() >= total_deadline {
            tracing::warn!("agent loop total timeout reached at iteration {iteration}");
            break;
        }

        crate::metrics::record_agent_iteration();

        // Build the Ollama request WITH tool definitions.
        let request = build_ollama_request(
            &decision.model,
            &conversation,
            Some(&filtered_tools),
            Some(backend_context_window as u64),
            config,
        );

        let resp = proxy::generate_chat_with_tools(
            &state.http_client,
            &decision.backend_url,
            request,
        )
        .await?;

        accumulated_usage.add(&resp.usage);

        // If the model produced NO tool calls → we have our final answer.
        let tool_calls = match resp.message.tool_calls {
            Some(ref tc) if !tc.is_empty() => tc,
            _ => {
                tracing::info!(
                    iteration,
                    total_tool_calls,
                    "agent loop complete — model produced text response"
                );
                crate::metrics::record_agent_loop_duration(loop_start.elapsed().as_secs_f64());
                return Ok(build_final_response(
                    completion_id,
                    &decision.model,
                    resp.message.content,
                    accumulated_usage,
                ));
            }
        };

        tracing::info!(
            iteration,
            num_tool_calls = tool_calls.len(),
            "model requested tool calls"
        );

        // Append the assistant message (preserving tool_calls) to the conversation.
        conversation.push(OllamaMessage {
            role: "assistant".to_string(),
            content: resp.message.content.clone(),
            tool_calls: resp.message.tool_calls.clone(),
        });

        // ── Pre-validate and dispatch tool calls in parallel ────────
        //
        // Phase 1: validate each call (lookup + tier check) and collect
        //          either an immediate error message or a ready-to-run future.
        // Phase 2: execute all valid calls concurrently via join_all.
        // Phase 3: append results to conversation in original order.

        let remaining_budget = config.max_tool_calls_total.saturating_sub(total_tool_calls);
        if remaining_budget == 0 {
            tracing::warn!("max total tool calls reached ({total_tool_calls})");
            conversation.push(OllamaMessage::new(
                "tool",
                "Error: maximum tool call limit reached. Please produce a final answer now.",
            ));
            continue;
        }

        // Validated entries: either an immediate error string or a (spec, args, timeout) tuple.
        enum Validated<'a> {
            Ready { spec: &'a ToolSpec, args: &'a serde_json::Value, timeout: Duration, call_id: String },
            Error(String),
        }

        let mut validated: Vec<Validated<'_>> = Vec::with_capacity(tool_calls.len());
        for (idx, tc) in tool_calls.iter().enumerate().take(remaining_budget) {
            let tool_name = &tc.function.name;
            let call_id = format!("call_{iteration}_{idx}");

            match tool_registry::find_tool(registry, tool_name) {
                Some(s) if allowed_tiers.contains(&s.tier) => {
                    let effective_timeout = s
                        .timeout_override_secs
                        .map(Duration::from_secs)
                        .unwrap_or(tool_timeout);
                    validated.push(Validated::Ready {
                        spec: s, args: &tc.function.arguments, timeout: effective_timeout, call_id,
                    });
                }
                Some(_) => {
                    tracing::warn!(tool = tool_name, "tool blocked by tier filter");
                    validated.push(Validated::Error(
                        format!("Error: tool '{tool_name}' is not allowed"),
                    ));
                }
                None => {
                    tracing::warn!(tool = tool_name, "tool not found in registry");
                    validated.push(Validated::Error(
                        format!("Error: unknown tool '{tool_name}'"),
                    ));
                }
            }
        }

        // Execute valid tool calls in parallel.
        let futures: Vec<_> = validated.iter().enumerate().map(|(idx, v)| {
            let tool_name = tool_calls.get(idx).map(|tc| tc.function.name.as_str()).unwrap_or("unknown");
            async move {
                match v {
                    Validated::Ready { spec, args, timeout, call_id } => {
                        let start = std::time::Instant::now();
                        let result = tokio::time::timeout(
                            *timeout,
                            tool_registry::execute_tool(state, spec, args, *timeout),
                        ).await;
                        let elapsed_ms = start.elapsed().as_millis();

                        match result {
                            Ok(Ok(output)) => {
                                crate::metrics::record_agent_tool_call(tool_name, "ok");
                                tracing::info!(
                                    tool = tool_name,
                                    call_id = call_id.as_str(),
                                    iteration,
                                    output_len = output.len(),
                                    duration_ms = elapsed_ms,
                                    "tool executed successfully"
                                );
                                truncate_tool_output(&output, config.tool_output_max_chars)
                            }
                            Ok(Err(e)) => {
                                crate::metrics::record_agent_tool_call(tool_name, "error");
                                tracing::warn!(
                                    tool = tool_name,
                                    call_id = call_id.as_str(),
                                    iteration,
                                    duration_ms = elapsed_ms,
                                    error = %e,
                                    "tool execution failed"
                                );
                                format!("Error: {e}")
                            }
                            Err(_) => {
                                crate::metrics::record_agent_tool_call(tool_name, "timeout");
                                tracing::warn!(
                                    tool = tool_name,
                                    call_id = call_id.as_str(),
                                    iteration,
                                    timeout_secs = timeout.as_secs(),
                                    "tool execution timed out"
                                );
                                format!("Error: tool '{tool_name}' timed out after {}s", timeout.as_secs())
                            }
                        }
                    }
                    Validated::Error(msg) => msg.clone(),
                }
            }
        }).collect();

        let results = join_all(futures).await;

        // Append results in original order to preserve conversation coherence.
        for result_text in results {
            conversation.push(OllamaMessage::new("tool", result_text));
            total_tool_calls += 1;
        }
    }

    // Max iterations exhausted — send one final request WITHOUT tools to force text.
    tracing::info!(
        total_tool_calls,
        "max iterations reached, forcing final text response"
    );

    let final_request = build_ollama_request(&decision.model, &conversation, None, Some(backend_context_window as u64), config);
    let final_resp = proxy::generate_chat_with_tools(
        &state.http_client,
        &decision.backend_url,
        final_request,
    )
    .await?;

    accumulated_usage.add(&final_resp.usage);
    crate::metrics::record_agent_loop_duration(loop_start.elapsed().as_secs_f64());

    Ok(build_final_response(
        completion_id,
        &decision.model,
        final_resp.message.content,
        accumulated_usage,
    ))
}

// ─────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────

fn build_ollama_request(
    model: &str,
    messages: &[OllamaMessage],
    tools: Option<&[ToolDefinition]>,
    num_ctx: Option<u64>,
    config: &AgentLoopConfig,
) -> OllamaChatRequest {
    OllamaChatRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        stream: false,
        options: Some(OllamaOptions {
            temperature: Some(config.temperature),
            num_predict: None,
            num_ctx,
            top_p: None,
            stop: None,
        }),
        think: Some(config.enable_thinking),
        tools: tools.map(|t| t.to_vec()),
    }
}

fn build_final_response(
    completion_id: &str,
    model: &str,
    content: String,
    usage: AccumulatedUsage,
) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: completion_id.to_string(),
        object: "chat.completion".to_string(),
        created: crate::proxy::unix_now(),
        model: model.to_string(),
        choices: vec![Choice {
            index: 0,
            message: ChatMessage::new(Role::Assistant, content),
            finish_reason: Some("stop".to_string()),
        }],
        usage: Some(Usage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.prompt_tokens + usage.completion_tokens,
        }),
    }
}

/// Truncate tool output to prevent context window overflow.
fn truncate_tool_output(output: &str, max_chars: usize) -> String {
    if output.len() <= max_chars {
        output.to_string()
    } else {
        let truncated = &output[..max_chars];
        format!("{truncated}\n\n... (output truncated, {total} chars total)", total = output.len())
    }
}

/// Accumulated token usage across all agent loop iterations.
#[derive(Default)]
struct AccumulatedUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

impl AccumulatedUsage {
    fn add(&mut self, usage: &Option<Usage>) {
        if let Some(u) = usage {
            self.prompt_tokens += u.prompt_tokens;
            self.completion_tokens += u.completion_tokens;
        }
    }
}
