/// Autonomous background task worker.
///
/// Polls Mimir's task queue at a fixed interval, claims pending tasks via
/// atomic `pop`, interprets them through the agent loop (model from config +
/// tool definitions), and reports results back via `complete`.
///
/// Follows the `mimir::summarization::SummarizationService` background
/// daemon pattern: `tokio::select!` on interval + shutdown channel.
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::watch;

use ygg_domain::config::TaskWorkerConfig;

use crate::agent;
use crate::openai::{ChatMessage, Role, ToolDefinition};
use crate::router::RoutingDecision;
use crate::state::AppState;
use crate::tool_registry::{self, ToolTier};

/// Background task worker that polls and executes queued tasks.
pub struct TaskWorker {
    state: AppState,
    config: TaskWorkerConfig,
    shutdown_rx: watch::Receiver<bool>,
}

/// Task as returned by Mimir's `/api/v1/tasks/pop`.
#[derive(Debug, Deserialize)]
struct TaskPopResponse {
    task: Option<TaskEntry>,
}

#[derive(Debug, Deserialize)]
struct TaskEntry {
    id: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
}

impl TaskWorker {
    pub fn new(
        state: AppState,
        config: TaskWorkerConfig,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self { state, config, shutdown_rx }
    }

    /// Run the worker loop. Blocks until shutdown signal.
    pub async fn run(mut self) {
        let interval = Duration::from_secs(self.config.poll_interval_secs);
        tracing::info!(
            agent = %self.config.agent_name,
            model = %self.config.model,
            poll_secs = self.config.poll_interval_secs,
            "task worker started"
        );

        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    if let Err(e) = self.process_next().await {
                        tracing::warn!(error = %e, "task worker cycle failed");
                    }
                }
                result = self.shutdown_rx.changed() => {
                    if result.is_ok() && *self.shutdown_rx.borrow() {
                        tracing::info!("task worker shutting down");
                        break;
                    }
                }
            }
        }
    }

    /// Pop one task, process it through the agent loop, report result.
    async fn process_next(&self) -> Result<(), String> {
        // Pop next pending task from Mimir.
        let task = self.pop_task().await?;
        let Some(task) = task else {
            return Ok(()); // No tasks — sleep again.
        };

        tracing::info!(
            task_id = %task.id,
            title = %task.title,
            tags = ?task.tags,
            "task worker: processing task"
        );

        // Build a prompt from the task.
        let system = "You are Fergus, an autonomous task executor for the Yggdrasil home server. \
                       You have tools available. Execute the task described below by calling the \
                       appropriate tool(s). Be precise with tool arguments. \
                       After execution, summarize what you did in one sentence.";

        let user_prompt = if task.description.is_empty() {
            task.title.clone()
        } else {
            format!("{}\n\n{}", task.title, task.description)
        };

        let messages = vec![
            ChatMessage::new(Role::System, system),
            ChatMessage::new(Role::User, user_prompt),
        ];

        // Build tool definitions and routing decision for the configured model.
        let allowed_tiers = vec![ToolTier::Safe, ToolTier::Restricted];
        let tool_defs: Vec<ToolDefinition> = tool_registry::to_tool_definitions(
            &self.state.tool_registry,
            &allowed_tiers,
        );

        let agent_config = self.state.config.agent.clone().unwrap_or_default();

        // Resolve the backend URL for the configured model.
        let decision = self.state.router
            .resolve_backend_for_model(&self.config.model)
            .unwrap_or_else(|| {
                // Fallback: use first configured backend with the configured model.
                let b = self.state.backends.first();
                RoutingDecision {
                    intent: "task_worker".to_string(),
                    confidence: None,
                    router_method: crate::router::RouterMethod::Explicit,
                    model: self.config.model.clone(),
                    backend_url: b.map(|b| b.url.clone()).unwrap_or_default(),
                    backend_name: b.map(|b| b.name.clone()).unwrap_or_default(),
                    backend_type: b.map(|b| b.backend_type.clone())
                        .unwrap_or(ygg_domain::config::BackendType::Ollama),
                    keyword_match_count: 0,
                    keyword_match_kind: crate::router::KeywordMatchKind::None,
                    explicit_flow: None,
                }
            });

        let backend_context_window = self.state.backends
            .iter()
            .find(|b| b.name == decision.backend_name)
            .map(|b| b.context_window)
            .unwrap_or(16384);

        // Run the agent loop.
        let result = agent::run_agent_loop(
            &self.state,
            &messages,
            &tool_defs,
            &self.state.tool_registry,
            &allowed_tiers,
            &decision,
            &format!("task-{}", task.id),
            &agent_config,
            backend_context_window,
        )
        .await;

        // Report result back to Mimir.
        match result {
            Ok(resp) => {
                let response_text = resp.choices
                    .first()
                    .map(|c| c.message.content.clone())
                    .unwrap_or_else(|| "Task completed (no response text)".to_string());

                tracing::info!(
                    task_id = %task.id,
                    response = %response_text,
                    "task worker: task completed successfully"
                );
                self.complete_task(&task.id, true, &response_text).await?;
            }
            Err(e) => {
                let error_msg = format!("Agent loop failed: {e}");
                tracing::warn!(task_id = %task.id, error = %e, "task worker: task failed");
                self.complete_task(&task.id, false, &error_msg).await?;
            }
        }

        Ok(())
    }

    /// Pop next pending task from Mimir.
    async fn pop_task(&self) -> Result<Option<TaskEntry>, String> {
        let url = format!("{}/api/v1/tasks/pop", self.state.mimir_url);
        let mut body = serde_json::json!({ "agent": self.config.agent_name });
        if let Some(ref project) = self.config.project {
            body["project"] = serde_json::json!(project);
        }

        let resp = self.state.http_client
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("task pop request failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("task pop returned {}", resp.status()));
        }

        let pop_resp: TaskPopResponse = resp.json().await
            .map_err(|e| format!("task pop parse failed: {e}"))?;

        Ok(pop_resp.task)
    }

    /// Report task completion to Mimir.
    async fn complete_task(&self, task_id: &str, success: bool, result: &str) -> Result<(), String> {
        let url = format!("{}/api/v1/tasks/complete", self.state.mimir_url);
        let body = serde_json::json!({
            "task_id": task_id,
            "success": success,
            "result": result,
        });

        let resp = self.state.http_client
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("task complete request failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("task complete returned {}", resp.status()));
        }

        Ok(())
    }
}
