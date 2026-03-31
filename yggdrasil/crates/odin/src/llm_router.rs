/// LLM-based "System 2" intent classifier (Sprint 052).
///
/// Sends structured classification prompts to a lightweight LLM (Liquid AI
/// LFM2.5-1.2B-Instruct) running on Hugin via Ollama.  The LLM receives the
/// user message plus the SDR router's fast suggestion as a hint, and returns a
/// JSON classification with intent, confidence, and agreement flag.
///
/// All failure modes (timeout, parse error, circuit open, semaphore full)
/// return `None`, causing the caller to fall through to the keyword router.
/// This follows the same graceful-degradation pattern as `rag.rs`.
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::sdr_router::SdrClassification;
use crate::state::CircuitBreaker;

// ─────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────

/// Result of an LLM classification call.
#[derive(Debug, Clone, Serialize)]
pub struct LlmClassification {
    /// Classified intent (e.g. "coding", "reasoning", "home_automation").
    pub intent: String,
    /// Model's self-reported confidence (0.0–1.0).
    pub confidence: f64,
    /// Brief reasoning for the classification.
    pub reasoning: String,
    /// Whether the LLM agreed with the SDR router's suggestion.
    pub agrees_with_sdr: bool,
    /// Suggested context sources (e.g. ["muninn", "mimir", "ha"]).
    pub context_sources: Vec<String>,
}

/// Raw JSON response from the LLM (deserialized from the model's output).
#[derive(Debug, Deserialize)]
struct RawClassification {
    intent: String,
    confidence: f64,
    #[serde(default)]
    reasoning: String,
    #[serde(default)]
    agrees_with_sdr: bool,
    #[serde(default)]
    context_sources: Vec<String>,
}

/// Ollama `/api/chat` response (subset of fields we need).
#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    content: String,
    /// Some models (like lfm2.5-thinking) put output in a separate `thinking` field.
    #[serde(default)]
    thinking: Option<String>,
}

// ─────────────────────────────────────────────────────────────────
// LlmRouterClient
// ─────────────────────────────────────────────────────────────────

/// Client for the Liquid AI LFM router model on Hugin.
///
/// Thread-safe and cheap to clone (all inner state is Arc-wrapped).
#[derive(Clone)]
pub struct LlmRouterClient {
    http: reqwest::Client,
    ollama_url: String,
    model: String,
    timeout: Duration,
    min_confidence: f64,
    semaphore: Arc<Semaphore>,
    circuit_breaker: Arc<CircuitBreaker>,
}

impl LlmRouterClient {
    /// Construct a new client from config values.
    pub fn new(
        http: reqwest::Client,
        ollama_url: String,
        model: String,
        timeout_ms: u64,
        min_confidence: f64,
        max_concurrent: usize,
    ) -> Self {
        Self {
            http,
            ollama_url,
            model,
            timeout: Duration::from_millis(timeout_ms),
            min_confidence,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            circuit_breaker: Arc::new(CircuitBreaker::new()),
        }
    }

    /// Classify a user message with an optional SDR hint.
    ///
    /// Returns `None` on any failure (timeout, parse error, circuit open,
    /// semaphore full, confidence below threshold).  The caller should fall
    /// through to the keyword router when this returns `None`.
    pub async fn classify(
        &self,
        message: &str,
        sdr_hint: Option<&SdrClassification>,
    ) -> Option<LlmClassification> {
        // Gate 1: circuit breaker.
        if !self.circuit_breaker.allow_request() {
            tracing::debug!("llm_router: circuit breaker open — skipping");
            return None;
        }

        // Gate 2: concurrency semaphore (non-blocking).
        let _permit = match self.semaphore.try_acquire() {
            Ok(permit) => permit,
            Err(_) => {
                tracing::debug!("llm_router: semaphore full — skipping");
                return None;
            }
        };

        // Build the classification prompt.
        let system_prompt = build_system_prompt(sdr_hint);
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": message }
            ],
            "stream": false,
            "options": {
                "temperature": 0.1,
                "num_predict": 128
            }
        });

        let url = format!("{}/api/chat", self.ollama_url.trim_end_matches('/'));

        // Gate 3: timeout.
        let result = tokio::time::timeout(self.timeout, async {
            self.http
                .post(&url)
                .json(&body)
                .send()
                .await?
                .json::<OllamaChatResponse>()
                .await
        })
        .await;

        let response = match result {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "llm_router: HTTP error");
                self.circuit_breaker.record_failure();
                return None;
            }
            Err(_) => {
                tracing::debug!(timeout_ms = self.timeout.as_millis(), "llm_router: timeout");
                self.circuit_breaker.record_failure();
                return None;
            }
        };

        // Parse the model's JSON output.
        let content = response.message.content.trim();
        // The model may wrap JSON in ```json ... ``` — strip it.
        let json_str = strip_markdown_fence(content);

        let raw: RawClassification = match serde_json::from_str(json_str) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    content = %content,
                    "llm_router: failed to parse classification JSON"
                );
                self.circuit_breaker.record_failure();
                return None;
            }
        };

        // Validate confidence.
        if raw.confidence < self.min_confidence {
            tracing::debug!(
                intent = %raw.intent,
                confidence = raw.confidence,
                min = self.min_confidence,
                "llm_router: confidence below threshold"
            );
            // Low confidence is not a circuit-breaker failure — the model is working.
            self.circuit_breaker.record_success();
            return None;
        }

        self.circuit_breaker.record_success();

        Some(LlmClassification {
            intent: raw.intent,
            confidence: raw.confidence,
            reasoning: raw.reasoning,
            agrees_with_sdr: raw.agrees_with_sdr,
            context_sources: raw.context_sources,
        })
    }

    /// Access the circuit breaker (for testing / metrics).
    pub fn circuit_breaker(&self) -> &CircuitBreaker {
        &self.circuit_breaker
    }
}

// ─────────────────────────────────────────────────────────────────
// Prompt construction
// ─────────────────────────────────────────────────────────────────

fn build_system_prompt(sdr_hint: Option<&SdrClassification>) -> String {
    let hint_line = match sdr_hint {
        Some(cls) => format!(
            "\nThe fast router suggests: {} (confidence: {:.2})\n",
            cls.intent, cls.confidence
        ),
        None => "\nThe fast router had no suggestion.\n".to_string(),
    };

    format!(
        r#"You are an intent classifier for Yggdrasil, a home server AI assistant.
{hint_line}
Classify the user's message into exactly one intent:
- coding: Programming, debugging, code review, build errors, architecture
- reasoning: Explanations, analysis, comparisons, design decisions, planning
- home_automation: Smart home, lights, sensors, climate, Home Assistant entities
- gaming: VM management, gaming PCs, Proxmox, GPU passthrough
- default: General questions, greetings, anything else

Respond with ONLY valid JSON (no markdown, no explanation):
{{"intent":"<intent>","confidence":<0.0-1.0>,"reasoning":"<brief>","agrees_with_sdr":<bool>,"context_sources":["muninn"|"mimir"|"ha"]}}"#
    )
}

/// Strip ```json ... ``` fences that models sometimes wrap around JSON output.
fn strip_markdown_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = s.strip_prefix("```") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        s
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_fence_plain_json() {
        let input = r#"{"intent":"coding","confidence":0.9}"#;
        assert_eq!(strip_markdown_fence(input), input);
    }

    #[test]
    fn strip_fence_with_json_fence() {
        let input = "```json\n{\"intent\":\"coding\"}\n```";
        assert_eq!(strip_markdown_fence(input), "{\"intent\":\"coding\"}");
    }

    #[test]
    fn strip_fence_with_bare_fence() {
        let input = "```\n{\"intent\":\"coding\"}\n```";
        assert_eq!(strip_markdown_fence(input), "{\"intent\":\"coding\"}");
    }

    #[test]
    fn raw_classification_deserializes_minimal() {
        let json = r#"{"intent":"coding","confidence":0.85}"#;
        let raw: RawClassification = serde_json::from_str(json).unwrap();
        assert_eq!(raw.intent, "coding");
        assert_eq!(raw.confidence, 0.85);
        assert!(!raw.agrees_with_sdr); // default
        assert!(raw.context_sources.is_empty()); // default
    }

    #[test]
    fn raw_classification_deserializes_full() {
        let json = r#"{
            "intent": "home_automation",
            "confidence": 0.92,
            "reasoning": "user wants to turn on lights",
            "agrees_with_sdr": true,
            "context_sources": ["ha", "mimir"]
        }"#;
        let raw: RawClassification = serde_json::from_str(json).unwrap();
        assert_eq!(raw.intent, "home_automation");
        assert!(raw.agrees_with_sdr);
        assert_eq!(raw.context_sources, vec!["ha", "mimir"]);
    }

    #[test]
    fn system_prompt_includes_sdr_hint() {
        let hint = SdrClassification {
            intent: "coding".into(),
            confidence: 0.82,
            runner_up_intent: None,
            runner_up_confidence: None,
        };
        let prompt = build_system_prompt(Some(&hint));
        assert!(prompt.contains("coding"));
        assert!(prompt.contains("0.82"));
    }

    #[test]
    fn system_prompt_without_hint() {
        let prompt = build_system_prompt(None);
        assert!(prompt.contains("no suggestion"));
    }
}
