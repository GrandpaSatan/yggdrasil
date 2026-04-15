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

use std::sync::Arc;

use tokio::sync::watch;
use uuid::Uuid;

use ygg_domain::config::TierConfig;
use ygg_store::{Store, qdrant::VectorStore};
use ygg_store::postgres::engrams;

use ygg_embed::OnnxEmbedder;

use crate::dense_index::DenseIndex;
use crate::error::MimirError;
use crate::sdr_index::SdrIndex;

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

/// Row type for consolidation cycle queries.
#[derive(sqlx::FromRow)]
#[allow(dead_code)]
struct ConsolidationRow {
    id: Uuid,
    cause: String,
    effect: String,
    sdr_bits: Vec<u8>,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Report returned by `SummarizationService::force_cycle` — used by the
/// `/api/v1/summarize/trigger` endpoint so E2E tests can assert what happened.
#[derive(Debug, serde::Serialize)]
pub struct SummaryReport {
    pub summarized_batches: u32,
    pub archived_engrams: u32,
    pub deduped: u32,
    pub contradictions: u32,
}

/// Background service for summarizing aging Recall engrams into Archival summaries.
///
/// Sprint 069 Phase D: `shutdown_rx` moved out of the struct to a `run()` parameter
/// so the service can be held as `Arc<SummarizationService>` in `AppState` and
/// reused by the `/api/v1/summarize/trigger` handler on demand.
pub struct SummarizationService {
    store: Store,
    vectors: VectorStore,
    sdr_index: Arc<SdrIndex>,
    dense_index: Arc<DenseIndex>,
    embedder: OnnxEmbedder,
    http: reqwest::Client,
    config: TierConfig,
}

impl SummarizationService {
    /// Create a new `SummarizationService`.
    ///
    /// `sdr_index` and `dense_index` are held so consolidation can drop deleted
    /// engram IDs from both in-memory indexes — the dreamer 404 bug (Sprint 067
    /// Phase 4a) stemmed from consolidation deleting Postgres rows without
    /// invalidating the stale SDR entries, which then mis-routed later novelty
    /// verdicts to `Update` for rows that no longer exist.
    pub fn new(
        store: Store,
        vectors: VectorStore,
        sdr_index: Arc<SdrIndex>,
        dense_index: Arc<DenseIndex>,
        embedder: OnnxEmbedder,
        config: TierConfig,
    ) -> Self {
        // Sprint 069 Phase C: Odin is now gated by `ygg_server::auth::bearer_auth`.
        // Set the in-fleet internal-trust header on every summarization call so
        // Odin's auth layer lets the request through on the fast path.
        let mut default_headers = reqwest::header::HeaderMap::new();
        default_headers.insert(
            "X-Yggdrasil-Internal",
            reqwest::header::HeaderValue::from_static("true"),
        );

        // Timeout of 120s matches the sprint doc requirement for Odin summarization calls.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .default_headers(default_headers)
            .build()
            // reqwest::Client::builder().build() only fails on TLS init, which would
            // be a fatal misconfiguration. Use expect here at service construction time.
            .expect("failed to build reqwest client for summarization service");

        Self {
            store,
            vectors,
            sdr_index,
            dense_index,
            embedder,
            http,
            config,
        }
    }

    /// Spawn the background summarization task.
    ///
    /// Returns a `JoinHandle` — the caller can drop it (fire-and-forget) or await it
    /// for clean shutdown testing.
    pub fn start(
        self: Arc<Self>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run(shutdown_rx).await;
        })
    }

    /// Force one summarization + consolidation cycle, bypassing the recall-capacity
    /// gate. Used by the `/api/v1/summarize/trigger` endpoint so E2E tests can
    /// verify the archive-and-delete flow without having to breach capacity.
    ///
    /// Returns a `SummaryReport` describing what the cycle did. On internal
    /// failure (PG / Qdrant / Odin unreachable), returns `Err` — the caller
    /// decides whether to surface the error to the client.
    pub async fn force_cycle(&self) -> Result<SummaryReport, MimirError> {
        let (summarized_batches, archived_engrams) =
            self.check_and_summarize_inner(true).await?;
        let (deduped, contradictions) = self.consolidate_cycle_inner().await?;
        Ok(SummaryReport {
            summarized_batches,
            archived_engrams,
            deduped,
            contradictions,
        })
    }

    /// Force-archive a single engram by ID — the lightweight test path.
    ///
    /// Skips the LLM summarization entirely and just performs the
    /// archive-and-invalidate step that consolidate_cycle does for dedup
    /// matches: delete the row from PG, drop the vectors from Qdrant, and
    /// invalidate both in-memory indexes (the Sprint 067 Phase 4a coherence
    /// fix). This is what the dreamer-coherence E2E test actually wants —
    /// a deterministic "this specific engram is now gone" without the
    /// 60–120s of LLM RTT a full summarization batch incurs.
    pub async fn force_archive_engram(&self, id: Uuid) -> Result<SummaryReport, MimirError> {
        let pool = self.store.pool();
        // Confirm the row exists before doing anything destructive.
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM yggdrasil.engrams WHERE id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|e| MimirError::Internal(format!("count query failed: {e}")))?;
        if exists == 0 {
            return Err(MimirError::NotFound(format!(
                "engram {id} not found — cannot force-archive"
            )));
        }
        let _ = engrams::delete_engram(pool, id).await;
        self.vectors.delete_many("engrams_sdr", &[id]).await.ok();
        self.sdr_index.remove(id);
        self.dense_index.remove(id);
        tracing::info!(engram_id = %id, "force-archived single engram via trigger endpoint");
        Ok(SummaryReport {
            summarized_batches: 0,
            archived_engrams: 1,
            deduped: 0,
            contradictions: 0,
        })
    }

    /// Main loop: sleep for `check_interval`, then run summarization + consolidation.
    /// Exits cleanly when the shutdown signal is received.
    async fn run(self: Arc<Self>, mut shutdown_rx: watch::Receiver<bool>) {
        let interval = std::time::Duration::from_secs(self.config.check_interval_secs);
        tracing::info!(
            check_interval_secs = self.config.check_interval_secs,
            recall_capacity = self.config.recall_capacity,
            batch_size = self.config.summarization_batch_size,
            odin_url = %self.config.odin_url,
            "summarization + consolidation service started"
        );

        loop {
            tokio::select! {
                // Wait for the check interval before each cycle.
                _ = tokio::time::sleep(interval) => {
                    if let Err(e) = self.check_and_summarize_inner(false).await {
                        tracing::warn!(error = %e, "summarization cycle failed, will retry next interval");
                    }
                    // Sprint 055: Run consolidation cycle after summarization.
                    if let Err(e) = self.consolidate_cycle_inner().await {
                        tracing::warn!(error = %e, "consolidation cycle failed, will retry next interval");
                    }
                }
                // Graceful shutdown: wait for the shutdown signal to become true.
                result = shutdown_rx.changed() => {
                    if result.is_ok() && *shutdown_rx.borrow() {
                        tracing::info!("summarization service stopped");
                        break;
                    }
                }
            }
        }
    }

    /// Sprint 055: Consolidation cycle — dedup near-duplicates and detect contradictions.
    ///
    /// Scans recent Recall-tier engrams for:
    /// 1. Near-duplicates (same content hash or SDR similarity > 0.95) → merge into one
    /// 2. Contradictions (SDR similarity > 0.85, low word overlap) → mark older as superseded
    ///
    /// Returns `(deduped_count, contradictions_count)` so the force-trigger path
    /// can surface totals to the client. Runs as part of the background sleep
    /// cycle AND on demand via `force_cycle`.
    async fn consolidate_cycle_inner(&self) -> Result<(u32, u32), MimirError> {
        let pool = self.store.pool();

        // Fetch recent recall engrams (last 200, ordered by created_at desc)
        let recent = sqlx::query_as::<_, ConsolidationRow>(
            r#"
            SELECT id, cause, effect, sdr_bits, created_at
            FROM yggdrasil.engrams
            WHERE tier = 'recall' AND sdr_bits IS NOT NULL
            ORDER BY created_at DESC
            LIMIT 200
            "#,
        )
        .fetch_all(pool)
        .await
        .map_err(|e| MimirError::Internal(format!("consolidation query failed: {e}")))?;

        if recent.len() < 2 {
            return Ok((0, 0));
        }

        let mut dedup_count = 0u32;
        let mut contradiction_count = 0u32;

        // Compare each pair (only adjacent in similarity space to keep O(n) not O(n^2))
        // We use a simple sliding window: for each engram, compare against the next 5.
        for i in 0..recent.len().saturating_sub(1) {
            let a = &recent[i];
            if a.sdr_bits.len() < crate::sdr::SDR_WORDS * 8 {
                continue;
            }
            let a_sdr = crate::sdr::from_bytes(&a.sdr_bits);

            for j in (i + 1)..recent.len().min(i + 6) {
                let b = &recent[j];
                if b.sdr_bits.len() < crate::sdr::SDR_WORDS * 8 {
                    continue;
                }
                let b_sdr = crate::sdr::from_bytes(&b.sdr_bits);

                let sim = crate::sdr::hamming_similarity(&a_sdr, &b_sdr);

                if sim > 0.95 {
                    // Near-duplicate: delete the older one (b is older since sorted desc)
                    tracing::info!(
                        kept = %a.id,
                        deleted = %b.id,
                        similarity = %sim,
                        "consolidation: dedup near-duplicate"
                    );
                    let _ = engrams::delete_engram(pool, b.id).await;
                    self.vectors.delete_many("engrams_sdr", &[b.id]).await.ok();
                    // Sprint 067 Phase 4a: invalidate both in-memory indexes so a
                    // later store does not resurrect a stale nearest-match pointing
                    // at a Postgres row that was just deleted — the root cause of
                    // the dreamer "engram store returned 404" burst logged on
                    // yggdrasil-dreamer.service. Pairs with the Postgres + Qdrant
                    // deletes above to keep SDR + dense views coherent with the
                    // authoritative store.
                    self.sdr_index.remove(b.id);
                    self.dense_index.remove(b.id);
                    dedup_count += 1;
                } else if sim > 0.85 {
                    // High similarity — check for contradiction (divergent effect text)
                    let a_words: std::collections::HashSet<&str> =
                        a.effect.split_whitespace().collect();
                    let b_words: std::collections::HashSet<&str> =
                        b.effect.split_whitespace().collect();
                    let intersection = a_words.intersection(&b_words).count();
                    let union = a_words.union(&b_words).count();
                    let jaccard = if union > 0 { intersection as f64 / union as f64 } else { 1.0 };

                    if jaccard < 0.5 {
                        // Contradiction detected: tag the older one as superseded
                        tracing::info!(
                            newer = %a.id,
                            older = %b.id,
                            similarity = %sim,
                            jaccard = %jaccard,
                            "consolidation: contradiction detected, tagging older as superseded"
                        );
                        let _ = sqlx::query(
                            "UPDATE yggdrasil.engrams SET tags = array_append(tags, 'superseded') WHERE id = $1 AND NOT ('superseded' = ANY(tags))"
                        )
                        .bind(b.id)
                        .execute(pool)
                        .await;
                        contradiction_count += 1;
                    }
                }
            }
        }

        if dedup_count > 0 || contradiction_count > 0 {
            tracing::info!(
                dedup_count,
                contradiction_count,
                "consolidation cycle complete"
            );
        }

        Ok((dedup_count, contradiction_count))
    }

    /// Run one summarization cycle.
    ///
    /// Returns `(summarized_batches, archived_engrams)`. When `bypass_capacity`
    /// is `true` the recall-capacity gate is skipped and one batch is summarized
    /// unconditionally — used by the `/api/v1/summarize/trigger` endpoint so
    /// tests can force an archive-and-delete without having to breach capacity.
    /// Returns `Err` only on hard failures that should be logged by the caller.
    async fn check_and_summarize_inner(
        &self,
        bypass_capacity: bool,
    ) -> Result<(u32, u32), MimirError> {
        let pool = self.store.pool();

        // --- Step 1: Check if Recall tier is over capacity ---
        let stats = engrams::get_stats(pool).await?;
        let recall_count = stats.recall_count;
        let recall_capacity = self.config.recall_capacity as i64;

        if !bypass_capacity && recall_count <= recall_capacity {
            tracing::debug!(
                recall_count,
                recall_capacity,
                "recall tier within capacity, skipping summarization"
            );
            return Ok((0, 0));
        }

        tracing::info!(
            recall_count,
            recall_capacity,
            "recall tier over capacity, starting summarization"
        );

        // --- Step 2: Fetch oldest/least-accessed batch eligible for summarization ---
        // Sprint 069 Phase D: when called from the force-trigger path
        // (bypass_capacity=true), ignore `min_age_secs` so freshly-stored test
        // engrams are eligible. The background loop keeps the configured age
        // floor so live consolidation respects the safety window.
        let effective_min_age = if bypass_capacity { 0 } else { self.config.min_age_secs };
        let batch = engrams::get_oldest_recall_engrams(
            pool,
            self.config.summarization_batch_size,
            effective_min_age,
        )
        .await?;

        if batch.is_empty() {
            tracing::info!(
                min_age_secs = self.config.min_age_secs,
                "no recall engrams old enough for summarization, skipping"
            );
            return Ok((0, 0));
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
                return Ok((0, 0));
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

        // Sprint 069 Phase D: when the caller bypassed the capacity gate (i.e.
        // the /api/v1/summarize/trigger path), also delete the source Postgres
        // rows. Rationale: the force-trigger contract is "archive AND retire"
        // — tests assert the source rows are gone afterwards, and dreamers
        // rely on archived rows disappearing from the SDR/dense indexes.
        // The background cycle keeps the tier='archival' rows for traceability.
        if bypass_capacity {
            for sid in &source_ids {
                let _ = engrams::delete_engram(pool, *sid).await;
                self.sdr_index.remove(*sid);
                self.dense_index.remove(*sid);
            }
        }

        tracing::info!(
            summary_id = %summary_id,
            archived_count = batch_size,
            "summarized {} engrams into archival engram {}",
            batch_size,
            summary_id
        );

        Ok((1, batch_size as u32))
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
