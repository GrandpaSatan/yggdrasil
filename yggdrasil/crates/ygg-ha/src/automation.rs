//! Automation YAML generation via Odin's reasoning model.
//!
//! `AutomationGenerator` fetches the current entity states and available
//! services from the HA instance, builds a structured prompt, and delegates
//! generation to Odin's `/v1/chat/completions` endpoint (non-streaming).
//!
//! The returned string is the raw YAML extracted from the model's response.
//! No schema validation is performed — the user is responsible for reviewing
//! the YAML before adding it to their Home Assistant configuration.

use serde::Deserialize;

use crate::client::HaClient;
use crate::error::HaError;

// ─────────────────────────────────────────────────────────────────
// AutomationGenerator
// ─────────────────────────────────────────────────────────────────

/// Generates Home Assistant automation YAML by prompting Odin's reasoning model.
///
/// The generator is cheaply clonable — `reqwest::Client` is internally
/// `Arc`-based.  Construct once and store in application state.
#[derive(Clone)]
pub struct AutomationGenerator {
    /// Base URL of the Odin orchestrator (e.g., `http://localhost:8080`).
    odin_url: String,
    http: reqwest::Client,
    /// Model name to request from Odin (e.g., `qwen3:30b-a3b`).
    model: String,
}

// ─────────────────────────────────────────────────────────────────
// Internal deserialization types for Odin chat response
// ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChatMessage>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Option<Vec<ChatChoice>>,
}

// ─────────────────────────────────────────────────────────────────
// Prompt template
// ─────────────────────────────────────────────────────────────────

const SYSTEM_PROMPT_TEMPLATE: &str = "\
You are a Home Assistant automation expert. Generate valid Home Assistant automation YAML based on the user's description.

## Available Entities
{entity_summary}

## Available Services
{service_summary}

## Rules
- Output ONLY valid Home Assistant automation YAML inside a ```yaml code fence
- Use only entity IDs and services that exist in the lists above
- Include appropriate triggers, conditions, and actions
- Add a meaningful alias and description
- Use time patterns, state triggers, sun triggers, or numeric state triggers as appropriate
- For time-based automations, use the 'time' platform
- For state-based automations, use the 'state' platform
- Always include 'mode: single' unless the user specifies otherwise

## Output Format
Return ONLY the YAML automation block. No explanation before or after.";

// ─────────────────────────────────────────────────────────────────
// Implementation
// ─────────────────────────────────────────────────────────────────

impl AutomationGenerator {
    /// Construct a new generator.
    ///
    /// `odin_url` — base URL of the Odin service (trailing slash stripped).
    /// `model` — model name to request (e.g. `"qwen3:30b-a3b"`).
    #[must_use]
    pub fn new(odin_url: &str, model: &str) -> Self {
        Self {
            odin_url: odin_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            model: model.to_string(),
        }
    }

    /// Generate Home Assistant automation YAML from a natural-language description.
    ///
    /// Steps:
    /// 1. Fetch entity states from HA for context (truncated to 200 per domain).
    /// 2. Fetch available services from HA for context.
    /// 3. Build a structured system prompt with entity/service summaries.
    /// 4. POST to Odin `/v1/chat/completions` (non-streaming).
    /// 5. Extract the `yaml` fenced code block from the response.
    ///
    /// If no fenced block is found, the raw response content is returned as-is
    /// (the model may have produced raw YAML without the fence).
    pub async fn generate_automation(
        &self,
        ha: &HaClient,
        description: &str,
    ) -> Result<String, HaError> {
        // ── 1. Gather entity context ─────────────────────────────
        let states = ha.get_states().await?;
        let entity_summary = build_entity_summary(&states);

        // ── 2. Gather service context ────────────────────────────
        let services = ha.get_services().await?;
        let service_summary = build_service_summary(&services);

        // ── 3. Build prompt ──────────────────────────────────────
        let system_content = SYSTEM_PROMPT_TEMPLATE
            .replace("{entity_summary}", &entity_summary)
            .replace("{service_summary}", &service_summary);

        // ── 4. Call Odin ─────────────────────────────────────────
        let url = format!("{}/v1/chat/completions", self.odin_url);

        let body = serde_json::json!({
            "model": self.model,
            "stream": false,
            "messages": [
                { "role": "system", "content": system_content },
                { "role": "user",   "content": description },
            ]
        });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HaError::Generation(format!("failed to reach Odin at {url}: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HaError::Generation(format!(
                "Odin returned {status}: {text}"
            )));
        }

        let chat: ChatResponse = resp
            .json()
            .await
            .map_err(|e| HaError::Generation(format!("failed to parse Odin response: {e}")))?;

        let content = chat
            .choices
            .unwrap_or_default()
            .into_iter()
            .next()
            .and_then(|c| c.message)
            .and_then(|m| m.content)
            .unwrap_or_default();

        if content.is_empty() {
            return Err(HaError::Generation(
                "Odin returned an empty response".to_string(),
            ));
        }

        // ── 5. Extract YAML from fenced code block ───────────────
        Ok(extract_yaml(&content))
    }
}

// ─────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────

/// Extract the content of a ```yaml fenced code block.
///
/// Searches for the first occurrence of "```yaml" and the closing "```".
/// If no fenced block is found, returns the trimmed full content (the model
/// may have produced raw YAML without a fence).
fn extract_yaml(content: &str) -> String {
    // Look for ```yaml or ``` yaml (with optional space)
    if let Some(start_pos) = content.find("```yaml").or_else(|| content.find("``` yaml")) {
        // Skip past the opening fence line
        let after_fence = &content[start_pos..];
        if let Some(newline) = after_fence.find('\n') {
            let yaml_start = &after_fence[newline + 1..];
            // Find the closing fence
            if let Some(end_pos) = yaml_start.find("\n```") {
                return yaml_start[..end_pos].trim().to_string();
            }
            // No closing fence — return everything after the opening fence
            return yaml_start.trim().to_string();
        }
    }
    // No fence at all — return raw content
    content.trim().to_string()
}

/// Build a compact entity summary grouped by domain.
///
/// Each domain section lists up to 200 entities as `entity_id (friendly_name)`.
/// This keeps the prompt size manageable when there are many entities.
fn build_entity_summary(states: &[crate::client::EntityState]) -> String {
    use std::collections::BTreeMap;

    // Group by domain (prefix before the first '.')
    let mut by_domain: BTreeMap<&str, Vec<&crate::client::EntityState>> = BTreeMap::new();
    for state in states {
        let domain = state
            .entity_id
            .split_once('.')
            .map(|(d, _)| d)
            .unwrap_or("unknown");
        by_domain.entry(domain).or_default().push(state);
    }

    let mut out = String::new();
    for (domain, entities) in &by_domain {
        let limit = 200_usize;
        out.push_str(&format!("### {} ({} entities)\n", domain, entities.len()));
        for entity in entities.iter().take(limit) {
            let friendly = entity
                .attributes
                .get("friendly_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            out.push_str(&format!(
                "- {} [{}]",
                entity.entity_id, entity.state
            ));
            if !friendly.is_empty() {
                out.push_str(&format!(" ({})", friendly));
            }
            out.push('\n');
        }
        if entities.len() > limit {
            out.push_str(&format!(
                "- ... and {} more\n",
                entities.len() - limit
            ));
        }
        out.push('\n');
    }
    out
}

/// Build a compact service summary listing domains and their service names.
fn build_service_summary(services: &[crate::client::DomainServices]) -> String {
    let mut out = String::new();
    for domain_svc in services {
        let service_names: Vec<&str> = domain_svc.services.keys().map(|k| k.as_str()).collect();
        out.push_str(&format!(
            "- {}: {}\n",
            domain_svc.domain,
            service_names.join(", ")
        ));
    }
    out
}

// ─────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_yaml_with_fence() {
        let content = "Here is the automation:\n\n```yaml\nalias: test\nmode: single\n```\n";
        assert_eq!(extract_yaml(content), "alias: test\nmode: single");
    }

    #[test]
    fn extract_yaml_without_fence() {
        let content = "alias: test\nmode: single";
        assert_eq!(extract_yaml(content), "alias: test\nmode: single");
    }

    #[test]
    fn extract_yaml_no_closing_fence() {
        let content = "```yaml\nalias: test\nmode: single\n";
        assert_eq!(extract_yaml(content), "alias: test\nmode: single");
    }
}
