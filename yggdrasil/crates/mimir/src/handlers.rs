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

    // Step 7: Insert into PostgreSQL
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

    // Step 8: Insert into in-memory SDR index
    state.sdr_index.insert(id, sdr_val);

    // Step 9: Upsert into Qdrant engrams_sdr collection
    let sdr_f32 = sdr::to_f32_vec(&sdr_val);
    state
        .vectors
        .upsert("engrams_sdr", id, sdr_f32, HashMap::new())
        .await?;

    // Step 10: Return 201 Created
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

    // Step 10: Return RecallResponse
    let response = RecallResponse { events, core_events };
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
