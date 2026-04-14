//! Flow runner — invokes Odin flows by name during dream cycles and persists
//! the result to Mimir as a tagged engram.
//!
//! Intentionally minimal: we do NOT re-implement Odin's FlowEngine here.
//! Instead we POST the flow invocation to Odin and trust Odin's routing to
//! dispatch through llama-swap (post-Track-B) or Ollama (pre-cutover).

use std::time::Duration;

use reqwest::Client;
use serde_json::json;

use crate::config::DreamFlow;

/// Execute a dream flow via Odin's chat endpoint, then persist the output
/// to Mimir as a dreamer-tagged engram.
pub async fn run_dream(
    client: &Client,
    odin_url: &str,
    mimir_url: &str,
    flow: &DreamFlow,
    sprint_tag: &str,
) -> anyhow::Result<String> {
    // Step 1: invoke flow via Odin chat endpoint with explicit flow_name.
    let body = json!({
        "messages": [{"role": "user", "content": flow.prompt}],
        "stream": false,
        "metadata": { "flow_name": flow.flow }
    });

    let chat_url = format!("{}/v1/chat/completions", odin_url.trim_end_matches('/'));
    let resp = client
        .post(&chat_url)
        .timeout(Duration::from_secs(300))
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("dream flow POST {}: {e}", chat_url))?;

    if !resp.status().is_success() {
        let status = resp.status();
        return Err(anyhow::anyhow!("dream flow {} returned {}", flow.name, status));
    }

    let data: serde_json::Value = resp.json().await
        .map_err(|e| anyhow::anyhow!("parse dream flow response: {e}"))?;

    let text = data
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if text.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "dream flow {} produced empty output",
            flow.name
        ));
    }

    // Step 2: persist as Mimir engram tagged with dreamer + sprint_tag +
    // flow name. project stays "yggdrasil" — adjust when dreamer is
    // extended to cross-project runs.
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let cause = format!("{} at {}", flow.name, now);
    let tags = vec!["dreamer".to_string(), sprint_tag.to_string(), flow.name.clone()];
    let store_body = json!({
        "cause": cause,
        "effect": text,
        "tags": tags,
        "project": "yggdrasil",
        "force": false,
    });

    let store_url = format!("{}/api/v1/store", mimir_url.trim_end_matches('/'));
    let store_resp = client
        .post(&store_url)
        .timeout(Duration::from_secs(30))
        .json(&store_body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("mimir store POST {}: {e}", store_url))?;

    if !store_resp.status().is_success() {
        let status = store_resp.status();
        tracing::warn!(flow = %flow.name, status = %status, "dream engram store returned non-success");
    }

    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dream_flow_serializes_round_trip() {
        let f = DreamFlow {
            name: "dream_exploration".into(),
            cron: Some("0 */1 * * *".into()),
            prompt: "Reflect on recent engineering activity.".into(),
            flow: "dream_exploration".into(),
        };
        let s = serde_json::to_string(&f).unwrap();
        let parsed: DreamFlow = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.flow, "dream_exploration");
    }
}
