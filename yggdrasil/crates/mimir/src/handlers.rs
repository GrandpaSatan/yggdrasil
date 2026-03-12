use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use ygg_domain::engram::{
    EngramEvent, EngramQuery, EngramTrigger, MemoryTier, NewEngram, RecallQuery, RecallResponse,
};
use ygg_store::postgres::engrams;

use crate::{error::MimirError, sdr, state::AppState};

// ---------------------------------------------------------------------------
// GET /health
// ---------------------------------------------------------------------------

/// Health check endpoint.  No database call — must return 200 in < 2ms.
pub async fn health() -> StatusCode {
    StatusCode::OK
}

// ---------------------------------------------------------------------------
// POST /api/v1/store
// ---------------------------------------------------------------------------

/// Store a new cause-effect engram using ONNX SDR encoding.
///
/// Pipeline:
/// 1. Validate inputs
/// 2. SHA-256 content hash
/// 3. ONNX embed (spawn_blocking) → 384-dim float vector
/// 4. binarize first 256 dims → 256-bit SDR
/// 5. Determine trigger type from tags
/// 6. Extract trigger label from cause text
/// 7. Insert into PostgreSQL (insert_engram_sdr)
/// 8. Insert into in-memory SDR index
/// 9. Upsert to Qdrant engrams_sdr collection
/// 10. Return 201 { "id": id }
pub async fn store_engram(
    State(state): State<Arc<AppState>>,
    Json(body): Json<NewEngram>,
) -> Result<(StatusCode, Json<serde_json::Value>), MimirError> {
    // Step 1: Validate
    if body.cause.trim().is_empty() {
        return Err(MimirError::Validation("cause must not be empty".into()));
    }
    if body.effect.trim().is_empty() {
        return Err(MimirError::Validation("effect must not be empty".into()));
    }

    // Step 2: SHA-256 content hash
    let content_hash: Vec<u8> = {
        let mut hasher = Sha256::new();
        hasher.update(body.cause.as_bytes());
        hasher.update(b"\n");
        hasher.update(body.effect.as_bytes());
        hasher.finalize().to_vec()
    };

    // Step 3: Embed cause text via ONNX (sync — must use spawn_blocking)
    let embedder = state.embedder.clone();
    let cause_text = body.cause.clone();
    let embedding: Vec<f32> =
        tokio::task::spawn_blocking(move || embedder.embed(&cause_text))
            .await
            .map_err(|e| MimirError::Embedder(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Embedder(e.to_string()))?;

    // Step 4: Binarize → 256-bit SDR (uses first 256 dims of the 384-dim vector)
    let sdr_val = sdr::binarize(&embedding[..sdr::SDR_BITS]);
    let sdr_bytes = sdr::to_bytes(&sdr_val);

    // Step 5: Determine trigger type from tags
    let tags_lower: Vec<String> = body.tags.iter().map(|t| t.to_lowercase()).collect();
    let trigger_type = if tags_lower.iter().any(|t| t == "fact" || t == "core") {
        "fact"
    } else if tags_lower.iter().any(|t| t == "decision") {
        "decision"
    } else {
        "pattern"
    };

    // Step 6: Extract trigger label — first 80 chars of cause, trimmed to word boundary
    let trigger_label = truncate_to_word_boundary(body.cause.trim(), 80);

    // Step 7: Update-by-ID path vs new insert path
    if let Some(existing_id) = body.id {
        // Update path: caller knows the engram ID, bypass novelty gate
        let updated = engrams::update_engram_sdr(
            state.store.pool(),
            existing_id,
            &body.cause,
            &body.effect,
            &sdr_bytes,
            &content_hash,
            &body.tags,
            trigger_type,
            &trigger_label,
        )
        .await?;

        if !updated {
            return Err(MimirError::NotFound(format!(
                "engram {} not found for update",
                existing_id
            )));
        }

        tracing::info!(engram_id = %existing_id, trigger_type, "engram updated by ID");

        // Update in-memory SDR index (remove old, insert new)
        state.sdr_index.remove(existing_id);
        state.sdr_index.insert(existing_id, sdr_val);

        // Update Qdrant
        let sdr_f32 = sdr::to_f32_vec(&sdr_val);
        state
            .vectors
            .upsert("engrams_sdr", existing_id, sdr_f32, HashMap::new())
            .await?;

        return Ok((StatusCode::OK, Json(serde_json::json!({ "id": existing_id, "updated": true }))));
    }

    // Step 4b: Novelty gate — reject near-duplicates by SDR similarity (new inserts only)
    let dedup_threshold = state.config.sdr.dedup_threshold;
    if dedup_threshold < 1.0 {
        let nearest = state.sdr_index.query(&sdr_val, 1);
        if let Some((dup_id, sim)) = nearest.first() {
            if *sim >= dedup_threshold {
                tracing::info!(
                    duplicate_id = %dup_id,
                    similarity = %sim,
                    threshold = %dedup_threshold,
                    "engram rejected by novelty gate"
                );
                return Ok((
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "near-duplicate detected",
                        "duplicate_id": dup_id,
                        "similarity": sim
                    })),
                ));
            }
        }
    }

    // Step 8: Insert into PostgreSQL
    let id = engrams::insert_engram_sdr(
        state.store.pool(),
        &body.cause,
        &body.effect,
        &sdr_bytes,
        &content_hash,
        MemoryTier::Recall,
        &body.tags,
        trigger_type,
        &trigger_label,
    )
    .await?;

    tracing::info!(engram_id = %id, trigger_type, "engram stored via SDR");

    // Step 9: Insert into in-memory SDR index
    state.sdr_index.insert(id, sdr_val);

    // Step 10: Upsert into Qdrant engrams_sdr collection
    let sdr_f32 = sdr::to_f32_vec(&sdr_val);
    state
        .vectors
        .upsert("engrams_sdr", id, sdr_f32, HashMap::new())
        .await?;

    // Step 11: Return 201 Created
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

// ---------------------------------------------------------------------------
// POST /api/v1/query  (backward-compat with Fergus engram_client.rs)
// ---------------------------------------------------------------------------

/// Query engrams by semantic similarity using SDR dual-system recall.
///
/// Backward-compatible with the Fergus `engram_client.rs` API contract.
/// Embeds via ONNX, then uses Hamming + Qdrant SDR search (same as recall),
/// but returns full `Engram` objects (with cause/effect text) instead of events.
pub async fn query_engrams(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EngramQuery>,
) -> Result<(StatusCode, Json<serde_json::Value>), MimirError> {
    if body.text.trim().is_empty() {
        return Err(MimirError::Validation("text must not be empty".into()));
    }

    // Step 1: Embed via ONNX → binarize → SDR
    let embedder = state.embedder.clone();
    let query_text = body.text.clone();
    let embedding: Vec<f32> =
        tokio::task::spawn_blocking(move || embedder.embed(&query_text))
            .await
            .map_err(|e| MimirError::Embedder(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Embedder(e.to_string()))?;

    let query_sdr = sdr::binarize(&embedding[..sdr::SDR_BITS]);
    let sdr_f32 = sdr::to_f32_vec(&query_sdr);

    // Step 2: Dual-system search (Hamming + Qdrant) in parallel
    let limit = body.limit;
    let (sys1_results, sys2_results) = tokio::join!(
        async { Ok::<_, MimirError>(state.sdr_index.query(&query_sdr, limit)) },
        state.vectors.search("engrams_sdr", sdr_f32, limit as u64)
    );

    let sys1 = sys1_results?;
    let sys2 = sys2_results?;

    // Step 3: Merge by UUID — take max similarity
    let mut merged: HashMap<Uuid, f64> = HashMap::new();
    for (id, sim) in sys1 {
        merged.insert(id, sim);
    }
    for (id, score) in sys2 {
        let score_f64 = score as f64;
        merged
            .entry(id)
            .and_modify(|s| {
                if score_f64 > *s {
                    *s = score_f64;
                }
            })
            .or_insert(score_f64);
    }

    // Sort by similarity descending, truncate to limit
    let mut ranked: Vec<(Uuid, f64)> = merged.into_iter().collect();
    ranked.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);

    if ranked.is_empty() {
        return Ok((StatusCode::OK, Json(serde_json::json!({ "results": [] }))));
    }

    // Step 4: Fetch full engram data from PostgreSQL
    let sim_map: HashMap<Uuid, f64> = ranked.iter().cloned().collect();
    let mut results = Vec::with_capacity(ranked.len());
    for (id, _) in &ranked {
        match engrams::get_engram(state.store.pool(), *id).await {
            Ok(mut engram) => {
                engram.similarity = sim_map.get(id).copied().unwrap_or(0.0);
                results.push(engram);
            }
            Err(e) => {
                tracing::warn!(engram_id = %id, error = %e, "skipping engram in query results");
            }
        }
    }

    // Fire-and-forget access count bump
    let result_ids: Vec<Uuid> = results.iter().map(|e| e.id).collect();
    if !result_ids.is_empty() {
        let pool = state.store.pool().clone();
        tokio::spawn(async move {
            if let Err(e) = bump_access_counts(&pool, &result_ids).await {
                tracing::warn!(error = %e, "failed to bump access counts");
            }
        });
    }

    Ok((StatusCode::OK, Json(serde_json::json!({ "results": results }))))
}

// ---------------------------------------------------------------------------
// POST /api/v1/recall  (Sprint 015 primary endpoint)
// ---------------------------------------------------------------------------

/// Recall engrams using dual-system SDR matching.
///
/// System 1 (fast): In-memory Hamming scan via SdrIndex.
/// System 2 (semantic): Qdrant dot-product search on the engrams_sdr collection.
///
/// Results are merged by UUID, taking the highest similarity score when both
/// systems return the same engram, then ranked and truncated to `limit`.
pub async fn recall_engrams(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RecallQuery>,
) -> Result<impl IntoResponse, MimirError> {
    // Step 1: Validate
    if body.text.trim().is_empty() {
        return Err(MimirError::Validation("text must not be empty".into()));
    }

    // Step 2: Embed → binarize → SDR
    let embedder = state.embedder.clone();
    let query_text = body.text.clone();
    let embedding: Vec<f32> =
        tokio::task::spawn_blocking(move || embedder.embed(&query_text))
            .await
            .map_err(|e| MimirError::Embedder(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Embedder(e.to_string()))?;

    let query_sdr = sdr::binarize(&embedding[..sdr::SDR_BITS]);
    let sdr_f32 = sdr::to_f32_vec(&query_sdr);

    // Steps 3 & 4: Run System 1 (in-memory Hamming) and System 2 (Qdrant) in parallel
    let limit = body.limit;
    let (sys1_results, sys2_results) = tokio::join!(
        // System 1: brute-force Hamming scan (sub-millisecond for < 50k entries)
        async { Ok::<_, MimirError>(state.sdr_index.query(&query_sdr, limit)) },
        // System 2: Qdrant dot-product search on engrams_sdr collection
        state.vectors.search("engrams_sdr", sdr_f32, limit as u64)
    );

    let sys1 = sys1_results?;
    let sys2 = sys2_results?;

    // Step 5: Merge by UUID — take max similarity when both systems return same ID.
    //
    // System 1 returns normalized Hamming similarity in [0.0, 1.0].
    // System 2 returns raw Qdrant dot-product scores (0 to ~popcount for binary vectors).
    // Normalize System 2 by dividing by query popcount so both are in [0.0, 1.0].
    // For ~50% density SDRs: hamming_similarity ≈ dot / popcount.
    let query_pop = sdr::popcount(&query_sdr) as f64;
    let normalizer = if query_pop > 0.0 { query_pop } else { 1.0 };

    let mut merged: HashMap<Uuid, f64> = HashMap::new();
    for (id, sim) in sys1 {
        merged.insert(id, sim);
    }
    for (id, score) in sys2 {
        let normalized = (score as f64 / normalizer).clamp(0.0, 1.0);
        merged
            .entry(id)
            .and_modify(|s| {
                if normalized > *s {
                    *s = normalized;
                }
            })
            .or_insert(normalized);
    }

    // Sort by similarity descending, truncate to limit
    let mut ranked: Vec<(Uuid, f64)> = merged.into_iter().collect();
    ranked.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);

    // Step 6: Fetch metadata from PostgreSQL
    let result_ids: Vec<Uuid> = ranked.iter().map(|(id, _)| *id).collect();
    let sim_map: HashMap<Uuid, f64> = ranked.into_iter().collect();

    let event_rows = engrams::fetch_engram_events(state.store.pool(), &result_ids).await?;

    // Step 7: Build EngramEvent list
    let events: Vec<EngramEvent> = event_rows
        .into_iter()
        .map(|(id, tier_str, tags, trigger_type, trigger_label, created_at, access_count)| {
            let similarity = sim_map.get(&id).copied().unwrap_or(0.0);
            EngramEvent {
                id,
                similarity,
                tier: parse_tier(&tier_str),
                tags: tags.clone(),
                trigger: build_trigger(&trigger_type, trigger_label, &tags),
                created_at,
                access_count,
            }
        })
        .collect();

    // Step 8: Fetch core engrams as events
    let core_rows = engrams::get_core_engram_events(state.store.pool()).await?;
    let core_events: Vec<EngramEvent> = core_rows
        .into_iter()
        .map(|(id, tags, trigger_type, trigger_label, created_at, access_count)| EngramEvent {
            id,
            // Core engrams are always included — similarity is a marker value
            similarity: 1.0,
            tier: MemoryTier::Core,
            tags: tags.clone(),
            trigger: build_trigger(&trigger_type, trigger_label, &tags),
            created_at,
            access_count,
        })
        .collect();

    // Step 9: Fire-and-forget access count bump for recalled engrams
    if !result_ids.is_empty() {
        let pool = state.store.pool().clone();
        let ids = result_ids.clone();
        tokio::spawn(async move {
            if let Err(e) = bump_access_counts(&pool, &ids).await {
                tracing::warn!(error = %e, "failed to bump access counts");
            }
        });
    }

    // Step 10: Return RecallResponse with query SDR for drift tracking
    let query_sdr_hex = Some(
        sdr::to_bytes(&query_sdr)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    );
    let response = RecallResponse {
        events,
        core_events,
        query_sdr_hex,
    };
    Ok((StatusCode::OK, Json(response)))
}

// ---------------------------------------------------------------------------
// GET /api/v1/stats
// ---------------------------------------------------------------------------

/// Return tier counts and capacity from PostgreSQL and config.
///
/// The store layer returns `recall_capacity = 0` as a placeholder; this handler
/// injects the actual configured value from `TierConfig`.
pub async fn get_stats(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, MimirError> {
    let mut stats = engrams::get_stats(state.store.pool()).await?;
    stats.recall_capacity = state.config.tiers.recall_capacity as i64;
    Ok((StatusCode::OK, Json(stats)))
}

// ---------------------------------------------------------------------------
// GET /api/v1/core
// ---------------------------------------------------------------------------

/// Return all Core tier engrams (full Engram structs, not events).
///
/// Core engrams are permanent context always prepended to query results. This endpoint
/// allows callers to inspect them directly. No pagination — Core tier is expected to
/// remain small (< 50 engrams by design).
pub async fn get_core_engrams_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, MimirError> {
    let core = engrams::get_core_engrams(state.store.pool()).await?;
    Ok((StatusCode::OK, Json(core)))
}

// ---------------------------------------------------------------------------
// POST /api/v1/promote
// ---------------------------------------------------------------------------

/// Promote (or demote) an engram to a different memory tier.
#[derive(Debug, Deserialize, Serialize)]
pub struct PromoteRequest {
    pub id: Uuid,
    pub tier: MemoryTier,
}

pub async fn promote_engram(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PromoteRequest>,
) -> Result<impl IntoResponse, MimirError> {
    engrams::set_tier(state.store.pool(), body.id, body.tier).await?;
    tracing::info!(engram_id = %body.id, tier = %body.tier, "engram tier promoted");
    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// POST /api/v1/sdr/operations
// ---------------------------------------------------------------------------

/// SDR set operation type.
#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum SdrOperation {
    And,
    Or,
    Xor,
}

/// Request body for SDR operations endpoint.
#[derive(Debug, Deserialize)]
pub struct SdrOperationRequest {
    /// Two or more texts to embed and combine with the specified operation.
    pub texts: Vec<String>,
    /// Bitwise operation to apply across the SDRs.
    pub operation: SdrOperation,
    /// Maximum number of matching engrams to return (default 5).
    #[serde(default = "default_sdr_op_limit")]
    pub limit: usize,
}

fn default_sdr_op_limit() -> usize {
    5
}

/// Response from SDR operations endpoint.
#[derive(Debug, Serialize)]
pub struct SdrOperationResponse {
    /// Jaccard similarity between the input SDRs (before applying operation to index).
    pub jaccard: f64,
    /// Number of set bits in the combined SDR.
    pub combined_popcount: u32,
    /// Matching engram events ranked by similarity to the combined SDR.
    pub events: Vec<EngramEvent>,
}

/// Combine two or more texts via SDR set operations and query the index.
///
/// Pipeline:
/// 1. Embed each text → binarize → SDR
/// 2. Fold SDRs with the specified bitwise operation
/// 3. Compute Jaccard similarity between first two SDRs (diagnostic)
/// 4. Query SdrIndex with the combined SDR
/// 5. Fetch metadata, return events
pub async fn sdr_operations(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SdrOperationRequest>,
) -> Result<impl IntoResponse, MimirError> {
    if body.texts.len() < 2 {
        return Err(MimirError::Validation(
            "at least 2 texts required".into(),
        ));
    }
    for (i, t) in body.texts.iter().enumerate() {
        if t.trim().is_empty() {
            return Err(MimirError::Validation(format!(
                "text at index {i} must not be empty"
            )));
        }
    }

    // Step 1: Embed all texts in a batch (embed_batch is already async)
    let embeddings: Vec<Vec<f32>> = state
        .embedder
        .embed_batch(&body.texts)
        .await
        .map_err(|e| MimirError::Embedder(e.to_string()))?;

    // Step 2: Binarize each embedding → SDR
    let sdrs: Vec<sdr::Sdr> = embeddings
        .iter()
        .map(|emb| sdr::binarize(&emb[..sdr::SDR_BITS]))
        .collect();

    // Step 3: Compute Jaccard between first two (diagnostic metric)
    let jaccard_val = sdr::jaccard(&sdrs[0], &sdrs[1]);

    // Step 4: Fold all SDRs with the specified operation
    let op_fn: fn(&sdr::Sdr, &sdr::Sdr) -> sdr::Sdr = match body.operation {
        SdrOperation::And => sdr::and,
        SdrOperation::Or => sdr::or,
        SdrOperation::Xor => sdr::xor,
    };
    let combined = sdrs[1..].iter().fold(sdrs[0], |acc, s| op_fn(&acc, s));
    let combined_pop = sdr::popcount(&combined);

    // Step 5: Query SdrIndex with the combined SDR
    let results = state.sdr_index.query(&combined, body.limit);

    if results.is_empty() {
        return Ok((
            StatusCode::OK,
            Json(SdrOperationResponse {
                jaccard: jaccard_val,
                combined_popcount: combined_pop,
                events: vec![],
            }),
        ));
    }

    // Step 6: Fetch metadata from PostgreSQL
    let result_ids: Vec<Uuid> = results.iter().map(|(id, _)| *id).collect();
    let sim_map: HashMap<Uuid, f64> = results.into_iter().collect();
    let event_rows = engrams::fetch_engram_events(state.store.pool(), &result_ids).await?;

    let events: Vec<EngramEvent> = event_rows
        .into_iter()
        .map(|(id, tier_str, tags, trigger_type, trigger_label, created_at, access_count)| {
            let similarity = sim_map.get(&id).copied().unwrap_or(0.0);
            EngramEvent {
                id,
                similarity,
                tier: parse_tier(&tier_str),
                tags: tags.clone(),
                trigger: build_trigger(&trigger_type, trigger_label, &tags),
                created_at,
                access_count,
            }
        })
        .collect();

    Ok((
        StatusCode::OK,
        Json(SdrOperationResponse {
            jaccard: jaccard_val,
            combined_popcount: combined_pop,
            events,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Bump access count and last_accessed timestamp for a batch of engram IDs.
async fn bump_access_counts(
    pool: &sqlx::PgPool,
    ids: &[Uuid],
) -> Result<(), ygg_store::error::StoreError> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"
        UPDATE yggdrasil.engrams
        SET access_count = access_count + 1,
            last_accessed = NOW()
        WHERE id = ANY($1)
        "#,
    )
    .bind(ids)
    .execute(pool)
    .await
    .map_err(|e: sqlx::Error| ygg_store::error::StoreError::Query(e.to_string()))?;
    Ok(())
}

/// Build an `EngramTrigger` from the stored trigger_type and trigger_label fields.
///
/// For `Pattern` triggers the `intent_hint` is derived from the engram's tags
/// (first tag that is not "auto-summary" or "fact" or "decision"), falling back
/// to "general" if no meaningful tag is found.
fn build_trigger(trigger_type: &str, trigger_label: String, tags: &[String]) -> EngramTrigger {
    match trigger_type {
        "fact" => EngramTrigger::Fact { label: trigger_label },
        "decision" => EngramTrigger::Decision { label: trigger_label },
        _ => {
            // Pattern: derive intent_hint from tags
            let intent_hint = tags
                .iter()
                .find(|t| {
                    let lower = t.to_lowercase();
                    lower != "auto-summary"
                        && lower != "fact"
                        && lower != "decision"
                        && lower != "core"
                        && lower != "pattern"
                })
                .cloned()
                .unwrap_or_else(|| "general".to_string());
            EngramTrigger::Pattern {
                label: trigger_label,
                intent_hint,
            }
        }
    }
}

/// Parse a tier string to `MemoryTier`.
fn parse_tier(s: &str) -> MemoryTier {
    match s {
        "core" => MemoryTier::Core,
        "archival" => MemoryTier::Archival,
        _ => MemoryTier::Recall,
    }
}

/// Truncate `text` to at most `max_chars` characters, breaking at a word boundary.
///
/// If the text is shorter than `max_chars`, returns it as-is.
/// Otherwise trims to the last whitespace before the limit, or hard-cuts if no
/// whitespace is found within the limit.
fn truncate_to_word_boundary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    // Find the last whitespace at or before max_chars
    let candidate = &text[..max_chars];
    if let Some(pos) = candidate.rfind(char::is_whitespace) {
        text[..pos].trim_end().to_string()
    } else {
        // No whitespace — hard cut at char boundary
        let mut end = max_chars;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_string()
    }
}

// ---------------------------------------------------------------------------
// POST /api/v1/timeline
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TimelineRequest {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default = "default_timeline_limit")]
    pub limit: u32,
}

fn default_timeline_limit() -> u32 {
    10
}

#[derive(Debug, Serialize)]
pub struct TimelineEngram {
    cause: String,
    effect: String,
    tier: String,
    tags: Vec<String>,
    created_at: String,
}

#[derive(Debug, Serialize)]
pub struct TimelineResponse {
    results: Vec<TimelineEngram>,
}

/// Query engrams with temporal and tag filters.
pub async fn timeline(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TimelineRequest>,
) -> Result<Json<TimelineResponse>, MimirError> {
    use chrono::DateTime;

    let after = body
        .after
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let before = body
        .before
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let tags_ref = body.tags.as_deref();
    let tier_ref = body.tier.as_deref();
    let limit = body.limit.min(50);

    let rows = engrams::query_timeline(state.store.pool(), after, before, tags_ref, tier_ref, limit)
        .await
        .map_err(|e| MimirError::Internal(format!("timeline query failed: {}", e)))?;

    let results: Vec<TimelineEngram> = rows
        .into_iter()
        .map(|(cause, effect, tier, tags, created_at)| TimelineEngram {
            cause,
            effect,
            tier,
            tags,
            created_at: created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(TimelineResponse { results }))
}

// ---------------------------------------------------------------------------
// Context offload — in-memory key-value store
// ---------------------------------------------------------------------------

struct ContextEntry {
    content: String,
    label: String,
}

static CONTEXT_STORE: std::sync::LazyLock<std::sync::Mutex<HashMap<String, ContextEntry>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[derive(Debug, Deserialize)]
pub struct ContextStoreRequest {
    pub content: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ContextStoreResponse {
    handle: String,
}

/// POST /api/v1/context — store content and return a handle.
pub async fn context_store(
    Json(body): Json<ContextStoreRequest>,
) -> Result<Json<ContextStoreResponse>, MimirError> {
    let handle = Uuid::new_v4().to_string()[..8].to_string();
    let label = body.label.unwrap_or_else(|| "(unlabeled)".to_string());

    CONTEXT_STORE
        .lock()
        .unwrap()
        .insert(handle.clone(), ContextEntry {
            content: body.content,
            label,
        });

    Ok(Json(ContextStoreResponse { handle }))
}

#[derive(Debug, Serialize)]
pub struct ContextRetrieveResponse {
    content: String,
    label: String,
}

/// GET /api/v1/context/:handle — retrieve stored content.
pub async fn context_retrieve(
    axum::extract::Path(handle): axum::extract::Path<String>,
) -> Result<Json<ContextRetrieveResponse>, MimirError> {
    let store = CONTEXT_STORE.lock().unwrap();
    match store.get(&handle) {
        Some(entry) => Ok(Json(ContextRetrieveResponse {
            content: entry.content.clone(),
            label: entry.label.clone(),
        })),
        None => Err(MimirError::NotFound(format!("context handle '{}' not found", handle))),
    }
}

#[derive(Debug, Serialize)]
pub struct ContextListItem {
    handle: String,
    label: String,
    size: usize,
}

#[derive(Debug, Serialize)]
pub struct ContextListResponse {
    items: Vec<ContextListItem>,
}

/// GET /api/v1/context — list all stored contexts.
pub async fn context_list() -> Json<ContextListResponse> {
    let store = CONTEXT_STORE.lock().unwrap();
    let items = store
        .iter()
        .map(|(handle, entry): (&String, &ContextEntry)| ContextListItem {
            handle: handle.clone(),
            label: entry.label.clone(),
            size: entry.content.len(),
        })
        .collect();

    Json(ContextListResponse { items })
}

// ---------------------------------------------------------------------------
// POST /api/v1/tasks/push — create a new task
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TaskPushRequest {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TaskPushResponse {
    pub id: String,
}

pub async fn task_push(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TaskPushRequest>,
) -> Result<Json<TaskPushResponse>, MimirError> {
    if req.title.trim().is_empty() {
        return Err(MimirError::Validation("title must not be empty".to_string()));
    }

    let id = ygg_store::postgres::tasks::push(
        state.store.pool(),
        &req.title,
        &req.description,
        req.priority,
        req.project.as_deref(),
        &req.tags,
    )
    .await?;

    Ok(Json(TaskPushResponse { id: id.to_string() }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/tasks/pop — claim the next pending task
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TaskPopRequest {
    pub agent: String,
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TaskResponse {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: i32,
    pub agent: Option<String>,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub result: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

impl From<ygg_store::postgres::tasks::Task> for TaskResponse {
    fn from(t: ygg_store::postgres::tasks::Task) -> Self {
        Self {
            id: t.id.to_string(),
            title: t.title,
            description: t.description,
            status: t.status,
            priority: t.priority,
            agent: t.agent,
            project: t.project,
            tags: t.tags,
            result: t.result,
            created_at: t.created_at.to_rfc3339(),
            updated_at: t.updated_at.to_rfc3339(),
            completed_at: t.completed_at.map(|dt| dt.to_rfc3339()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TaskPopResponse {
    pub task: Option<TaskResponse>,
}

pub async fn task_pop(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TaskPopRequest>,
) -> Result<Json<TaskPopResponse>, MimirError> {
    if req.agent.trim().is_empty() {
        return Err(MimirError::Validation("agent must not be empty".to_string()));
    }

    let task = ygg_store::postgres::tasks::pop(
        state.store.pool(),
        &req.agent,
        req.project.as_deref(),
    )
    .await?;

    Ok(Json(TaskPopResponse {
        task: task.map(Into::into),
    }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/tasks/complete — mark a task as completed/failed
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TaskCompleteRequest {
    pub task_id: String,
    #[serde(default = "default_true_val")]
    pub success: bool,
    #[serde(default)]
    pub result: Option<String>,
}

fn default_true_val() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct TaskCompleteResponse {
    pub updated: bool,
}

pub async fn task_complete(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TaskCompleteRequest>,
) -> Result<Json<TaskCompleteResponse>, MimirError> {
    let task_id = Uuid::parse_str(&req.task_id)
        .map_err(|_| MimirError::Validation("invalid task_id UUID".to_string()))?;

    let updated = ygg_store::postgres::tasks::complete(
        state.store.pool(),
        task_id,
        req.success,
        req.result.as_deref(),
    )
    .await?;

    Ok(Json(TaskCompleteResponse { updated }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/tasks/cancel — cancel a task
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TaskCancelRequest {
    pub task_id: String,
}

#[derive(Debug, Serialize)]
pub struct TaskCancelResponse {
    pub cancelled: bool,
}

pub async fn task_cancel(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TaskCancelRequest>,
) -> Result<Json<TaskCancelResponse>, MimirError> {
    let task_id = Uuid::parse_str(&req.task_id)
        .map_err(|_| MimirError::Validation("invalid task_id UUID".to_string()))?;

    let cancelled = ygg_store::postgres::tasks::cancel(state.store.pool(), task_id).await?;
    Ok(Json(TaskCancelResponse { cancelled }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/tasks/list — list tasks with filters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TaskListRequest {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default = "default_task_list_limit")]
    pub limit: Option<u32>,
}

fn default_task_list_limit() -> Option<u32> {
    Some(20)
}

#[derive(Debug, Serialize)]
pub struct TaskListResponse {
    pub tasks: Vec<TaskResponse>,
}

pub async fn task_list(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TaskListRequest>,
) -> Result<Json<TaskListResponse>, MimirError> {
    let limit = req.limit.unwrap_or(20).min(100);

    let tasks = ygg_store::postgres::tasks::list(
        state.store.pool(),
        req.status.as_deref(),
        req.project.as_deref(),
        req.agent.as_deref(),
        limit,
    )
    .await?;

    Ok(Json(TaskListResponse {
        tasks: tasks.into_iter().map(Into::into).collect(),
    }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/graph/link — create an edge between engrams
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GraphLinkRequest {
    pub source_id: String,
    pub target_id: String,
    pub relation: String,
    #[serde(default = "default_weight")]
    pub weight: f32,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

fn default_weight() -> f32 {
    1.0
}

#[derive(Debug, Serialize)]
pub struct GraphLinkResponse {
    pub id: String,
}

pub async fn graph_link(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GraphLinkRequest>,
) -> Result<Json<GraphLinkResponse>, MimirError> {
    let source = Uuid::parse_str(&req.source_id)
        .map_err(|_| MimirError::Validation("invalid source_id UUID".to_string()))?;
    let target = Uuid::parse_str(&req.target_id)
        .map_err(|_| MimirError::Validation("invalid target_id UUID".to_string()))?;
    if req.relation.trim().is_empty() {
        return Err(MimirError::Validation("relation must not be empty".to_string()));
    }

    let id = ygg_store::postgres::edges::link(
        state.store.pool(),
        source,
        target,
        &req.relation,
        req.weight,
        req.metadata,
    )
    .await?;

    Ok(Json(GraphLinkResponse { id: id.to_string() }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/graph/unlink — remove an edge
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GraphUnlinkRequest {
    pub source_id: String,
    pub target_id: String,
    pub relation: String,
}

#[derive(Debug, Serialize)]
pub struct GraphUnlinkResponse {
    pub removed: bool,
}

pub async fn graph_unlink(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GraphUnlinkRequest>,
) -> Result<Json<GraphUnlinkResponse>, MimirError> {
    let source = Uuid::parse_str(&req.source_id)
        .map_err(|_| MimirError::Validation("invalid source_id UUID".to_string()))?;
    let target = Uuid::parse_str(&req.target_id)
        .map_err(|_| MimirError::Validation("invalid target_id UUID".to_string()))?;

    let removed =
        ygg_store::postgres::edges::unlink(state.store.pool(), source, target, &req.relation)
            .await?;

    Ok(Json(GraphUnlinkResponse { removed }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/graph/neighbors — get edges for an engram
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GraphNeighborsRequest {
    pub engram_id: String,
    /// "outgoing", "incoming", or "both" (default: "both").
    #[serde(default = "default_direction")]
    pub direction: String,
    #[serde(default)]
    pub relation: Option<String>,
    #[serde(default = "default_graph_limit")]
    pub limit: Option<u32>,
}

fn default_direction() -> String {
    "both".to_string()
}

fn default_graph_limit() -> Option<u32> {
    Some(20)
}

#[derive(Debug, Serialize)]
pub struct EdgeResponse {
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    pub relation: String,
    pub weight: f32,
    pub created_at: String,
}

impl From<ygg_store::postgres::edges::Edge> for EdgeResponse {
    fn from(e: ygg_store::postgres::edges::Edge) -> Self {
        Self {
            id: e.id.to_string(),
            source_id: e.source_id.to_string(),
            target_id: e.target_id.to_string(),
            relation: e.relation,
            weight: e.weight,
            created_at: e.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct GraphNeighborsResponse {
    pub edges: Vec<EdgeResponse>,
}

pub async fn graph_neighbors(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GraphNeighborsRequest>,
) -> Result<Json<GraphNeighborsResponse>, MimirError> {
    let engram_id = Uuid::parse_str(&req.engram_id)
        .map_err(|_| MimirError::Validation("invalid engram_id UUID".to_string()))?;

    let direction = match req.direction.as_str() {
        "outgoing" => ygg_store::postgres::edges::Direction::Outgoing,
        "incoming" => ygg_store::postgres::edges::Direction::Incoming,
        _ => ygg_store::postgres::edges::Direction::Both,
    };

    let limit = req.limit.unwrap_or(20).min(100);
    let edges = ygg_store::postgres::edges::neighbors(
        state.store.pool(),
        engram_id,
        direction,
        req.relation.as_deref(),
        limit,
    )
    .await?;

    Ok(Json(GraphNeighborsResponse {
        edges: edges.into_iter().map(Into::into).collect(),
    }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/graph/traverse — BFS graph traversal
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GraphTraverseRequest {
    pub start_id: String,
    /// Max hops (default 2, max 5).
    #[serde(default = "default_max_depth")]
    pub max_depth: Option<u32>,
    #[serde(default)]
    pub relation: Option<String>,
    #[serde(default = "default_graph_limit")]
    pub limit: Option<u32>,
}

fn default_max_depth() -> Option<u32> {
    Some(2)
}

#[derive(Debug, Serialize)]
pub struct GraphTraverseResponse {
    pub edges: Vec<EdgeResponse>,
}

pub async fn graph_traverse(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GraphTraverseRequest>,
) -> Result<Json<GraphTraverseResponse>, MimirError> {
    let start_id = Uuid::parse_str(&req.start_id)
        .map_err(|_| MimirError::Validation("invalid start_id UUID".to_string()))?;

    let max_depth = req.max_depth.unwrap_or(2).min(5);
    let limit = req.limit.unwrap_or(50).min(200);

    let edges = ygg_store::postgres::edges::traverse(
        state.store.pool(),
        start_id,
        max_depth,
        req.relation.as_deref(),
        limit,
    )
    .await?;

    Ok(Json(GraphTraverseResponse {
        edges: edges.into_iter().map(Into::into).collect(),
    }))
}

// ---------------------------------------------------------------------------
// Tests for pure helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_unchanged() {
        let s = "hello world";
        assert_eq!(truncate_to_word_boundary(s, 80), s);
    }

    #[test]
    fn truncate_at_word_boundary() {
        let s = "the quick brown fox jumps over the lazy dog";
        let result = truncate_to_word_boundary(s, 15);
        // "the quick brown" is 15 chars — last space before 15 is at index 9 ("the quick")
        assert!(!result.contains(' ') || result.len() <= 15);
        assert!(result.len() <= 15);
    }

    #[test]
    fn build_trigger_fact() {
        let t = build_trigger("fact", "some fact".to_string(), &[]);
        assert!(matches!(t, EngramTrigger::Fact { .. }));
    }

    #[test]
    fn build_trigger_decision() {
        let t = build_trigger("decision", "some decision".to_string(), &[]);
        assert!(matches!(t, EngramTrigger::Decision { .. }));
    }

    #[test]
    fn build_trigger_pattern_with_hint() {
        let tags = vec!["coding".to_string()];
        let t = build_trigger("pattern", "label".to_string(), &tags);
        if let EngramTrigger::Pattern { intent_hint, .. } = t {
            assert_eq!(intent_hint, "coding");
        } else {
            panic!("expected Pattern");
        }
    }

    #[test]
    fn build_trigger_pattern_fallback() {
        let t = build_trigger("pattern", "label".to_string(), &[]);
        if let EngramTrigger::Pattern { intent_hint, .. } = t {
            assert_eq!(intent_hint, "general");
        } else {
            panic!("expected Pattern");
        }
    }
}
