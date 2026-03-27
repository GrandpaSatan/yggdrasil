//! Background service that periodically checks Recall tier capacity and summarizes
//! aging engrams into Archival tier summaries via Odin LLM calls.
//!
//! ## Lifecycle
//!
//! `SummarizationService::start()` spawns a single Tokio task. The task loops with
//! `check_interval_secs` between cycles. Each cycle:
//!
//! 1. Fetches tier counts from PostgreSQL.
//! 2. If Recall count is within capacity, sleeps until next cycle.
//! 3. If over capacity, fetches the oldest/least-accessed batch of Recall engrams.
//! 4. Calls Odin `POST /v1/chat/completions` with a summarization prompt.
//! 5. Parses the `{cause, effect}` JSON from the LLM response.
//! 6. Stores the summary as a new Archival engram in PostgreSQL + Qdrant.
//! 7. Marks the source engrams as `archived` in PostgreSQL.
//! 8. Deletes the source engram vectors from Qdrant.
//!
//! ## Error handling
//!
//! All errors within a cycle are logged as warnings. The cycle returns and the
//! task retries on the next tick. No panics — the task must remain alive for the
//! lifetime of the process.
//!
//! ## Hardware notes
//!
//! Target: Munin (Intel Core Ultra 185H, 48GB DDR5). The task runs on a single
//! Tokio task and is I/O-bound (PG queries + Odin HTTP call). Peak RSS < 2MB
//! per cycle (batch of 100 engrams ~50KB text + prompt ~60KB).

use tokio::sync::watch;
use uuid::Uuid;

use ygg_domain::config::TierConfig;
use ygg_store::{Store, qdrant::VectorStore};
use ygg_store::postgres::engrams;

use ygg_embed::OnnxEmbedder;

use crate::error::MimirError;

/// System prompt for the summarization LLM call.
const SYSTEM_PROMPT: &str = "\
You are a memory consolidation system. You receive a batch of cause-effect memory \
pairs and must produce a single consolidated summary that preserves all important \
information.

## Rules
- Preserve key facts, decisions, and lessons learned
- Merge duplicate or overlapping information
- Maintain cause-effect relationships where meaningful
- Output a single cause-effect pair where:
  - \"cause\" is a concise description of the topics and contexts covered
  - \"effect\" is the consolidated knowledge and outcomes
- Keep the total output under 2000 characters
- Do not add information not present in the source memories
- Output ONLY valid JSON: {\"cause\": \"...\", \"effect\": \"...\"}";

/// Request body sent to Odin for chat completions.
#[derive(Debug, serde::Serialize)]
struct ChatCompletionRequest {
    model: Option<String>,
    messages: Vec<ChatMessage>,
    stream: bool,
    max_tokens: u32,
    temperature: f32,
}

#[derive(Debug, serde::Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

/// Minimal response shape from Odin (OpenAI-compatible).
#[derive(Debug, serde::Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, serde::Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, serde::Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

/// Parsed summary output from the LLM.
#[derive(Debug, serde::Deserialize)]
struct SummaryJson {
    cause: String,
    effect: String,
}

/// Background service for summarizing aging Recall engrams into Archival summaries.
pub struct SummarizationService {
    store: Store,
    vectors: VectorStore,
    embedder: OnnxEmbedder,
    http: reqwest::Client,
    config: TierConfig,
    shutdown_rx: watch::Receiver<bool>,
}

impl SummarizationService {
    /// Create a new `SummarizationService`.
    ///
    /// The `shutdown_rx` receiver is subscribed from the `AppState` watch channel.
    /// When `true` is sent on the channel, the background task stops after the
    /// current cycle completes.
    pub fn new(
        store: Store,
        vectors: VectorStore,
        embedder: OnnxEmbedder,
        config: TierConfig,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        // Timeout of 120s matches the sprint doc requirement for Odin summarization calls.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            // reqwest::Client::builder().build() only fails on TLS init, which would
            // be a fatal misconfiguration. Use expect here at service construction time.
            .expect("failed to build reqwest client for summarization service");

        Self {
            store,
            vectors,
            embedder,
            http,
            config,
            shutdown_rx,
        }
    }

    /// Spawn the background summarization task.
    ///
    /// Returns a `JoinHandle` — the caller can drop it (fire-and-forget) or await it
    /// for clean shutdown testing.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
        })
    }

    /// Main loop: sleep for `check_interval`, then run one summarization cycle.
    /// Exits cleanly when the shutdown signal is received.
    async fn run(mut self) {
        let interval = std::time::Duration::from_secs(self.config.check_interval_secs);
        tracing::info!(
            check_interval_secs = self.config.check_interval_secs,
            recall_capacity = self.config.recall_capacity,
            batch_size = self.config.summarization_batch_size,
            odin_url = %self.config.odin_url,
            "summarization service started"
        );

        loop {
            tokio::select! {
                // Wait for the check interval before each cycle.
                _ = tokio::time::sleep(interval) => {
                    if let Err(e) = self.check_and_summarize().await {
                        // Log and continue — the task must not die on transient errors.
                        tracing::warn!(error = %e, "summarization cycle failed, will retry next interval");
                    }
                }
                // Graceful shutdown: wait for the shutdown signal to become true.
                result = self.shutdown_rx.changed() => {
                    if result.is_ok() && *self.shutdown_rx.borrow() {
                        tracing::info!("summarization service stopped");
                        break;
                    }
                }
            }
        }
    }

    /// Run one summarization cycle.
    ///
    /// Returns `Ok(())` if the cycle completed without needing to summarize (Recall
    /// count within capacity) or after a successful summarization. Returns `Err` only
    /// on hard failures that should be logged by the caller.
    async fn check_and_summarize(&self) -> Result<(), MimirError> {
        let pool = self.store.pool();

        // --- Step 1: Check if Recall tier is over capacity ---
        let stats = engrams::get_stats(pool).await?;
        let recall_count = stats.recall_count;
        let recall_capacity = self.config.recall_capacity as i64;

        if recall_count <= recall_capacity {
            tracing::debug!(
                recall_count,
                recall_capacity,
                "recall tier within capacity, skipping summarization"
            );
            return Ok(());
        }

        tracing::info!(
            recall_count,
            recall_capacity,
            "recall tier over capacity, starting summarization"
        );

        // --- Step 2: Fetch oldest/least-accessed batch eligible for summarization ---
        let batch = engrams::get_oldest_recall_engrams(
            pool,
            self.config.summarization_batch_size,
            self.config.min_age_secs,
        )
        .await?;

        if batch.is_empty() {
            tracing::info!(
                min_age_secs = self.config.min_age_secs,
                "no recall engrams old enough for summarization, skipping"
            );
            return Ok(());
        }

        let batch_size = batch.len();
        let source_ids: Vec<Uuid> = batch.iter().map(|e| e.id).collect();

        // Capture date range for fallback summary cause.
        let start_date = batch
            .first()
            .map(|e| e.created_at.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        let end_date = batch
            .last()
            .map(|e| e.created_at.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        tracing::info!(
            batch_size,
            start_date = %start_date,
            end_date = %end_date,
            "starting summarization of {} engrams",
            batch_size
        );

        // --- Step 3: Build the summarization prompt ---
        let user_prompt = build_user_prompt(&batch);

        // --- Step 4: Call Odin for summarization ---
        let (summary_cause, summary_effect) =
            self.call_odin_summarize(&user_prompt, batch_size, &start_date, &end_date)
                .await?;

        // --- Step 5: Compute content hash and embedding for the summary ---
        let content_hash = crate::handlers::engram_content_hash(&summary_cause, &summary_effect);

        // OnnxEmbedder::embed is synchronous — run on the blocking thread pool.
        let embedder = self.embedder.clone();
        let cause_text = summary_cause.clone();
        let embedding: Vec<f32> = tokio::task::spawn_blocking(move || {
            embedder.embed(&cause_text)
        })
        .await
        .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))??;

        // Tags: "auto-summary" + a batch marker using the first source ID's short form.
        let batch_tag = format!("batch-{}", &source_ids[0].to_string()[..8]);
        let tags = vec!["auto-summary".to_string(), batch_tag];

        // --- Step 6: Insert archival engram in PostgreSQL ---
        // Binarize the summary embedding into an SDR for BYTEA storage.
        let sdr = crate::sdr::binarize(&embedding[..crate::sdr::SDR_BITS]);
        let sdr_bytes = crate::sdr::to_bytes(&sdr);
        // Trigger label: first 80 chars of summary cause
        let label_len = summary_cause.len().min(80);
        let trigger_label = summary_cause[..label_len].to_string();

        // On duplicate content hash (extremely unlikely), log a warning and leave the
        // source engrams in Recall tier — they will be candidates again next cycle.
        let summary_id = match engrams::insert_engram_sdr(
            pool,
            &engrams::EngramSdrParams {
                cause: &summary_cause,
                effect: &summary_effect,
                sdr_bits: &sdr_bytes,
                content_hash: &content_hash,
                tags: &tags,
                trigger_type: "pattern",
                trigger_label: &trigger_label,
                project: None,  // summaries inherit global scope for now
                scope: "global",
            },
            ygg_domain::engram::MemoryTier::Archival,
        )
        .await
        {
            Ok(id) => id,
            Err(ygg_store::error::StoreError::Duplicate(msg)) => {
                tracing::warn!(
                    error = %msg,
                    "summary content hash collision, skipping this batch"
                );
                return Ok(());
            }
            Err(e) => return Err(MimirError::Store(e)),
        };

        tracing::info!(
            summary_id = %summary_id,
            source_count = batch_size,
            "archival engram created"
        );

        // --- Step 7: Upsert summary SDR into Qdrant engrams_sdr collection ---
        // Convert the packed SDR to bipolar {-1.0, 1.0} for Qdrant Dot product.
        // Bipolar mapping makes Dot product rank-equivalent to Hamming distance.
        let sdr_f32 = crate::sdr::to_bipolar_f32(&sdr);
        self.vectors
            .upsert(
                "engrams_sdr",
                summary_id,
                sdr_f32,
                std::collections::HashMap::new(),
            )
            .await?;

        // --- Step 8: Mark source engrams as archived in PostgreSQL ---
        engrams::archive_engrams(pool, &source_ids, summary_id).await?;

        // --- Step 9: Delete source engram vectors from Qdrant engrams_sdr collection ---
        // Archived engrams are removed from the SDR search index to prevent stale recall.
        self.vectors.delete_many("engrams_sdr", &source_ids).await?;

        tracing::info!(
            summary_id = %summary_id,
            archived_count = batch_size,
            "summarized {} engrams into archival engram {}",
            batch_size,
            summary_id
        );

        Ok(())
    }

    /// POST to Odin `POST /v1/chat/completions` and parse the summary response.
    ///
    /// On HTTP error or JSON parse failure, returns `MimirError::Summarization`.
    /// The fallback cause is constructed from the batch metadata when the model
    /// fails to produce valid JSON.
    ///
    /// Timeout: 120s (configured on the reqwest client at construction time).
    async fn call_odin_summarize(
        &self,
        user_prompt: &str,
        batch_size: usize,
        start_date: &str,
        end_date: &str,
    ) -> Result<(String, String), MimirError> {
        let url = format!("{}/v1/chat/completions", self.config.odin_url);

        let request_body = ChatCompletionRequest {
            model: None,
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: SYSTEM_PROMPT.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_prompt.to_string(),
                },
            ],
            stream: false,
            max_tokens: 2048,
            temperature: 0.3,
        };

        let response = self
            .http
            .post(&url)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| {
                MimirError::Summarization(format!("odin request failed: {e}"))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(MimirError::Summarization(format!(
                "odin returned {status}: {body}"
            )));
        }

        let completion: ChatCompletionResponse = response.json().await.map_err(|e| {
            MimirError::Summarization(format!("failed to parse odin response: {e}"))
        })?;

        let content = completion
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        // --- Parse {cause, effect} JSON from model output ---
        // The model may wrap the JSON in a markdown code fence. Strip it first.
        let clean = strip_code_fence(&content);

        match serde_json::from_str::<SummaryJson>(clean) {
            Ok(summary) => {
                tracing::debug!("successfully parsed summary json from odin");
                Ok((summary.cause, summary.effect))
            }
            Err(parse_err) => {
                // Fallback: use entire response content as effect, construct a generic cause.
                tracing::warn!(
                    error = %parse_err,
                    "odin response was not valid summary json, using fallback"
                );
                let fallback_cause = format!(
                    "Consolidated summary of {batch_size} memories from {start_date} to {end_date}"
                );
                Ok((fallback_cause, content))
            }
        }
    }
}

/// Build the user prompt from a batch of engrams.
fn build_user_prompt(batch: &[ygg_domain::engram::Engram]) -> String {
    let count = batch.len();
    let mut parts = Vec::with_capacity(count + 1);
    parts.push(format!("Consolidate these {count} memories into a single summary:\n"));

    for (i, engram) in batch.iter().enumerate() {
        parts.push(format!(
            "{}. Cause: {}\n   Effect: {}\n   Created: {}\n",
            i + 1,
            engram.cause,
            engram.effect,
            engram.created_at.format("%Y-%m-%dT%H:%M:%SZ"),
        ));
    }

    parts.join("\n")
}

/// Strip markdown code fences from model output.
///
/// Models sometimes wrap JSON in triple-backtick fences:
/// ```json
/// {"cause": "...", "effect": "..."}
/// ```
///
/// This function strips the fences and returns only the inner content trimmed.
fn strip_code_fence(s: &str) -> &str {
    let trimmed = s.trim();
    // Look for opening fence (```json or ```)
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip optional language specifier (e.g. "json\n")
        let after_lang = rest
            .find('\n')
            .map(|pos| &rest[pos + 1..])
            .unwrap_or(rest);
        // Strip closing fence
        if let Some(inner) = after_lang.strip_suffix("```") {
            return inner.trim();
        }
        return after_lang.trim();
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_code_fence_removes_json_fence() {
        let input = "```json\n{\"cause\": \"a\", \"effect\": \"b\"}\n```";
        assert_eq!(strip_code_fence(input), "{\"cause\": \"a\", \"effect\": \"b\"}");
    }

    #[test]
    fn strip_code_fence_no_fence_unchanged() {
        let input = "{\"cause\": \"a\", \"effect\": \"b\"}";
        assert_eq!(strip_code_fence(input), input);
    }

    #[test]
    fn strip_code_fence_bare_fence() {
        let input = "```\n{\"cause\": \"a\", \"effect\": \"b\"}\n```";
        assert_eq!(strip_code_fence(input), "{\"cause\": \"a\", \"effect\": \"b\"}");
    }
}
