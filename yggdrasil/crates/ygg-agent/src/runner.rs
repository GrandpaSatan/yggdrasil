use std::process::Command;

use tracing::info;

use crate::gitea::GiteaClient;
use crate::AgentConfig;

/// Task runner — processes code tasks from the queue.
pub struct TaskRunner {
    config: AgentConfig,
    gitea: GiteaClient,
    client: reqwest::Client,
}

/// A code task to be processed.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CodeTask {
    pub id: String,
    pub repo: String,
    pub branch: String,
    pub description: String,
    #[serde(default)]
    pub files: Vec<String>,
}

impl TaskRunner {
    pub fn new(config: AgentConfig, gitea: GiteaClient) -> Self {
        Self {
            config,
            gitea,
            client: reqwest::Client::new(),
        }
    }

    /// Validate that the repo and branch are allowed by the gate policy.
    fn check_gate(&self, repo: &str, branch: &str) -> Result<(), RunnerError> {
        if !self.config.allowed_repos.is_empty() {
            let allowed = self
                .config
                .allowed_repos
                .iter()
                .any(|pattern| glob_match(pattern, repo));
            if !allowed {
                return Err(RunnerError::GateViolation(format!(
                    "repo '{}' not in allowed list",
                    repo
                )));
            }
        }
        if !self.config.allowed_branches.is_empty() {
            let allowed = self
                .config
                .allowed_branches
                .iter()
                .any(|pattern| glob_match(pattern, branch));
            if !allowed {
                return Err(RunnerError::GateViolation(format!(
                    "branch '{}' not in allowed list",
                    branch
                )));
            }
        }
        Ok(())
    }

    /// Process a single code task. Returns a summary string on success.
    pub async fn process_task(&self, task: &CodeTask) -> Result<String, RunnerError> {
        info!(task_id = %task.id, repo = %task.repo, "processing code task");

        // Gate check — reject unauthorized repos/branches
        self.check_gate(&task.repo, &task.branch)?;

        // 1. Clone the repo
        let work_path = format!("{}/{}", self.config.work_dir, task.id);
        self.git_clone(&task.repo, &task.branch, &work_path)?;

        // 2. Generate changes via Odin
        let changes = self.generate_changes(task).await?;
        let changes_count = changes.len();

        // 3. Apply changes
        for (file, content) in &changes {
            let file_path = format!("{}/{}", work_path, file);
            if let Some(parent) = std::path::Path::new(&file_path).parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| RunnerError::Io(e.to_string()))?;
            }
            std::fs::write(&file_path, content)
                .map_err(|e| RunnerError::Io(e.to_string()))?;
        }

        // 4. Run tests
        self.run_tests(&work_path)?;

        // 5. Commit and push
        self.git_commit_push(&work_path, &task.description)?;

        let summary = format!("Modified {} files, tests passed", changes_count);
        info!(task_id = %task.id, summary = %summary, "task completed successfully");
        Ok(summary)
    }

    fn git_clone(&self, repo: &str, branch: &str, path: &str) -> Result<(), RunnerError> {
        let output = Command::new("git")
            .args(["clone", "--branch", branch, "--depth", "1", repo, path])
            .output()
            .map_err(|e| RunnerError::Git(e.to_string()))?;

        if !output.status.success() {
            return Err(RunnerError::Git(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(())
    }

    async fn generate_changes(
        &self,
        task: &CodeTask,
    ) -> Result<Vec<(String, String)>, RunnerError> {
        let payload = serde_json::json!({
            "model": "default",
            "messages": [
                {
                    "role": "system",
                    "content": "You are a code generation assistant. Output file changes as markdown code blocks with the file path as the language tag. Example:\n```path/to/file.rs\nfn main() {}\n```"
                },
                {
                    "role": "user",
                    "content": task.description
                }
            ]
        });

        let resp = self
            .client
            .post(format!("{}/api/v1/chat", self.config.odin_url))
            .json(&payload)
            .send()
            .await
            .map_err(|e| RunnerError::Odin(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RunnerError::Odin(body));
        }

        // Parse the response to extract file path/content pairs.
        // Expected format from LLM:
        // ```path/to/file.rs
        // <content>
        // ```
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| RunnerError::Odin(e.to_string()))?;

        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");

        Ok(parse_file_blocks(content))
    }

    fn run_tests(&self, work_path: &str) -> Result<(), RunnerError> {
        // Detect project type and run appropriate tests
        let cargo_path = format!("{}/Cargo.toml", work_path);
        if std::path::Path::new(&cargo_path).exists() {
            let output = Command::new("cargo")
                .args(["test", "--workspace"])
                .current_dir(work_path)
                .output()
                .map_err(|e| RunnerError::Test(e.to_string()))?;

            if !output.status.success() {
                return Err(RunnerError::Test(
                    String::from_utf8_lossy(&output.stderr).to_string(),
                ));
            }
        }
        Ok(())
    }

    fn git_commit_push(&self, work_path: &str, message: &str) -> Result<(), RunnerError> {
        let commands = [
            vec!["add", "-A"],
            vec!["commit", "-m", message],
            vec!["push"],
        ];

        for args in &commands {
            let output = Command::new("git")
                .args(args)
                .current_dir(work_path)
                .output()
                .map_err(|e| RunnerError::Git(e.to_string()))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // git commit with nothing to commit is ok
                if stderr.contains("nothing to commit") {
                    continue;
                }
                return Err(RunnerError::Git(stderr.to_string()));
            }
        }
        Ok(())
    }
}

/// Parse markdown code blocks with file paths into (path, content) pairs.
///
/// Expected format:
/// ```path/to/file.rs
/// fn main() {}
/// ```
pub fn parse_file_blocks(content: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if let Some(path) = trimmed.strip_prefix("```") {
            let path = path.trim();
            // Skip blocks with no file path or generic language tags
            if path.is_empty() || !path.contains('.') {
                // Consume until closing ```
                for inner in lines.by_ref() {
                    if inner.trim() == "```" {
                        break;
                    }
                }
                continue;
            }

            let file_path = path.to_string();
            let mut block_content = String::new();

            for inner in lines.by_ref() {
                if inner.trim() == "```" {
                    break;
                }
                if !block_content.is_empty() {
                    block_content.push('\n');
                }
                block_content.push_str(inner);
            }

            if !file_path.is_empty() && !block_content.is_empty() {
                results.push((file_path, block_content));
            }
        }
    }

    results
}

/// Simple glob matching supporting `*` wildcards.
/// `*` matches any sequence of non-separator characters.
fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    // Split pattern by '*' and match segments in order
    let parts: Vec<&str> = pattern.split('*').collect();

    if parts.len() == 1 {
        // No wildcards — exact match
        return pattern == value;
    }

    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if let Some(found) = value[pos..].find(part) {
            if i == 0 && found != 0 {
                // First segment must match at the start
                return false;
            }
            pos += found + part.len();
        } else {
            return false;
        }
    }

    // If pattern doesn't end with '*', value must end at the right place
    if let Some(last) = parts.last() {
        if !last.is_empty() {
            return value.ends_with(last);
        }
    }

    true
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("git error: {0}")]
    Git(String),

    #[error("test failure: {0}")]
    Test(String),

    #[error("Odin error: {0}")]
    Odin(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("gate violation: {0}")]
    GateViolation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_file_blocks() {
        let content = r#"Here are the changes:

```src/main.rs
fn main() {
    println!("hello");
}
```

Some explanation text.

```src/lib.rs
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
```
"#;

        let blocks = parse_file_blocks(content);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "src/main.rs");
        assert!(blocks[0].1.contains("println"));
        assert_eq!(blocks[1].0, "src/lib.rs");
        assert!(blocks[1].1.contains("pub fn add"));
    }

    #[test]
    fn test_parse_file_blocks_skips_generic_language() {
        let content = r#"```rust
fn example() {}
```

```src/real.rs
fn real() {}
```
"#;

        let blocks = parse_file_blocks(content);
        // "rust" has no dot, so it's treated as a language tag and skipped
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, "src/real.rs");
    }

    #[test]
    fn test_gate_allows_matching_repo() {
        let config = AgentConfig {
            odin_url: String::new(),
            gitea_url: String::new(),
            gitea_token: String::new(),
            work_dir: "/tmp".to_string(),
            poll_interval_secs: 10,
            queue_name: "test".to_string(),
            project: None,
            allowed_repos: vec!["myorg/*".to_string()],
            allowed_branches: vec![],
        };
        let gitea = crate::gitea::GiteaClient::new(String::new(), String::new());
        let runner = TaskRunner::new(config, gitea);

        assert!(runner.check_gate("myorg/myrepo", "main").is_ok());
    }

    #[test]
    fn test_gate_blocks_unauthorized_repo() {
        let config = AgentConfig {
            odin_url: String::new(),
            gitea_url: String::new(),
            gitea_token: String::new(),
            work_dir: "/tmp".to_string(),
            poll_interval_secs: 10,
            queue_name: "test".to_string(),
            project: None,
            allowed_repos: vec!["myorg/*".to_string()],
            allowed_branches: vec![],
        };
        let gitea = crate::gitea::GiteaClient::new(String::new(), String::new());
        let runner = TaskRunner::new(config, gitea);

        let result = runner.check_gate("evilorg/hack", "main");
        assert!(result.is_err());
        match result.unwrap_err() {
            RunnerError::GateViolation(msg) => {
                assert!(msg.contains("evilorg/hack"));
            }
            other => panic!("expected GateViolation, got: {:?}", other),
        }
    }

    #[test]
    fn test_gate_empty_allows_all() {
        let config = AgentConfig {
            odin_url: String::new(),
            gitea_url: String::new(),
            gitea_token: String::new(),
            work_dir: "/tmp".to_string(),
            poll_interval_secs: 10,
            queue_name: "test".to_string(),
            project: None,
            allowed_repos: vec![],
            allowed_branches: vec![],
        };
        let gitea = crate::gitea::GiteaClient::new(String::new(), String::new());
        let runner = TaskRunner::new(config, gitea);

        assert!(runner.check_gate("any/repo", "any-branch").is_ok());
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("myorg/*", "myorg/repo"));
        assert!(!glob_match("myorg/*", "other/repo"));
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "main.py"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "other"));
    }
}
