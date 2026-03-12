//! Yggdrasil Agent — autonomous code agent with Gitea integration.
//!
//! Pops tasks from Odin's task queue, clones repos from Gitea, makes changes
//! using local LLM, runs tests, and pushes results.
//!
//! Usage:
//!   ygg-agent [--config <path>]

mod gitea;
mod runner;

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

/// Agent configuration.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentConfig {
    /// Odin URL for task queue and LLM code generation.
    pub odin_url: String,
    /// Gitea instance URL (e.g. "http://REDACTED_MUNIN_IP:3000").
    pub gitea_url: String,
    /// Gitea API token.
    pub gitea_token: String,
    /// Working directory for cloning repos.
    #[serde(default = "default_work_dir")]
    pub work_dir: String,
    /// Poll interval for new tasks (seconds).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Agent identity used when popping tasks from the queue.
    #[serde(default = "default_queue_name")]
    pub queue_name: String,
    /// Optional project filter for task queue.
    #[serde(default)]
    pub project: Option<String>,
    /// Allowed repositories (glob patterns).
    #[serde(default)]
    pub allowed_repos: Vec<String>,
    /// Allowed branches (glob patterns).
    #[serde(default)]
    pub allowed_branches: Vec<String>,
}

fn default_work_dir() -> String {
    "/tmp/ygg-agent".to_string()
}

fn default_poll_interval() -> u64 {
    10
}

fn default_queue_name() -> String {
    "code-agent".to_string()
}

#[derive(Debug, Parser)]
#[command(name = "ygg-agent", version, about = "Yggdrasil autonomous code agent")]
struct Args {
    #[arg(
        short,
        long,
        default_value = "configs/agent/config.json",
        env = "YGG_AGENT_CONFIG"
    )]
    config: std::path::PathBuf,
}

// ── Task queue API types (mirrors Odin/Mimir wire format) ─────────────

#[derive(Debug, serde::Serialize)]
struct TaskPopRequest {
    agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct TaskPopResponse {
    task: Option<TaskResponse>,
}

#[derive(Debug, serde::Deserialize)]
struct TaskResponse {
    id: String,
    title: String,
    description: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct TaskCompleteRequest {
    task_id: String,
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct TaskCancelRequest {
    task_id: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    info!(config = %args.config.display(), "loading agent configuration");

    let config: AgentConfig = ygg_config::load_json(&args.config)
        .with_context(|| format!("failed to load config: {}", args.config.display()))?;

    info!(
        odin_url = %config.odin_url,
        gitea_url = %config.gitea_url,
        work_dir = %config.work_dir,
        queue_name = %config.queue_name,
        poll_interval_secs = config.poll_interval_secs,
        "ygg-agent starting"
    );

    // Ensure work directory exists.
    std::fs::create_dir_all(&config.work_dir)
        .with_context(|| format!("failed to create work dir: {}", config.work_dir))?;

    let poll_interval = Duration::from_secs(config.poll_interval_secs);
    let odin_url = config.odin_url.trim_end_matches('/').to_string();
    let queue_name = config.queue_name.clone();
    let project = config.project.clone();

    let gitea = gitea::GiteaClient::new(config.gitea_url.clone(), config.gitea_token.clone());
    let runner = runner::TaskRunner::new(config, gitea);

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")?;

    info!("ygg-agent: entering task loop");

    // ── Main loop — poll, process, report ────────────────────────────
    let mut interval = tokio::time::interval(poll_interval);
    // The first tick completes immediately; consume it so we start with a
    // proper delay to let services come up.
    interval.tick().await;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("ygg-agent received shutdown signal");
                break;
            }
            _ = interval.tick() => {
                match pop_task(&http, &odin_url, &queue_name, project.as_deref()).await {
                    Ok(Some(task_resp)) => {
                        info!(
                            task_id = %task_resp.id,
                            title = %task_resp.title,
                            "claimed task from queue"
                        );

                        // Deserialize the task description into a CodeTask.
                        let code_task = runner::CodeTask {
                            id: task_resp.id.clone(),
                            repo: find_tag_value(&task_resp.tags, "repo")
                                .unwrap_or_default(),
                            branch: find_tag_value(&task_resp.tags, "branch")
                                .unwrap_or_else(|| "main".to_string()),
                            description: task_resp.description.clone(),
                            files: vec![],
                        };

                        match runner.process_task(&code_task).await {
                            Ok(summary) => {
                                info!(task_id = %task_resp.id, summary = %summary, "task completed successfully");
                                if let Err(e) = complete_task(
                                    &http, &odin_url, &task_resp.id, true, Some(summary),
                                ).await {
                                    error!(
                                        task_id = %task_resp.id,
                                        error = %e,
                                        "failed to report task completion"
                                    );
                                }
                            }
                            Err(e) => {
                                error!(
                                    task_id = %task_resp.id,
                                    error = %e,
                                    "task processing failed"
                                );
                                if let Err(cancel_err) = cancel_task(
                                    &http, &odin_url, &task_resp.id,
                                ).await {
                                    error!(
                                        task_id = %task_resp.id,
                                        error = %cancel_err,
                                        "failed to cancel task after failure"
                                    );
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        // No tasks available — normal idle state.
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to pop task from queue");
                    }
                }
            }
        }
    }

    info!("ygg-agent shut down");
    Ok(())
}

// ── Task queue HTTP helpers ──────────────────────────────────────────

/// Pop the next pending task from Odin's task queue.
async fn pop_task(
    client: &reqwest::Client,
    odin_url: &str,
    agent: &str,
    project: Option<&str>,
) -> Result<Option<TaskResponse>> {
    let url = format!("{}/api/v1/tasks/pop", odin_url);

    let resp = client
        .post(&url)
        .json(&TaskPopRequest {
            agent: agent.to_string(),
            project: project.map(|s| s.to_string()),
        })
        .send()
        .await
        .context("task pop request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("task pop returned HTTP {}: {}", status, body);
    }

    let pop_resp: TaskPopResponse = resp.json().await.context("failed to parse pop response")?;
    Ok(pop_resp.task)
}

/// Report a task as completed to Odin.
async fn complete_task(
    client: &reqwest::Client,
    odin_url: &str,
    task_id: &str,
    success: bool,
    result: Option<String>,
) -> Result<()> {
    let url = format!("{}/api/v1/tasks/complete", odin_url);

    let resp = client
        .post(&url)
        .json(&TaskCompleteRequest {
            task_id: task_id.to_string(),
            success,
            result,
        })
        .send()
        .await
        .context("task complete request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("task complete returned HTTP {}: {}", status, body);
    }

    Ok(())
}

/// Cancel a task in Odin's queue (used on processing failure).
async fn cancel_task(
    client: &reqwest::Client,
    odin_url: &str,
    task_id: &str,
) -> Result<()> {
    let url = format!("{}/api/v1/tasks/cancel", odin_url);

    let resp = client
        .post(&url)
        .json(&TaskCancelRequest {
            task_id: task_id.to_string(),
        })
        .send()
        .await
        .context("task cancel request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("task cancel returned HTTP {}: {}", status, body);
    }

    Ok(())
}

/// Extract a value from tags formatted as "key:value".
fn find_tag_value(tags: &[String], key: &str) -> Option<String> {
    let prefix = format!("{}:", key);
    tags.iter()
        .find(|t| t.starts_with(&prefix))
        .map(|t| t[prefix.len()..].to_string())
}
