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
use crate::flow_streaming::StreamEvent;
use crate::openai::{
    ChatCompletionRequest, ChatMessage, OllamaChatRequest, OllamaMessage, OllamaOptions, Role,
};
use crate::proxy::{self, TokenEvent};
use crate::router::RoutingDecision;
use crate::state::{AppState, BackendState};
use crate::tool_registry::{self, ToolTier};
use futures::StreamExt;
use std::collections::HashSet;
use tokio::sync::mpsc;

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
        // Sprint 064 P7: resolve flow + step vault secrets and rewrite the
        // flow with `{{secret:NAME}}` substitutions applied. When `state` is
        // None (e.g. background scheduler with no AppState plumbed through)
        // or the flow declares no secrets, this is a free no-op.
        let resolved_flow = self.with_substituted_secrets(flow, state).await?;
        let flow = &resolved_flow;
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

        // Sprint 064 P8: per-step + whole-flow duration histograms.
        for st in &step_timings {
            crate::metrics::record_flow_duration(
                &flow.name,
                &st.name,
                (st.elapsed_ms as f64) / 1000.0,
            );
        }
        let total_secs = start.elapsed().as_secs_f64();
        crate::metrics::record_flow_duration(&flow.name, "__total__", total_secs);

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
            keyword_match_count: 0,
            keyword_match_kind: crate::router::KeywordMatchKind::None,
            explicit_flow: None,
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
                    flow: None,
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

    /// Streaming variant of [`execute`] (Sprint 061).
    ///
    /// Emits `StreamEvent`s into `tx` as steps progress. Per-step `stream_role`
    /// determines whether a step's deltas route to the main assistant stream
    /// or to the `ygg_step` thinking channel. When a step's `stream_role` is
    /// `None`, the engine infers: the last step in the flow becomes
    /// `"assistant"`; all prior steps become `"swarm_thinking"`.
    ///
    /// Agentic steps (those with `tools`) still buffer to completion and emit
    /// a single `StepDelta` with the full output at step boundary — per-token
    /// streaming inside an agent loop is future work. Regular (non-tool) steps
    /// stream per token via `proxy::stream_tokens_ollama` / `stream_tokens_openai`.
    pub async fn execute_streaming(
        &self,
        flow: &FlowConfig,
        user_message: &str,
        images: Option<&[String]>,
        state: Option<&AppState>,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<FlowResult, OdinError> {
        // Sprint 064 P7: substitute vault secrets before the streaming engine
        // touches the prompts (matches the non-streaming `execute()` path).
        let resolved_flow = self.with_substituted_secrets(flow, state).await?;
        let flow = &resolved_flow;
        let start = Instant::now();
        let mut outputs: HashMap<String, String> = HashMap::new();
        let mut step_timings = Vec::new();
        let mut final_key = String::new();
        let timeout = tokio::time::Duration::from_secs(flow.timeout_secs);

        let convergence_re = flow.loop_config.as_ref().map(|lc| {
            regex::Regex::new(&lc.convergence_pattern).unwrap_or_else(|e| {
                tracing::warn!(pattern = %lc.convergence_pattern, error = %e, "invalid convergence regex, using fallback");
                regex::Regex::new("LGTM|CONVERGED").unwrap()
            })
        });

        let restart_idx = flow.loop_config.as_ref().map(|lc| {
            flow.steps.iter().position(|s| s.name == lc.restart_from_step).unwrap_or(0)
        });

        let terminal_step_name = flow
            .steps
            .last()
            .map(|s| s.name.clone())
            .unwrap_or_default();

        // Sprint 061: skip set grows via `sentinel` matches; steps named in
        // `sentinel_skips` are announced + closed without invoking the LLM.
        let mut skip_set: HashSet<String> = HashSet::new();

        for step in &flow.steps {
            if Self::run_or_skip(
                step,
                flow,
                user_message,
                images,
                &mut outputs,
                &mut step_timings,
                &mut final_key,
                &start,
                &timeout,
                state,
                &tx,
                &terminal_step_name,
                &mut skip_set,
                self,
            )
            .await?
            {
                // Step ran — check sentinel to possibly short-circuit downstream.
                Self::maybe_apply_sentinel(step, &outputs, &mut skip_set, &flow.name);
            }
        }

        if let (Some(lc), Some(re), Some(restart)) = (&flow.loop_config, &convergence_re, restart_idx) {
            let max_iter = lc.max_iterations;
            for iteration in 1..max_iter {
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

                for step in &flow.steps[restart..] {
                    if Self::run_or_skip(
                        step,
                        flow,
                        user_message,
                        images,
                        &mut outputs,
                        &mut step_timings,
                        &mut final_key,
                        &start,
                        &timeout,
                        state,
                        &tx,
                        &terminal_step_name,
                        &mut skip_set,
                        self,
                    )
                    .await?
                    {
                        Self::maybe_apply_sentinel(step, &outputs, &mut skip_set, &flow.name);
                    }
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;

        Ok(FlowResult {
            outputs,
            final_key,
            elapsed_ms: start.elapsed().as_millis() as u64,
            step_timings,
        })
    }

    /// Sentinel-aware wrapper around [`run_step_streaming`]. If `step.name`
    /// is in `skip_set`, emit a minimal start/end announcement and return
    /// `Ok(false)` (skipped). Otherwise run the step normally and return
    /// `Ok(true)` (ran). Callers should gate the post-step sentinel check
    /// on the returned `bool`.
    #[allow(clippy::too_many_arguments)]
    async fn run_or_skip(
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
        tx: &mpsc::Sender<StreamEvent>,
        terminal_step_name: &str,
        skip_set: &mut HashSet<String>,
        engine: &Self,
    ) -> Result<bool, OdinError> {
        if skip_set.contains(&step.name) {
            tracing::info!(
                flow = %flow.name,
                step = %step.name,
                "step skipped via sentinel short-circuit"
            );
            let _ = tx
                .send(StreamEvent::StepStart {
                    step: step.name.clone(),
                    label: "(skipped)".to_string(),
                    role: "swarm_thinking".to_string(),
                })
                .await;
            let _ = tx
                .send(StreamEvent::StepEnd {
                    step: step.name.clone(),
                })
                .await;
            return Ok(false);
        }

        engine
            .run_step_streaming(
                step,
                flow,
                user_message,
                images,
                outputs,
                step_timings,
                final_key,
                start,
                timeout,
                state,
                tx,
                terminal_step_name,
            )
            .await?;
        Ok(true)
    }

    /// If the step declares a `sentinel` regex and it matches the step's
    /// output, extend `skip_set` with `sentinel_skips`. Invalid patterns
    /// are logged and ignored — better to finish the flow than to error.
    fn maybe_apply_sentinel(
        step: &FlowStep,
        outputs: &HashMap<String, String>,
        skip_set: &mut HashSet<String>,
        flow_name: &str,
    ) {
        let Some(pattern) = &step.sentinel else {
            return;
        };
        let Some(output) = outputs.get(&step.output_key) else {
            return;
        };
        let re = match regex::Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    flow = %flow_name,
                    step = %step.name,
                    pattern = %pattern,
                    error = %e,
                    "invalid sentinel regex — ignoring"
                );
                return;
            }
        };
        if !re.is_match(output) {
            return;
        }
        let Some(skips) = &step.sentinel_skips else {
            return;
        };
        tracing::info!(
            flow = %flow_name,
            step = %step.name,
            pattern = %pattern,
            skips = ?skips,
            "sentinel matched — skipping downstream steps"
        );
        for s in skips {
            skip_set.insert(s.clone());
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_step_streaming(
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
        tx: &mpsc::Sender<StreamEvent>,
        terminal_step_name: &str,
    ) -> Result<(), OdinError> {
        let step_start = Instant::now();
        let input_text = self.resolve_input(&step.input, user_message, outputs)?;

        let step_images = match step.input {
            FlowInput::AudioInput | FlowInput::ImageInput | FlowInput::UserMessage => images,
            _ => None,
        };

        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            let msg = format!(
                "flow '{}' timed out before step '{}'",
                flow.name, step.name
            );
            let _ = tx
                .send(StreamEvent::Error {
                    step: Some(step.name.clone()),
                    message: msg.clone(),
                })
                .await;
            return Err(OdinError::Upstream(msg));
        }

        let role = step.stream_role.clone().unwrap_or_else(|| {
            if step.name == terminal_step_name {
                "assistant".to_string()
            } else {
                "swarm_thinking".to_string()
            }
        });
        let label = step
            .stream_label
            .clone()
            .unwrap_or_else(|| title_case_step_name(&step.name));

        let _ = tx
            .send(StreamEvent::StepStart {
                step: step.name.clone(),
                label: label.clone(),
                role: role.clone(),
            })
            .await;

        let result = if step.tools.is_some() && state.is_some() {
            tokio::time::timeout(
                remaining,
                self.call_step(step, &input_text, step_images, state),
            )
            .await
            .map_err(|_| {
                OdinError::Upstream(format!(
                    "flow '{}' timed out at step '{}' after {}s",
                    flow.name, step.name, flow.timeout_secs
                ))
            })??
        } else {
            match tokio::time::timeout(
                remaining,
                self.stream_step_tokens(step, &input_text, step_images, &role, tx),
            )
            .await
            {
                Ok(Ok(text)) => text,
                Ok(Err(e)) => {
                    let _ = tx
                        .send(StreamEvent::Error {
                            step: Some(step.name.clone()),
                            message: format!("{e}"),
                        })
                        .await;
                    return Err(e);
                }
                Err(_) => {
                    let msg = format!(
                        "flow '{}' timed out at step '{}' after {}s",
                        flow.name, step.name, flow.timeout_secs
                    );
                    let _ = tx
                        .send(StreamEvent::Error {
                            step: Some(step.name.clone()),
                            message: msg.clone(),
                        })
                        .await;
                    return Err(OdinError::Upstream(msg));
                }
            }
        };

        if step.tools.is_some() && state.is_some() && !result.is_empty() {
            let _ = tx
                .send(StreamEvent::StepDelta {
                    step: step.name.clone(),
                    role: role.clone(),
                    content: result.clone(),
                })
                .await;
        }

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
            role = %role,
            chars = truncated.len(),
            ms = step_start.elapsed().as_millis() as u64,
            "flow step complete (streaming)"
        );

        let _ = tx
            .send(StreamEvent::StepEnd {
                step: step.name.clone(),
            })
            .await;

        // Only track `assistant`-role steps as the final output. Intermediate
        // `swarm_thinking` steps (reviewers, critics, plans) produce
        // side-channel output routed to the thinking fold; when a sentinel
        // short-circuits the terminal refiner, the final answer must fall
        // back to the LAST assistant step that actually ran — typically the
        // drafter. (Sprint 061.)
        if role == "assistant" {
            *final_key = step.output_key.clone();
        }
        outputs.insert(step.output_key.clone(), truncated);
        Ok(())
    }

    async fn stream_step_tokens(
        &self,
        step: &FlowStep,
        input: &str,
        images: Option<&[String]>,
        role: &str,
        tx: &mpsc::Sender<StreamEvent>,
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

        match backend.backend_type {
            BackendType::Ollama => {
                let mut messages = Vec::new();
                if let Some(sys) = &step.system_prompt {
                    messages.push(OllamaMessage::new("system", sys.as_str()));
                }
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
                    stream: true,
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

                let mut stream = proxy::stream_tokens_ollama(
                    self.http_client.clone(),
                    backend.url.clone(),
                    request,
                )
                .await?;

                let mut final_text = String::new();
                while let Some(ev) = stream.next().await {
                    match ev? {
                        TokenEvent::Content(c) => {
                            let _ = tx
                                .send(StreamEvent::StepDelta {
                                    step: step.name.clone(),
                                    role: role.to_string(),
                                    content: c,
                                })
                                .await;
                        }
                        TokenEvent::Done(full) => {
                            final_text = full;
                            break;
                        }
                    }
                }
                Ok(final_text)
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
                    stream: true,
                    temperature: Some(step.temperature),
                    max_tokens: Some(step.max_tokens as u64),
                    top_p: None,
                    stop: None,
                    session_id: None,
                    project_id: None,
                    tools: None,
                    tool_choice: None,
                    flow: None,
                };

                let mut stream = proxy::stream_tokens_openai(
                    self.http_client.clone(),
                    backend.url.clone(),
                    request,
                )
                .await?;

                let mut final_text = String::new();
                while let Some(ev) = stream.next().await {
                    match ev? {
                        TokenEvent::Content(c) => {
                            let _ = tx
                                .send(StreamEvent::StepDelta {
                                    step: step.name.clone(),
                                    role: role.to_string(),
                                    content: c,
                                })
                                .await;
                        }
                        TokenEvent::Done(full) => {
                            final_text = full;
                            break;
                        }
                    }
                }
                Ok(final_text)
            }
        }
    }

    /// Sprint 064 P7 — resolve flow + step secrets from the Mimir vault and
    /// return a substituted clone of the flow with `{{secret:NAME}}` tokens
    /// replaced in `system_prompt` and any template-shaped inputs.
    ///
    /// Returns the input flow unchanged when:
    ///   - `state` is None (no AppState wired — typical for tests/scheduler)
    ///   - the flow declares no secrets (free no-op)
    ///   - any individual step also declares no secrets (handled per-step)
    async fn with_substituted_secrets(
        &self,
        flow: &FlowConfig,
        state: Option<&AppState>,
    ) -> Result<FlowConfig, OdinError> {
        let any_secrets =
            !flow.secrets.is_empty() || flow.steps.iter().any(|s| !s.secrets.is_empty());
        if !any_secrets {
            return Ok(flow.clone());
        }
        let Some(state) = state else {
            tracing::warn!(
                flow = %flow.name,
                "flow declares secrets but no AppState was passed; substitution skipped"
            );
            return Ok(flow.clone());
        };

        let token = std::env::var("MIMIR_VAULT_CLIENT_TOKEN").ok();
        let mut out = flow.clone();
        for step in out.steps.iter_mut() {
            let resolved = match crate::flow_secrets::resolve(
                &self.http_client,
                &state.mimir_url,
                token.as_deref(),
                &flow.secrets,
                &step.secrets,
            )
            .await
            {
                Ok(map) => map,
                Err(e) => {
                    tracing::warn!(
                        flow = %flow.name,
                        step = %step.name,
                        error = %e,
                        "flow secrets resolution failed; running step with literal tokens"
                    );
                    continue;
                }
            };
            if resolved.is_empty() {
                continue;
            }
            if let Some(ref sys) = step.system_prompt {
                step.system_prompt =
                    Some(crate::flow_secrets::substitute(sys, &resolved));
            }
            substitute_in_input(&mut step.input, &resolved);
        }
        Ok(out)
    }
}

/// Apply `{{secret:NAME}}` substitution to any string-shaped variant of
/// `FlowInput`. Non-string variants (StepOutput, UserMessage, etc.) are
/// passed through unchanged.
fn substitute_in_input(input: &mut FlowInput, secrets: &HashMap<String, String>) {
    if let FlowInput::Template { template } = input {
        *template = crate::flow_secrets::substitute(template, secrets);
    }
}

// ─────────────────────────────────────────────────────────────────
// Sprint 063 P1 — explicit /flow pinning gate
// ─────────────────────────────────────────────────────────────────

/// Outcome of validating a caller-supplied `flow` name against the flow
/// catalog. Returned by `classify_explicit_flow` so `handlers.rs` can
/// translate the verdict into an HTTP response without owning trigger logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplicitFlowVerdict {
    /// Flow exists and its trigger permits explicit user invocation —
    /// handler should dispatch directly, skipping intent classification.
    Dispatch,
    /// No flow with this name is loaded.
    NotFound,
    /// Flow exists but is `Cron { schedule }` — rejected because cron flows
    /// must not be user-invocable (they run on the scheduler only).
    CronOnly,
    /// Flow exists but its trigger is a system-only type (`Modality`,
    /// `Idle`). Not user-invocable via `/flow`.
    NotInvocable,
}

/// Sprint 063 P1: validate a caller-supplied flow name against the loaded
/// flow catalog and classify whether it may be invoked explicitly.
///
/// Pure function — no I/O, no state mutation — so it is fully unit-testable.
/// `handlers.rs` calls this first when `ChatCompletionRequest.flow` is set
/// and branches on the returned verdict.
pub fn classify_explicit_flow(flows: &[FlowConfig], name: &str) -> ExplicitFlowVerdict {
    let Some(flow) = flows.iter().find(|f| f.name == name) else {
        return ExplicitFlowVerdict::NotFound;
    };

    match &flow.trigger {
        FlowTrigger::Manual | FlowTrigger::Intent(_) => ExplicitFlowVerdict::Dispatch,
        FlowTrigger::Cron { .. } => ExplicitFlowVerdict::CronOnly,
        FlowTrigger::Modality(_) | FlowTrigger::Idle { .. } => ExplicitFlowVerdict::NotInvocable,
    }
}

fn title_case_step_name(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => format!("{}{}…", c.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
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

    fn step_with_sentinel(name: &str, pattern: &str, skips: Vec<String>) -> FlowStep {
        FlowStep {
            name: name.to_string(),
            backend: "mock".into(),
            model: "mock".into(),
            system_prompt: None,
            input: FlowInput::UserMessage,
            output_key: format!("{name}_out"),
            max_tokens: 256,
            temperature: 0.3,
            tools: None,
            think: None,
            agent_config: None,
            stream_role: None,
            stream_label: None,
            parallel_with: None,
            watches: None,
            sentinel: Some(pattern.to_string()),
            sentinel_skips: Some(skips),
            secrets: vec![],
        }
    }

    #[test]
    fn sentinel_match_extends_skip_set() {
        let step = step_with_sentinel("review", r"(?i)\bLGTM\b", vec!["refine".to_string()]);
        let mut outputs = HashMap::new();
        outputs.insert("review_out".to_string(), "Looks good. LGTM".to_string());
        let mut skip_set: HashSet<String> = HashSet::new();
        FlowEngine::maybe_apply_sentinel(&step, &outputs, &mut skip_set, "swarm_chat");
        assert!(skip_set.contains("refine"));
    }

    #[test]
    fn sentinel_no_match_leaves_skip_set_empty() {
        let step = step_with_sentinel("review", r"(?i)\bLGTM\b", vec!["refine".to_string()]);
        let mut outputs = HashMap::new();
        outputs.insert("review_out".to_string(), "Has issues; rewrite.".to_string());
        let mut skip_set: HashSet<String> = HashSet::new();
        FlowEngine::maybe_apply_sentinel(&step, &outputs, &mut skip_set, "swarm_chat");
        assert!(skip_set.is_empty());
    }

    #[test]
    fn sentinel_invalid_regex_is_ignored() {
        let step = step_with_sentinel("review", "[invalid(regex", vec!["refine".to_string()]);
        let mut outputs = HashMap::new();
        outputs.insert("review_out".to_string(), "anything".to_string());
        let mut skip_set: HashSet<String> = HashSet::new();
        // Must not panic.
        FlowEngine::maybe_apply_sentinel(&step, &outputs, &mut skip_set, "swarm_chat");
        assert!(skip_set.is_empty());
    }

    #[test]
    fn sentinel_missing_output_is_noop() {
        let step = step_with_sentinel("review", "LGTM", vec!["refine".to_string()]);
        let outputs: HashMap<String, String> = HashMap::new(); // no output recorded
        let mut skip_set: HashSet<String> = HashSet::new();
        FlowEngine::maybe_apply_sentinel(&step, &outputs, &mut skip_set, "swarm_chat");
        assert!(skip_set.is_empty());
    }

    #[test]
    fn sentinel_without_skips_is_noop() {
        let mut step = step_with_sentinel("review", "LGTM", vec![]);
        step.sentinel_skips = None;
        let mut outputs = HashMap::new();
        outputs.insert("review_out".to_string(), "LGTM".to_string());
        let mut skip_set: HashSet<String> = HashSet::new();
        FlowEngine::maybe_apply_sentinel(&step, &outputs, &mut skip_set, "swarm_chat");
        assert!(skip_set.is_empty());
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

    // ── Sprint 063 P1 — explicit /flow pinning gate ──────────────

    fn fixture_flow(name: &str, trigger: FlowTrigger) -> FlowConfig {
        FlowConfig {
            name: name.to_string(),
            trigger,
            steps: vec![],
            timeout_secs: 60,
            max_step_output_chars: 4096,
            loop_config: None,
            secrets: vec![],
        }
    }

    /// P1 test 1: a manual-trigger flow dispatches — this proves explicit
    /// invocation bypasses intent classification (the handler picks the
    /// `Dispatch` branch before the router is ever consulted).
    #[test]
    fn test_explicit_flow_bypasses_classification() {
        let flows = vec![fixture_flow(
            "dream_exploration",
            FlowTrigger::Manual,
        )];
        let verdict = classify_explicit_flow(&flows, "dream_exploration");
        assert_eq!(verdict, ExplicitFlowVerdict::Dispatch);
    }

    /// P1 test 2: an unknown flow name is rejected with `NotFound`, which
    /// `handlers.rs` translates into HTTP 400 `"flow not found: <name>"`.
    #[test]
    fn test_explicit_flow_unknown_returns_400() {
        let flows = vec![fixture_flow(
            "dream_exploration",
            FlowTrigger::Manual,
        )];
        let verdict = classify_explicit_flow(&flows, "no_such_flow");
        assert_eq!(verdict, ExplicitFlowVerdict::NotFound);
    }

    /// P1 test 3: a cron-triggered flow is rejected with `CronOnly`, which
    /// `handlers.rs` translates into HTTP 400 `"flow '<name>' is cron-only,
    /// not user-invocable"`. Cron flows must run from the scheduler only.
    #[test]
    fn test_explicit_flow_cron_only_rejected() {
        let flows = vec![fixture_flow(
            "dream_self_improvement",
            FlowTrigger::Cron { schedule: "0 3 * * *".to_string() },
        )];
        let verdict = classify_explicit_flow(&flows, "dream_self_improvement");
        assert_eq!(verdict, ExplicitFlowVerdict::CronOnly);
    }

    /// P1 test 4: a flow whose primary trigger is an intent can still be
    /// invoked explicitly — the user may pin it for a single turn even if
    /// it would normally auto-fire on that intent's keyword set.
    #[test]
    fn test_explicit_flow_intent_trigger_allowed() {
        let flows = vec![fixture_flow(
            "home_automation",
            FlowTrigger::Intent("home_automation".to_string()),
        )];
        let verdict = classify_explicit_flow(&flows, "home_automation");
        assert_eq!(verdict, ExplicitFlowVerdict::Dispatch);
    }

    /// P1 auxiliary: `Modality` and `Idle` triggers are system-only and
    /// must be rejected. Modality flows auto-route on images; Idle flows
    /// run from the scheduler. Neither is pinnable via `/flow`.
    #[test]
    fn test_explicit_flow_modality_and_idle_not_invocable() {
        let modality = vec![fixture_flow(
            "perceive",
            FlowTrigger::Modality("omni".to_string()),
        )];
        assert_eq!(
            classify_explicit_flow(&modality, "perceive"),
            ExplicitFlowVerdict::NotInvocable,
        );

        let idle = vec![fixture_flow(
            "dream_consolidate",
            FlowTrigger::Idle { min_idle_secs: 300 },
        )];
        assert_eq!(
            classify_explicit_flow(&idle, "dream_consolidate"),
            ExplicitFlowVerdict::NotInvocable,
        );
    }
}
