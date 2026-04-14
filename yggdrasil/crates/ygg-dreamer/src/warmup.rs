//! Warmup loop — fires deterministic prefix completions against the configured
//! backends so LMCache retains hot KV state for the common flow prefixes.
//!
//! Sprint 065 C·P3. When LMCache (Track B·P10) is deployed, every warmup
//! completion populates the disk tier; a subsequent real user request with
//! the same prefix skips prefill. Without LMCache, the warmup still works
//! (just populates vLLM's in-process prefix cache) but the benefit is
//! bounded by model-swap / vLLM-restart boundaries.

use std::time::Duration;

use reqwest::Client;
use serde_json::json;

use crate::config::WarmupPrefix;

/// Fire a single warmup completion. `max_tokens=1` because we only need to
/// force prefill — decoded tokens are discarded.
pub async fn fire_one(
    client: &Client,
    prefix: &WarmupPrefix,
) -> anyhow::Result<()> {
    let body = json!({
        "model": prefix.model,
        "messages": [
            { "role": "system", "content": prefix.system },
            { "role": "user",   "content": prefix.user_prefix },
        ],
        "max_tokens": 1,
        "stream": false,
    });

    let url = format!("{}/v1/chat/completions", prefix.url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .timeout(Duration::from_secs(120))
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("warmup POST {}: {e}", url))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "warmup {} non-success: {} ({})",
            prefix.name,
            status,
            body.chars().take(200).collect::<String>()
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmup_prefix_serializes_round_trip() {
        let prefix = WarmupPrefix {
            name: "test".into(),
            model: "gemma4:e4b".into(),
            url: "http://127.0.0.1:11500".into(),
            system: "You are a tester.".into(),
            user_prefix: "ping".into(),
        };
        let serialized = serde_json::to_string(&prefix).unwrap();
        let parsed: WarmupPrefix = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed.name, prefix.name);
        assert_eq!(parsed.model, prefix.model);
    }
}
