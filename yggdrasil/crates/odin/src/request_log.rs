/// JSONL request logging, feedback, and training data generation (Sprint 052, Phase 3).
///
/// Every routing decision is logged as a single JSON line to an append-only file.
/// Logging is fire-and-forget — writes never block the response pipeline.
///
/// The log captures both SDR and LLM classification results, enabling:
/// - Accuracy tracking via AI-driven feedback (`POST /api/v1/request/feedback`)
/// - Nightly SDR prototype reinforcement from confirmed classifications
/// - Training data generation for future LLM fine-tuning
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

// ─────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────

/// A single request log entry (one per routing decision).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogEntry {
    pub request_id: String,
    pub timestamp: DateTime<Utc>,
    /// Request source: "http", "voice", "task_worker".
    pub source: String,
    /// The user's message text (for training data generation).
    pub user_message: String,

    // Routing decisions (core training data).
    pub sdr_intent: Option<String>,
    pub sdr_confidence: Option<f64>,
    pub llm_intent: Option<String>,
    pub llm_confidence: Option<f64>,
    pub llm_agrees_with_sdr: Option<bool>,
    pub final_intent: String,
    /// How the decision was made: "llm_confirmed", "llm_override", "sdr_only", "keyword", "fallback".
    pub router_method: String,

    // Performance.
    pub model: String,
    pub backend: String,
    pub e2e_latency_ms: u64,
    pub router_latency_ms: Option<u64>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,

    pub session_id: String,
}

/// Feedback submitted by the requesting AI after evaluating a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub request_id: String,
    pub timestamp: DateTime<Utc>,
    /// Quality rating from the requesting AI (0.0–1.0).
    pub accuracy_rating: f64,
    /// Whether the AI requested a redo of the response.
    pub redo_requested: bool,
    /// Optional notes explaining why the redo was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback_notes: Option<String>,
}

/// Feedback request body for the HTTP API.
#[derive(Debug, Deserialize)]
pub struct FeedbackRequest {
    pub request_id: String,
    pub accuracy_rating: f64,
    #[serde(default)]
    pub redo_requested: bool,
    #[serde(default)]
    pub feedback_notes: Option<String>,
}

/// Query parameters for the request log endpoint.
#[derive(Debug, Deserialize)]
pub struct LogQueryParams {
    #[serde(default = "default_log_limit")]
    pub limit: usize,
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
}

fn default_log_limit() -> usize { 100 }

// ─────────────────────────────────────────────────────────────────
// RequestLogWriter
// ─────────────────────────────────────────────────────────────────

/// Append-only JSONL writer.  Thread-safe via `Arc<Mutex<BufWriter>>`.
///
/// All writes are fire-and-forget — failures are logged but never propagated
/// to the caller.  This ensures request logging never blocks responses.
#[derive(Clone)]
pub struct RequestLogWriter {
    inner: Arc<Mutex<tokio::io::BufWriter<tokio::fs::File>>>,
    path: PathBuf,
}

impl RequestLogWriter {
    /// Open (or create) the log file in append mode.
    pub async fn open(path: &Path) -> std::io::Result<Self> {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;

        Ok(Self {
            inner: Arc::new(Mutex::new(tokio::io::BufWriter::new(file))),
            path: path.to_path_buf(),
        })
    }

    /// Append a request log entry.  Never fails — errors are logged internally.
    pub async fn log(&self, entry: &RequestLogEntry) {
        if let Err(e) = self.append_json(entry).await {
            tracing::warn!(error = %e, path = %self.path.display(), "request_log: write failed");
        }
    }

    /// Append a feedback entry.  Never fails — errors are logged internally.
    pub async fn log_feedback(&self, entry: &FeedbackEntry) {
        if let Err(e) = self.append_json(entry).await {
            tracing::warn!(error = %e, path = %self.path.display(), "request_log: feedback write failed");
        }
    }

    async fn append_json<T: Serialize>(&self, value: &T) -> std::io::Result<()> {
        let mut line = serde_json::to_string(value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        line.push('\n');

        let mut guard = self.inner.lock().await;
        guard.write_all(line.as_bytes()).await?;
        guard.flush().await?;
        Ok(())
    }

    /// Read recent log entries (for the query API).  Reads from disk each time
    /// rather than maintaining an in-memory index — acceptable for debugging use.
    pub async fn query_recent(&self, params: &LogQueryParams) -> Vec<serde_json::Value> {
        let data = match tokio::fs::read_to_string(&self.path).await {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };

        let mut entries: Vec<serde_json::Value> = data
            .lines()
            .rev()
            .filter_map(|line| serde_json::from_str(line).ok())
            .filter(|v: &serde_json::Value| {
                // Filter by intent if specified.
                if let Some(ref intent) = params.intent {
                    if v.get("final_intent").and_then(|v| v.as_str()) != Some(intent) {
                        return false;
                    }
                }
                // Filter by timestamp if specified.
                if let Some(since) = params.since {
                    if let Some(ts_str) = v.get("timestamp").and_then(|v| v.as_str()) {
                        if let Ok(ts) = ts_str.parse::<DateTime<Utc>>() {
                            if ts < since {
                                return false;
                            }
                        }
                    }
                }
                true
            })
            .take(params.limit)
            .collect();

        entries.reverse(); // chronological order
        entries
    }
}

// ─────────────────────────────────────────────────────────────────
// Training data generation (nightly self-tuning)
// ─────────────────────────────────────────────────────────────────

/// A training example for LLM fine-tuning (collected from high-confidence logs).
#[derive(Debug, Clone, Serialize)]
pub struct TrainingExample {
    pub user_message: String,
    pub correct_intent: String,
    pub confidence: f64,
    /// How this example was produced.
    pub source: String,
}

/// Generate training data from request logs since a given timestamp.
///
/// Filters for entries with feedback `accuracy_rating >= min_rating` and
/// `!redo_requested`, producing structured training triples.
pub async fn generate_training_data(
    log_path: &Path,
    since: DateTime<Utc>,
    min_rating: f64,
) -> Vec<TrainingExample> {
    let data = match tokio::fs::read_to_string(log_path).await {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    // Build a map of request_id → feedback for joining.
    let mut feedback_map: std::collections::HashMap<String, FeedbackEntry> =
        std::collections::HashMap::new();
    let mut log_entries: Vec<RequestLogEntry> = Vec::new();

    for line in data.lines() {
        // Try parsing as feedback first (has accuracy_rating field).
        if let Ok(fb) = serde_json::from_str::<FeedbackEntry>(line) {
            feedback_map.insert(fb.request_id.clone(), fb);
            continue;
        }
        // Then try as a regular log entry.
        if let Ok(entry) = serde_json::from_str::<RequestLogEntry>(line) {
            if entry.timestamp >= since {
                log_entries.push(entry);
            }
        }
    }

    log_entries
        .into_iter()
        .filter_map(|entry| {
            let fb = feedback_map.get(&entry.request_id)?;
            if fb.accuracy_rating < min_rating || fb.redo_requested {
                return None;
            }
            Some(TrainingExample {
                user_message: entry.user_message.clone(),
                correct_intent: entry.final_intent,
                confidence: fb.accuracy_rating,
                source: entry.router_method,
            })
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn log_and_read_round_trip() {
        let tmp = std::env::temp_dir().join("odin-reqlog-test.jsonl");
        let _ = tokio::fs::remove_file(&tmp).await;

        let writer = RequestLogWriter::open(&tmp).await.unwrap();

        let entry = RequestLogEntry {
            request_id: "test-001".into(),
            timestamp: Utc::now(),
            source: "http".into(),
            user_message: "Write a Rust function".into(),
            sdr_intent: Some("coding".into()),
            sdr_confidence: Some(0.82),
            llm_intent: Some("coding".into()),
            llm_confidence: Some(0.91),
            llm_agrees_with_sdr: Some(true),
            final_intent: "coding".into(),
            router_method: "llm_confirmed".into(),
            model: "qwen3-coder".into(),
            backend: "munin".into(),
            e2e_latency_ms: 450,
            router_latency_ms: Some(120),
            tokens_in: Some(200),
            tokens_out: Some(150),
            session_id: "sess-1".into(),
        };

        writer.log(&entry).await;

        let params = LogQueryParams {
            limit: 10,
            intent: None,
            since: None,
        };
        let results = writer.query_recent(&params).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["request_id"], "test-001");
        assert_eq!(results[0]["final_intent"], "coding");

        let _ = tokio::fs::remove_file(&tmp).await;
    }

    #[tokio::test]
    async fn query_filters_by_intent() {
        let tmp = std::env::temp_dir().join("odin-reqlog-filter-test.jsonl");
        let _ = tokio::fs::remove_file(&tmp).await;

        let writer = RequestLogWriter::open(&tmp).await.unwrap();

        let make_entry = |id: &str, intent: &str| RequestLogEntry {
            request_id: id.into(),
            timestamp: Utc::now(),
            source: "http".into(),
            user_message: "test message".into(),
            sdr_intent: None,
            sdr_confidence: None,
            llm_intent: None,
            llm_confidence: None,
            llm_agrees_with_sdr: None,
            final_intent: intent.into(),
            router_method: "keyword".into(),
            model: "test".into(),
            backend: "test".into(),
            e2e_latency_ms: 100,
            router_latency_ms: None,
            tokens_in: None,
            tokens_out: None,
            session_id: "s1".into(),
        };

        writer.log(&make_entry("r1", "coding")).await;
        writer.log(&make_entry("r2", "reasoning")).await;
        writer.log(&make_entry("r3", "coding")).await;

        let params = LogQueryParams {
            limit: 100,
            intent: Some("coding".into()),
            since: None,
        };
        let results = writer.query_recent(&params).await;
        assert_eq!(results.len(), 2);

        let _ = tokio::fs::remove_file(&tmp).await;
    }

    #[test]
    fn feedback_entry_serialization() {
        let fb = FeedbackEntry {
            request_id: "test-001".into(),
            timestamp: Utc::now(),
            accuracy_rating: 0.85,
            redo_requested: false,
            feedback_notes: None,
        };
        let json = serde_json::to_string(&fb).unwrap();
        assert!(json.contains("accuracy_rating"));
        let parsed: FeedbackEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.request_id, "test-001");
    }
}
