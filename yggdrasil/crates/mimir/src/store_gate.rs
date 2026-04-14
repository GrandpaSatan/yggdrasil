//! Sprint 064 P1.5 — server-side LLM store gate.
//!
//! Disambiguates near-duplicate engrams using a small instruction-tuned model
//! (LFM2.5-1.2B-Instruct by default; any Ollama model emitting a JSON verdict
//! works). Fires only when the SDR top-1 candidate is at/above
//! `NoveltyConfig.update_threshold` — the common "no candidate" path skips the
//! gate entirely (zero added latency).
//!
//! Backends are tried in order. With a Munin-local primary and a Hugin
//! secondary, USB4 makes the cross-node cost negligible — the chain gives free
//! hot-failover when the primary GPU is contended. On exhaustion the caller
//! falls back to the threshold classifier.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ygg_domain::config::{StoreGateBackend, StoreGateConfig};

use crate::novelty::NoveltyVerdict;

/// Decision returned by the store gate. `store_worthy=false` short-circuits
/// the caller into a no-op response (verdict carries the matched id when
/// relevant).
#[derive(Debug, Clone)]
pub struct StoreGateDecision {
    pub verdict: NoveltyVerdict,
    pub store_worthy: bool,
    pub reasoning: String,
    /// Index of the backend in `cfg.backends` that produced the verdict.
    /// Useful for metrics and log triage.
    pub backend_index: usize,
}

/// Errors the caller should handle by falling back to the threshold gate.
#[derive(Debug, thiserror::Error)]
pub enum StoreGateError {
    #[error("store gate disabled in config")]
    Disabled,
    #[error("store gate has no backends configured")]
    NoBackends,
    #[error("all {tried} backends failed; last error: {last}")]
    AllBackendsFailed { tried: usize, last: String },
}

/// Maximum chars per cause/effect field included in the gate prompt. Bounds
/// generation latency — model prompt-processing scales roughly linearly with
/// input length and a 5KB sprint summary will blow past any sensible timeout.
const MAX_FIELD_CHARS: usize = 500;

fn truncate(s: &str) -> String {
    if s.chars().count() <= MAX_FIELD_CHARS {
        s.to_owned()
    } else {
        let head: String = s.chars().take(MAX_FIELD_CHARS).collect();
        format!("{head}…[truncated]")
    }
}

/// Iterate through configured backends, returning the first valid verdict.
///
/// `similarity_hint` is the SDR Hamming similarity that surfaced the
/// candidate; included in the prompt so the model has the same context the
/// threshold gate would have used.
pub async fn classify(
    client: &reqwest::Client,
    cfg: &StoreGateConfig,
    new_cause: &str,
    new_effect: &str,
    candidate_id: Uuid,
    candidate_cause: &str,
    candidate_effect: &str,
    similarity_hint: f64,
) -> Result<StoreGateDecision, StoreGateError> {
    if !cfg.enabled {
        return Err(StoreGateError::Disabled);
    }
    if cfg.backends.is_empty() {
        return Err(StoreGateError::NoBackends);
    }

    let new_cause_t = truncate(new_cause);
    let new_effect_t = truncate(new_effect);
    let candidate_cause_t = truncate(candidate_cause);
    let candidate_effect_t = truncate(candidate_effect);
    let prompt = build_prompt(
        &new_cause_t,
        &new_effect_t,
        candidate_id,
        &candidate_cause_t,
        &candidate_effect_t,
        similarity_hint,
    );

    let mut last_err = String::from("(no attempts)");
    for (idx, backend) in cfg.backends.iter().enumerate() {
        match call_backend(
            client,
            backend,
            cfg.timeout_ms,
            cfg.keep_alive_secs,
            &prompt,
            candidate_id,
            &candidate_cause_t,
            &candidate_effect_t,
        )
        .await
        {
            Ok(mut decision) => {
                decision.backend_index = idx;
                return Ok(decision);
            }
            Err(e) => {
                tracing::warn!(
                    backend = %backend.url,
                    model = %backend.model,
                    error = %e,
                    "store gate backend failed; trying next"
                );
                last_err = e;
            }
        }
    }

    Err(StoreGateError::AllBackendsFailed {
        tried: cfg.backends.len(),
        last: last_err,
    })
}

/// One round-trip to a single Ollama backend. Returns the parsed decision
/// (with `backend_index` left at 0 — the caller patches it).
async fn call_backend(
    client: &reqwest::Client,
    backend: &StoreGateBackend,
    timeout_ms: u64,
    keep_alive_secs: u64,
    prompt: &str,
    candidate_id: Uuid,
    candidate_cause: &str,
    candidate_effect: &str,
) -> Result<StoreGateDecision, String> {
    let url = format!("{}/api/generate", backend.url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": backend.model,
        "prompt": prompt,
        "stream": false,
        "format": "json",
        "keep_alive": format!("{keep_alive_secs}s"),
        "options": {
            "temperature": 0.0,
            "num_predict": 256,
        }
    });

    let resp = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        client.post(&url).json(&body).send(),
    )
    .await
    .map_err(|_| format!("timed out after {timeout_ms}ms"))?
    .map_err(|e| format!("http error: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }

    let body: OllamaResponse = resp
        .json()
        .await
        .map_err(|e| format!("ollama envelope: {e}"))?;

    let mut decision = parse_decision(&body.response, candidate_id)
        .map_err(|e| format!("parse: {e}"))?;
    if let NoveltyVerdict::Update {
        ref mut previous_cause,
        ref mut previous_effect,
        ..
    } = decision.verdict
    {
        *previous_cause = candidate_cause.to_owned();
        *previous_effect = candidate_effect.to_owned();
    }
    Ok(decision)
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    response: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct VerdictPayload {
    /// "new" | "update" | "old"
    verdict: String,
    #[serde(default = "default_store_worthy")]
    store_worthy: bool,
    #[serde(default)]
    reasoning: String,
}

fn default_store_worthy() -> bool {
    true
}

fn build_prompt(
    new_cause: &str,
    new_effect: &str,
    candidate_id: Uuid,
    candidate_cause: &str,
    candidate_effect: &str,
    similarity_hint: f64,
) -> String {
    format!(
        "You are a memory storage gate for the Yggdrasil project. Decide whether and how to STORE a new memory engram given the nearest existing engram.\n\
         \n\
         New engram:\n\
           cause: {new_cause}\n\
           effect: {new_effect}\n\
         \n\
         Nearest existing engram (SDR similarity={similarity_hint:.2}):\n\
           id: {candidate_id}\n\
           cause: {candidate_cause}\n\
           effect: {candidate_effect}\n\
         \n\
         Choose ONE verdict:\n\
         - \"old\":    existing already covers this; skip the write.\n\
         - \"update\": same topic but content meaningfully changed (new dates, fields, corrections, or substantively different effect); overwrite existing.\n\
         - \"new\":    genuinely different content even if vocabulary overlaps (e.g. different sprint number, different incident); keep both.\n\
         \n\
         Output ONLY a JSON object with this exact shape:\n\
         {{\"verdict\":\"new|update|old\",\"store_worthy\":true|false,\"reasoning\":\"<one sentence>\"}}"
    )
}

fn parse_decision(raw: &str, candidate_id: Uuid) -> Result<StoreGateDecision, String> {
    let payload: VerdictPayload =
        serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;

    let verdict = match payload.verdict.trim().to_lowercase().as_str() {
        "new" => NoveltyVerdict::New,
        "update" => NoveltyVerdict::Update {
            id: candidate_id,
            previous_cause: String::new(),
            previous_effect: String::new(),
        },
        "old" => NoveltyVerdict::Old { id: candidate_id },
        other => return Err(format!("unknown verdict '{other}'")),
    };

    Ok(StoreGateDecision {
        verdict,
        store_worthy: payload.store_worthy,
        reasoning: payload.reasoning,
        backend_index: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_new_verdict() {
        let id = Uuid::new_v4();
        let raw = r#"{"verdict":"new","store_worthy":true,"reasoning":"different sprint"}"#;
        let d = parse_decision(raw, id).unwrap();
        assert!(matches!(d.verdict, NoveltyVerdict::New));
        assert!(d.store_worthy);
        assert_eq!(d.reasoning, "different sprint");
    }

    #[test]
    fn parse_update_attaches_candidate_id() {
        let id = Uuid::new_v4();
        let raw = r#"{"verdict":"update","store_worthy":true,"reasoning":"new dates"}"#;
        let d = parse_decision(raw, id).unwrap();
        match d.verdict {
            NoveltyVerdict::Update { id: out_id, .. } => assert_eq!(out_id, id),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn parse_old_attaches_candidate_id() {
        let id = Uuid::new_v4();
        let raw = r#"{"verdict":"old","store_worthy":false,"reasoning":"identical"}"#;
        let d = parse_decision(raw, id).unwrap();
        assert!(matches!(d.verdict, NoveltyVerdict::Old { id: x } if x == id));
        assert!(!d.store_worthy);
    }

    #[test]
    fn parse_handles_uppercase_verdict() {
        let id = Uuid::new_v4();
        let raw = r#"{"verdict":"UPDATE","store_worthy":true,"reasoning":""}"#;
        assert!(matches!(
            parse_decision(raw, id).unwrap().verdict,
            NoveltyVerdict::Update { .. }
        ));
    }

    #[test]
    fn parse_rejects_unknown_verdict() {
        let id = Uuid::new_v4();
        let raw = r#"{"verdict":"maybe","store_worthy":true,"reasoning":""}"#;
        assert!(parse_decision(raw, id).is_err());
    }

    #[test]
    fn parse_rejects_malformed_json() {
        assert!(parse_decision("not json at all", Uuid::new_v4()).is_err());
    }

    #[test]
    fn parse_defaults_store_worthy_when_missing() {
        let id = Uuid::new_v4();
        let raw = r#"{"verdict":"new"}"#;
        assert!(parse_decision(raw, id).unwrap().store_worthy);
    }

    #[test]
    fn build_prompt_contains_both_engrams() {
        let id = Uuid::new_v4();
        let p = build_prompt("c1", "e1", id, "c2", "e2", 0.93);
        assert!(p.contains("c1") && p.contains("e1"));
        assert!(p.contains("c2") && p.contains("e2"));
        assert!(p.contains("0.93"));
        assert!(p.contains(&id.to_string()));
    }

    #[test]
    fn truncate_caps_long_inputs() {
        let s = "x".repeat(800);
        let out = truncate(&s);
        assert!(out.ends_with("[truncated]"));
        assert!(out.chars().count() <= MAX_FIELD_CHARS + "…[truncated]".chars().count());
    }

    #[test]
    fn truncate_passes_short_inputs_through() {
        assert_eq!(truncate("short"), "short");
    }
}
