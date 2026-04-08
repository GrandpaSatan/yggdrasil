/// Flow Engine — multi-model pipeline executor (Sprint 055).
///
/// Executes configurable pipelines where specialist models collaborate
/// sequentially, with output from one step feeding into the next.
///
/// Supports text, audio, and image inputs via `FlowInput` variants.
/// Each step dispatches to a backend via `proxy::generate_chat()` and
/// stores its output in a shared context for downstream steps.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use ygg_domain::config::{AgentLoopConfig, BackendType, FlowConfig, FlowInput, FlowStep, FlowTrigger};

use crate::error::OdinError;
use crate::openai::{
    ChatCompletionRequest, ChatMessage, OllamaChatRequest, OllamaMessage, OllamaOptions, Role,
};
use crate::proxy;
use crate::router::RoutingDecision;
use crate::state::{AppState, BackendState};
use crate::tool_registry::{self, ToolTier};

/// Result of a completed flow execution.
#[derive(Debug)]
pub struct FlowResult {
    /// All step outputs, keyed by output_key.
    pub outputs: HashMap<String, String>,
    /// Name of the last step (its output is the final response).
    pub final_key: String,
    /// Total execution time.
    pub elapsed_ms: u64,
    /// Step-level timing.
    pub step_timings: Vec<StepTiming>,
}

#[derive(Debug)]
pub struct StepTiming {
    pub name: String,
    pub model: String,
    pub elapsed_ms: u64,
    pub output_chars: usize,
}

impl FlowResult {
    /// Get the final output text (last step's output).
    pub fn final_output(&self) -> &str {
        self.outputs
            .get(&self.final_key)
            .map(|s| s.as_str())
            .unwrap_or("")
    }
}

/// Flow execution engine.
pub struct FlowEngine {
    http_client: reqwest::Client,
    backends: Arc<Vec<BackendState>>,
}

impl FlowEngine {
    pub fn new(http_client: reqwest::Client, backends: Arc<Vec<BackendState>>) -> Self {
        Self {
            http_client,
            backends,
        }
    }

    /// Find a flow matching the given intent.
    pub fn find_by_intent<'a>(
        &self,
        flows: &'a [FlowConfig],
        intent: &str,
    ) -> Option<&'a FlowConfig> {
        flows
            .iter()
            .find(|f| matches!(&f.trigger, FlowTrigger::Intent(i) if i == intent))
    }

    /// Find a flow matching the given modality.
    pub fn find_by_modality<'a>(
        &self,
        flows: &'a [FlowConfig],
        modality: &str,
    ) -> Option<&'a FlowConfig> {
        flows
            .iter()
            .find(|f| matches!(&f.trigger, FlowTrigger::Modality(m) if m == modality))
    }

    /// Find a flow by name (for manual trigger).
    pub fn find_by_name<'a>(
        &self,
        flows: &'a [FlowConfig],
        name: &str,
    ) -> Option<&'a FlowConfig> {
        flows.iter().find(|f| f.name == name)
    }

    /// Execute a flow pipeline, optionally with convergence looping.
    ///
    /// When `state` is `Some`, tool-enabled steps (those with `step.tools`) run
    /// a mini agent loop via `agent::run_agent_loop()`.  When `None` (tests),
    /// tool-enabled steps fall back to single-turn chat.
    ///
    /// `images` carries base64-encoded multimodal data (images or audio) for
    /// steps with `AudioInput` or `ImageInput` input types. Ollama uses the
    /// same `images` field for both modalities.
    pub async fn execute(
        &self,
        flow: &FlowConfig,
        user_message: &str,
        images: Option<&[String]>,
        state: Option<&AppState>,
    ) -> Result<FlowResult, OdinError> {
        let start = Instant::now();
        let mut outputs: HashMap<String, String> = HashMap::new();
        let mut step_timings = Vec::new();
        let mut final_key = String::new();
        let timeout = tokio::time::Duration::from_secs(flow.timeout_secs);

        // Build convergence regex if loop is configured
        let convergence_re = flow.loop_config.as_ref().map(|lc| {
            regex::Regex::new(&lc.convergence_pattern).unwrap_or_else(|e| {
                tracing::warn!(pattern = %lc.convergence_pattern, error = %e, "invalid convergence regex, using fallback");
                regex::Regex::new("LGTM|CONVERGED").unwrap()
            })
        });

        // Find the restart step index for looping
        let restart_idx = flow.loop_config.as_ref().map(|lc| {
            flow.steps.iter().position(|s| s.name == lc.restart_from_step).unwrap_or(0)
        });

        // Run all steps once
        for step in &flow.steps {
            self.run_step(step, flow, user_message, images, &mut outputs, &mut step_timings, &mut final_key, &start, &timeout, state).await?;
        }

        // If loop is configured, check convergence and repeat
        if let (Some(lc), Some(re), Some(restart)) = (&flow.loop_config, &convergence_re, restart_idx) {
            let max_iter = lc.max_iterations;
            for iteration in 1..max_iter {
                // Check convergence on the check_step's output
                let check_output = outputs.get(&lc.check_step).cloned().unwrap_or_default();
                if re.is_match(&check_output) {
                    tracing::info!(
                        flow = %flow.name,
                        iteration = iteration,
                        pattern = %lc.convergence_pattern,
                        "flow converged"
                    );
                    break;
                }

                tracing::info!(
                    flow = %flow.name,
                    iteration = iteration,
                    max = max_iter,
                    "loop iteration (not yet converged)"
                );

                // Re-run steps from restart_from_step onwards
                for step in &flow.steps[restart..] {
                    self.run_step(step, flow, user_message, images, &mut outputs, &mut step_timings, &mut final_key, &start, &timeout, state).await?;
                }
            }
        }

        Ok(FlowResult {
            outputs,
            final_key,
            elapsed_ms: start.elapsed().as_millis() as u64,
            step_timings,
        })
    }

    /// Execute a single step within a flow, updating outputs and timings.
    async fn run_step(
        &self,
        step: &FlowStep,
        flow: &FlowConfig,
        user_message: &str,
        images: Option<&[String]>,
        outputs: &mut HashMap<String, String>,
        step_timings: &mut Vec<StepTiming>,
        final_key: &mut String,
        start: &Instant,
        timeout: &tokio::time::Duration,
        state: Option<&AppState>,
    ) -> Result<(), OdinError> {
        let step_start = Instant::now();
        let input_text = self.resolve_input(&step.input, user_message, outputs)?;

        // Only pass multimodal data for steps that consume raw input
        let step_images = match step.input {
            FlowInput::AudioInput | FlowInput::ImageInput | FlowInput::UserMessage => images,
            _ => None,
        };

        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(OdinError::Upstream(format!(
                "flow '{}' timed out before step '{}'",
                flow.name, step.name
            )));
        }

        let result = tokio::time::timeout(remaining, self.call_step(step, &input_text, step_images, state))
            .await
            .map_err(|_| {
                OdinError::Upstream(format!(
                    "flow '{}' timed out at step '{}' after {}s",
                    flow.name, step.name, flow.timeout_secs
                ))
            })??;

        let truncated = truncate_preserving_ends(&result, flow.max_step_output_chars);

        step_timings.push(StepTiming {
            name: step.name.clone(),
            model: step.model.clone(),
            elapsed_ms: step_start.elapsed().as_millis() as u64,
            output_chars: truncated.len(),
        });

        tracing::info!(
            flow = %flow.name,
            step = %step.name,
            model = %step.model,
            chars = truncated.len(),
            ms = step_start.elapsed().as_millis() as u64,
            "flow step complete"
        );

        *final_key = step.output_key.clone();
        outputs.insert(step.output_key.clone(), truncated);
        Ok(())
    }

    /// Resolve input text for a step from the flow context.
    fn resolve_input(
        &self,
        input: &FlowInput,
        user_message: &str,
        outputs: &HashMap<String, String>,
    ) -> Result<String, OdinError> {
        match input {
            FlowInput::UserMessage | FlowInput::AudioInput | FlowInput::ImageInput => {
                Ok(user_message.to_string())
            }
            FlowInput::StepOutput { key } => outputs.get(key).cloned().ok_or_else(|| {
                OdinError::BadRequest(format!(
                    "flow step references unknown output key '{key}'"
                ))
            }),
            FlowInput::Template { template } => {
                let mut result = template.clone();
                result = result.replace("{user_message}", user_message);
                for (key, value) in outputs {
                    result = result.replace(&format!("{{{key}.output}}"), value);
                }
                Ok(result)
            }
            FlowInput::Accumulated { keys, separator } => {
                let mut parts = Vec::new();
                for key in keys {
                    if let Some(value) = outputs.get(key) {
                        parts.push(value.clone());
                    }
                }
                if parts.is_empty() {
                    return Err(OdinError::BadRequest(
                        "accumulated input: none of the referenced keys have outputs".to_string(),
                    ));
                }
                Ok(parts.join(separator))
            }
        }
    }

    /// Call a single flow step against its configured backend.
    ///
    /// When the step declares `tools` and `state` is available, this runs a
    /// mini agent loop (multi-turn tool calling) instead of single-turn chat.
    async fn call_step(
        &self,
        step: &FlowStep,
        input: &str,
        images: Option<&[String]>,
        state: Option<&AppState>,
    ) -> Result<String, OdinError> {
        // ── Agentic path: step has tools and we have AppState ──────
        if let (Some(tool_names), Some(app)) = (&step.tools, state) {
            return self.call_step_agentic(step, input, tool_names, app).await;
        }

        // ── Standard single-turn path ──────────────────────────────
        self.call_step_single(step, input, images).await
    }

    /// Agentic step: runs a mini agent loop with tool calling.
    async fn call_step_agentic(
        &self,
        step: &FlowStep,
        input: &str,
        tool_names: &[String],
        state: &AppState,
    ) -> Result<String, OdinError> {
        let backend = self
            .backends
            .iter()
            .find(|b| b.name == step.backend)
            .ok_or_else(|| {
                OdinError::BadRequest(format!(
                    "flow step '{}' references unknown backend '{}'",
                    step.name, step.backend
                ))
            })?;

        // Build tool definitions filtered by the step's tool list.
        let allowed_tiers = &[ToolTier::Safe, ToolTier::Restricted];
        let tool_defs = tool_registry::to_tool_definitions_filtered(
            &state.tool_registry,
            allowed_tiers,
            tool_names,
        );

        if tool_defs.is_empty() {
            tracing::warn!(
                step = %step.name,
                tools = ?tool_names,
                "agentic step: no matching tools found in registry, falling back to single-turn"
            );
            return self.call_step_single(step, input, None).await;
        }

        // Build messages for the agent loop.
        let mut messages = Vec::new();
        if let Some(sys) = &step.system_prompt {
            messages.push(ChatMessage::new(Role::System, sys.as_str()));
        }
        messages.push(ChatMessage::new(Role::User, input));

        // Construct a synthetic routing decision from the step config.
        let decision = RoutingDecision {
            intent: format!("flow_step:{}", step.name),
            confidence: Some(1.0),
            router_method: crate::router::RouterMethod::Keyword,
            model: step.model.clone(),
            backend_url: backend.url.clone(),
            backend_type: backend.backend_type.clone(),
            backend_name: backend.name.clone(),
        };

        // Use step's agent_config or sensible defaults for flow steps.
        let default_config = AgentLoopConfig {
            max_iterations: 5,
            max_tool_calls_total: 15,
            tool_timeout_secs: 30,
            total_timeout_secs: 120,
            default_tiers: vec!["safe".into(), "restricted".into()],
            temperature: step.temperature,
            tool_output_max_chars: 4000,
            enable_thinking: step.think.unwrap_or(false),
        };
        let config = step.agent_config.as_ref().unwrap_or(&default_config);

        let completion_id = format!("flow-agent-{}-{}", step.name, proxy::unix_now());
        let context_window = backend.context_window;

        tracing::info!(
            step = %step.name,
            tools = tool_defs.len(),
            max_iter = config.max_iterations,
            "starting agentic flow step"
        );

        let resp = crate::agent::run_agent_loop(
            state,
            &messages,
            &tool_defs,
            &state.tool_registry,
            allowed_tiers,
            &decision,
            &completion_id,
            config,
            context_window,
        )
        .await?;

        Ok(resp
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default())
    }

    /// Standard single-turn chat step (no tools).
    async fn call_step_single(&self, step: &FlowStep, input: &str, images: Option<&[String]>) -> Result<String, OdinError> {
        let backend = self
            .backends
            .iter()
            .find(|b| b.name == step.backend)
            .ok_or_else(|| {
                OdinError::BadRequest(format!(
                    "flow step '{}' references unknown backend '{}'",
                    step.name, step.backend
                ))
            })?;

        match backend.backend_type {
            BackendType::Ollama => {
                let mut messages = Vec::new();
                if let Some(sys) = &step.system_prompt {
                    messages.push(OllamaMessage::new("system", sys.as_str()));
                }
                // Attach multimodal data (images/audio) when available
                let user_msg = match images {
                    Some(imgs) if !imgs.is_empty() => {
                        OllamaMessage::with_images("user", input, imgs.to_vec())
                    }
                    _ => OllamaMessage::new("user", input),
                };
                messages.push(user_msg);

                let request = OllamaChatRequest {
                    model: step.model.clone(),
                    messages,
                    stream: false,
                    options: Some(OllamaOptions {
                        temperature: Some(step.temperature),
                        num_predict: Some(step.max_tokens as u64),
                        num_ctx: None,
                        top_p: None,
                        stop: None,
                    }),
                    think: if step.think == Some(false) {
                        Some(false)
                    } else {
                        None
                    },
                    tools: None,
                };

                let completion_id = format!("flow-{}-{}", step.name, proxy::unix_now());
                let resp = proxy::generate_chat(
                    &self.http_client,
                    &backend.url,
                    request,
                    &completion_id,
                )
                .await?;

                Ok(resp
                    .choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_default())
            }
            BackendType::Openai => {
                let mut messages = Vec::new();
                if let Some(sys) = &step.system_prompt {
                    messages.push(ChatMessage::new(Role::System, sys.as_str()));
                }
                messages.push(ChatMessage::new(Role::User, input));

                let request = ChatCompletionRequest {
                    model: Some(step.model.clone()),
                    messages,
                    stream: false,
                    temperature: Some(step.temperature),
                    max_tokens: Some(step.max_tokens as u64),
                    top_p: None,
                    stop: None,
                    session_id: None,
                    project_id: None,
                    tools: None,
                    tool_choice: None,
                };

                let resp =
                    proxy::generate_chat_openai(&self.http_client, &backend.url, request).await?;

                Ok(resp
                    .choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_default())
            }
        }
    }
}

/// Truncate text to max_chars, preserving head and tail with a middle marker.
fn truncate_preserving_ends(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(50) / 2;
    let head = &text[..keep];
    let tail = &text[text.len() - keep..];
    format!(
        "{head}\n\n[...truncated {}/{} chars...]\n\n{tail}",
        text.len() - max_chars,
        text.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short() {
        let text = "hello world";
        assert_eq!(truncate_preserving_ends(text, 100), text);
    }

    #[test]
    fn test_truncate_long() {
        let text = "a".repeat(200);
        let result = truncate_preserving_ends(&text, 100);
        assert!(result.len() <= 110);
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_resolve_template() {
        let engine = FlowEngine {
            http_client: reqwest::Client::new(),
            backends: Arc::new(vec![]),
        };
        let mut outputs = HashMap::new();
        outputs.insert(
            "generate".to_string(),
            "fn add(a: i32, b: i32) -> i32 { a + b }".to_string(),
        );

        let input = FlowInput::Template {
            template: "Review this code:\n{generate.output}\n\nUser asked: {user_message}"
                .to_string(),
        };

        let result = engine
            .resolve_input(&input, "write an add function", &outputs)
            .unwrap();
        assert!(result.contains("fn add"));
        assert!(result.contains("write an add function"));
    }

    #[test]
    fn test_resolve_accumulated() {
        let engine = FlowEngine {
            http_client: reqwest::Client::new(),
            backends: Arc::new(vec![]),
        };
        let mut outputs = HashMap::new();
        outputs.insert("search_internal".to_string(), "Found 3 results from memory".to_string());
        outputs.insert("search_external".to_string(), "Found 2 web results".to_string());

        let input = FlowInput::Accumulated {
            keys: vec!["search_internal".to_string(), "search_external".to_string()],
            separator: "\n---\n".to_string(),
        };

        let result = engine.resolve_input(&input, "query", &outputs).unwrap();
        assert!(result.contains("Found 3 results from memory"));
        assert!(result.contains("Found 2 web results"));
        assert!(result.contains("---"));
    }

    #[test]
    fn test_resolve_accumulated_empty_keys_errors() {
        let engine = FlowEngine {
            http_client: reqwest::Client::new(),
            backends: Arc::new(vec![]),
        };
        let outputs = HashMap::new();

        let input = FlowInput::Accumulated {
            keys: vec!["nonexistent".to_string()],
            separator: "\n".to_string(),
        };

        assert!(engine.resolve_input(&input, "query", &outputs).is_err());
    }
}
