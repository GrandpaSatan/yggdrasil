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

use ygg_domain::config::{BackendType, FlowConfig, FlowInput, FlowStep, FlowTrigger};

use crate::error::OdinError;
use crate::openai::{
    ChatCompletionRequest, ChatMessage, OllamaChatRequest, OllamaMessage, OllamaOptions, Role,
};
use crate::proxy;
use crate::state::BackendState;

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

    /// Execute a flow pipeline.
    pub async fn execute(
        &self,
        flow: &FlowConfig,
        user_message: &str,
    ) -> Result<FlowResult, OdinError> {
        let start = Instant::now();
        let mut outputs: HashMap<String, String> = HashMap::new();
        let mut step_timings = Vec::new();
        let mut final_key = String::new();

        let timeout = tokio::time::Duration::from_secs(flow.timeout_secs);

        for step in &flow.steps {
            let step_start = Instant::now();

            // Resolve input for this step
            let input_text = self.resolve_input(&step.input, user_message, &outputs)?;

            // Execute with flow-level timeout
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(OdinError::Upstream(format!(
                    "flow '{}' timed out before step '{}'",
                    flow.name, step.name
                )));
            }

            let result = tokio::time::timeout(remaining, self.call_step(step, &input_text))
                .await
                .map_err(|_| {
                    OdinError::Upstream(format!(
                        "flow '{}' timed out at step '{}' after {}s",
                        flow.name, step.name, flow.timeout_secs
                    ))
                })??;

            // Defensive truncation
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

            final_key = step.output_key.clone();
            outputs.insert(step.output_key.clone(), truncated);
        }

        Ok(FlowResult {
            outputs,
            final_key,
            elapsed_ms: start.elapsed().as_millis() as u64,
            step_timings,
        })
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
        }
    }

    /// Call a single flow step against its configured backend.
    async fn call_step(&self, step: &FlowStep, input: &str) -> Result<String, OdinError> {
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
                messages.push(OllamaMessage::new("user", input));

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
}
