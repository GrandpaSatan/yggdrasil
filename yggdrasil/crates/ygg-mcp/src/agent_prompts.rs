//! Agent prompt loader — reads TOML configs from disk on each call.
//!
//! Prompt files live in `{workspace}/configs/agent-prompts/{agent_type}.toml`.
//! Re-reading on every call enables prompt iteration without server restart.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Parsed agent prompt configuration.
#[derive(Debug, Deserialize)]
pub struct AgentPromptConfig {
    pub prompt: PromptSection,
    #[serde(default)]
    pub budget: BudgetSection,
}

#[derive(Debug, Deserialize)]
pub struct PromptSection {
    pub system: String,
    #[serde(default = "default_response_format")]
    pub response_format: String,
    #[serde(default)]
    pub constraints: Vec<String>,
}

fn default_response_format() -> String {
    "markdown".to_string()
}

#[derive(Debug, Deserialize)]
pub struct BudgetSection {
    #[serde(default = "default_memory_tokens")]
    pub max_memory_tokens: usize,
    #[serde(default = "default_code_tokens")]
    pub max_code_tokens: usize,
    #[serde(default = "default_file_tokens")]
    pub max_file_tokens: usize,
    #[serde(default = "default_reserve")]
    pub generation_reserve: usize,
}

impl Default for BudgetSection {
    fn default() -> Self {
        Self {
            max_memory_tokens: default_memory_tokens(),
            max_code_tokens: default_code_tokens(),
            max_file_tokens: default_file_tokens(),
            generation_reserve: default_reserve(),
        }
    }
}

fn default_memory_tokens() -> usize { 4096 }
fn default_code_tokens() -> usize { 8192 }
fn default_file_tokens() -> usize { 8192 }
fn default_reserve() -> usize { 6144 }

/// Load an agent prompt config from disk. Falls back to a built-in default
/// if the file doesn't exist or fails to parse.
pub fn load_prompt(workspace_path: &Path, agent_type: &str) -> AgentPromptConfig {
    let path = prompt_path(workspace_path, agent_type);
    match std::fs::read_to_string(&path) {
        Ok(content) => match toml::from_str::<AgentPromptConfig>(&content) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to parse agent prompt, using default"
                );
                default_prompt(agent_type)
            }
        },
        Err(_) => default_prompt(agent_type),
    }
}

fn prompt_path(workspace_path: &Path, agent_type: &str) -> PathBuf {
    workspace_path
        .join("configs")
        .join("agent-prompts")
        .join(format!("{}.toml", agent_type))
}

/// Built-in defaults for each agent type.
fn default_prompt(agent_type: &str) -> AgentPromptConfig {
    let system = match agent_type {
        "executor" => "You are a code implementation agent. Follow existing patterns exactly. \
            Output file changes as markdown code blocks with the file path as the language tag. \
            Example:\n```path/to/file.rs\nfn main() {}\n```\n\
            Output ONLY code blocks. No explanations unless asked.",
        "docs" => "You are a documentation writer. Write clear, concise technical documentation \
            in markdown. Verify all code references are accurate. Include usage examples.",
        "review" => "You are a code reviewer. Analyze the provided code for bugs, security issues, \
            architecture violations, and performance problems. Be specific and actionable.",
        "qa" => "You are a test engineer. Write comprehensive tests covering happy paths, \
            edge cases, error conditions, and boundary values. Use the project's existing \
            test patterns and assertion style.",
        _ => "You are a helpful coding assistant. Be concise and direct.",
    };

    AgentPromptConfig {
        prompt: PromptSection {
            system: system.to_string(),
            response_format: if agent_type == "executor" {
                "code_blocks".to_string()
            } else {
                "markdown".to_string()
            },
            constraints: Vec::new(),
        },
        budget: BudgetSection::default(),
    }
}

/// Estimate token count from text length (chars / 4).
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Truncate text to fit within a token budget (estimated).
pub fn truncate_to_budget(text: &str, max_tokens: usize) -> &str {
    let max_chars = max_tokens * 4;
    if text.len() <= max_chars {
        text
    } else {
        // Find a safe UTF-8 boundary
        let mut end = max_chars;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }
}
