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
    AutoIngestRequest, AutoIngestResponse, EngramEvent, EngramQuery, EngramTrigger, MemoryTier,
    NewEngram, RecallQuery, RecallResponse,
};
use ygg_store::postgres::engrams;
use ygg_store::qdrant::{Condition, Filter, Value};

use sqlx;

use crate::{error::MimirError, sdr, state::AppState};

/// Build a Qdrant payload for project/scope isolation on the v2 collection.
fn build_qdrant_payload(project: Option<&str>, scope: &str) -> HashMap<String, Value> {
    let mut payload = HashMap::new();
    if let Some(p) = project {
        payload.insert("project".to_string(), Value::from(p.to_string()));
    }
    payload.insert("scope".to_string(), Value::from(scope.to_string()));
    payload
}

/// Build a Qdrant filter for project-scoped queries.
///
/// - project=Some + include_global: should(project=p OR scope="global")
/// - project=Some + !include_global: must(project=p)
/// - project=None: no filter (search everything)
fn build_project_filter(project: Option<&str>, include_global: bool) -> Option<Filter> {
    match project {
        Some(p) if include_global => Some(Filter::should(vec![
            Condition::matches("project", p.to_string()),
            Condition::matches("scope", "global".to_string()),
        ])),
        Some(p) => Some(Filter::must(vec![
            Condition::matches("project", p.to_string()),
        ])),
        None => None,
    }
}

/// SHA-256 hash of `cause + "\n" + effect` for engram content dedup in PG.
pub fn engram_content_hash(cause: &str, effect: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(cause.as_bytes());
    hasher.update(b"\n");
    hasher.update(effect.as_bytes());
    hasher.finalize().to_vec()
}

/// Sprint 065 A·P1: extract partition-prefix tags from free-form cause text.
///
/// Detects `sprint NNN` / `sprint-NNN` / `sprint:NNN` / `sprint NNN` (3 digits)
/// and emits normalized `sprint:NNN` tags. Used to auto-partition engrams
/// whose cause text references a sprint number even when the caller did not
/// supply an explicit `sprint:NNN` tag. Prevents cross-sprint SDR collisions
/// on "Sprint NNN: archived" summaries where the only distinguishing feature
/// is the number (engram `d6701e4c` workaround fix).
///
/// Conservative scan: only 3-digit sprint numbers, no regex dep.
pub fn detect_partition_tags(cause: &str) -> Vec<String> {
    fn is_separator(b: u8) -> bool {
        matches!(b, b' ' | b'\t' | b':' | b'-' | b'_')
    }

    let mut out = Vec::new();
    let lower = cause.to_lowercase();
    for (idx, _) in lower.match_indices("sprint") {
        let rest = &lower[idx + "sprint".len()..];
        let bytes = rest.as_bytes();
        // Skip up to 4 separator chars only — whitespace, colon, hyphen, underscore.
        // Any other non-digit char (e.g. 'a' in "sprint a065") means NOT a partition tag.
        let mut i = 0;
        while i < bytes.len() && i < 4 && is_separator(bytes[i]) {
            i += 1;
        }
        // Require exactly 3 digits in sequence, with a word boundary after.
        if i + 3 <= bytes.len()
            && bytes[i].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && (i + 3 == bytes.len() || !bytes[i + 3].is_ascii_digit())
        {
            let digits = &rest[i..i + 3];
            let tag = format!("sprint:{digits}");
            if !out.contains(&tag) {
                out.push(tag);
            }
        }
    }
    out
}

/// Sprint 065 A·P1: compute the effective tag list for an engram by merging
/// caller-supplied tags with auto-detected partition-prefix tags from the
/// cause text. Deduplicates case-insensitively. Returns the merged Vec for
/// persistence into PG and for the in-memory tag_index.
pub fn effective_tags(caller_tags: &[String], cause: &str) -> Vec<String> {
    let mut merged: Vec<String> = caller_tags.to_vec();
    let lower: Vec<String> = merged.iter().map(|t| t.to_lowercase()).collect();
    for auto in detect_partition_tags(cause) {
        if !lower.contains(&auto.to_lowercase()) {
            merged.push(auto);
        }
    }
    merged
}

/// Sprint 065 A·P1: extract partition-prefix tags (sprint:, incident:, release:)
/// from a full tag list — the ones the SDR index uses to hard-partition queries.
fn partition_prefix_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .filter(|t| {
            let l = t.to_lowercase();
            l.starts_with("sprint:") || l.starts_with("incident:") || l.starts_with("release:")
        })
        .cloned()
        .collect()
}

/// Helper to build a skip response for auto-ingest.
fn auto_ingest_skip(reason: &str) -> (StatusCode, Json<AutoIngestResponse>) {
    (
        StatusCode::OK,
        Json(AutoIngestResponse {
            stored: false,
            engram_id: None,
            matched_template: None,
            similarity: None,
            skipped_reason: Some(reason.into()),
        }),
    )
}

// ---------------------------------------------------------------------------
// GET /health
// ---------------------------------------------------------------------------

/// Health check endpoint.  Probes PG pool and Qdrant to detect degradation.
pub async fn health(State(state): State<Arc<AppState>>) -> (StatusCode, Json<serde_json::Value>) {
    let pg_ok = sqlx::query("SELECT 1")
        .fetch_one(state.store.pool())
        .await
        .is_ok();

    let qdrant_ok = state
        .vectors
        .ensure_collection("engrams_sdr")
        .await
        .is_ok();

    let status = if pg_ok && qdrant_ok { "healthy" } else { "degraded" };
    let code = if pg_ok && qdrant_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        code,
        Json(serde_json::json!({
            "status": status,
            "postgresql": pg_ok,
            "qdrant": qdrant_ok,
        })),
    )
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
    let content_hash = engram_content_hash(&body.cause, &body.effect);

    // Step 3: Embed cause text via ONNX (sync — must use spawn_blocking)
    let embedder = state.embedder.clone();
    let cause_text = body.cause.clone();
    let embedding: Vec<f32> =
        tokio::task::spawn_blocking(move || embedder.embed(&cause_text))
            .await
            .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

    // Step 4: Binarize → 256-bit SDR (uses first 256 dims of the 384-dim vector)
    let sdr_val = sdr::binarize(&embedding[..sdr::SDR_BITS]);
    let sdr_bytes = sdr::to_bytes(&sdr_val);

    // Step 5: Determine trigger type from tags.
    // Sprint 065 A·P1: merge caller tags with auto-detected partition tags
    // (e.g. `sprint:NNN` from cause text) into effective_tags, then compute
    // partition_tags for the SDR index hard-partition filter.
    let merged_tags = effective_tags(&body.tags, &body.cause);
    let tags_lower: Vec<String> = merged_tags.iter().map(|t| t.to_lowercase()).collect();
    let trigger_type = if tags_lower.iter().any(|t| t == "fact" || t == "core") {
        "fact"
    } else if tags_lower.iter().any(|t| t == "decision") {
        "decision"
    } else {
        "pattern"
    };
    let partition_tags = partition_prefix_tags(&merged_tags);

    // Step 6: Extract trigger label — first 80 chars of cause, trimmed to word boundary
    let trigger_label = truncate_to_word_boundary(body.cause.trim(), 80);

    // Step 7: Update-by-ID path vs new insert path
    if let Some(existing_id) = body.id {
        // Update path: caller knows the engram ID, bypass novelty gate
        // Resolve scope: explicit > inferred from project > "global"
        let scope = body.scope.as_deref().unwrap_or(
            if body.project.is_some() { "project" } else { "global" }
        );

        let updated = engrams::update_engram_sdr(
            state.store.pool(),
            existing_id,
            &engrams::EngramSdrParams {
                cause: &body.cause,
                effect: &body.effect,
                sdr_bits: &sdr_bytes,
                content_hash: &content_hash,
                tags: &merged_tags,
                trigger_type,
                trigger_label: &trigger_label,
                project: body.project.as_deref(),
                scope,
            },
        )
        .await?;

        if !updated {
            return Err(MimirError::NotFound(format!(
                "engram {} not found for update",
                existing_id
            )));
        }

        tracing::info!(engram_id = %existing_id, trigger_type, "engram updated by ID");

        // Update in-memory SDR index: remove old, insert scoped with merged tags
        // so the partition-prefix filter keeps working after update-by-id.
        state.sdr_index.remove(existing_id);
        state.sdr_index.insert_scoped_with_tags(
            body.project.as_deref(),
            existing_id,
            sdr_val,
            &merged_tags,
        );

        // Update both legacy and v2 Qdrant collections
        let sdr_f32 = sdr::to_bipolar_f32(&sdr_val);
        let payload = build_qdrant_payload(body.project.as_deref(), scope);
        state
            .vectors
            .upsert("engrams_sdr", existing_id, sdr_f32.clone(), HashMap::new())
            .await?;
        state
            .vectors
            .upsert(crate::state::V2_SDR_COLLECTION, existing_id, sdr_f32, payload)
            .await?;

        return Ok((StatusCode::OK, Json(serde_json::json!({ "id": existing_id, "updated": true }))));
    }

    // Step 4b: Novelty triage (Sprint 064 P1) — server-side New / Update / Old verdict.
    // Skipped entirely when force=true (operator override).
    let novelty_cfg = &state.config.sdr.novelty;
    if !body.force {
        // Sprint 065 A·P1: when the engram carries partition-prefix tags
        // (sprint:NNN, incident:NNN, release:vX), hard-partition the novelty
        // lookup so sprint-archive SDRs at similarity ~1.0 cannot collide
        // across sprints. Empty partition_tags = legacy behavior.
        let nearest_match: Option<(Uuid, f64)> = if let Some(ref proj) = body.project {
            if partition_tags.is_empty() {
                state
                    .sdr_index
                    .query_scoped(&sdr_val, proj, true, 1)
                    .into_iter()
                    .next()
            } else {
                state
                    .sdr_index
                    .query_scoped_with_tags(&sdr_val, proj, true, &partition_tags, 1)
                    .into_iter()
                    .next()
            }
        } else {
            state.sdr_index.query(&sdr_val, 1).into_iter().next()
        };

        if let Some((dup_id, sim)) = nearest_match
            && sim >= novelty_cfg.update_threshold
        {
            // Fetch the existing engram so the verdict has full context.
            let dup_ids = vec![dup_id];
            let empty_sim = std::collections::HashMap::new();
            let existing = engrams::fetch_engrams_by_ids(
                state.store.pool(),
                &dup_ids,
                &empty_sim,
            )
            .await
            .ok()
            .and_then(|mut v| v.pop());

            let (existing_cause, existing_effect) = existing
                .map(|e| (e.cause, e.effect))
                .unwrap_or_default();

            // Sprint 064 P1.5: ask the store-gate LLM (LFM2.5 by default) for a
            // semantic verdict, falling through the configured backend chain.
            // On disabled/no-backends/all-failed, fall back to the threshold
            // classifier so the write path is never blocked by a model outage.
            let mut store_worthy = true;
            let verdict = match state.config.store_gate.as_ref() {
                Some(gate_cfg) if gate_cfg.enabled => {
                    match crate::store_gate::classify(
                        &state.http_client,
                        gate_cfg,
                        &body.cause,
                        &body.effect,
                        dup_id,
                        &existing_cause,
                        &existing_effect,
                        sim,
                    )
                    .await
                    {
                        Ok(decision) => {
                            store_worthy = decision.store_worthy;
                            let backend = gate_cfg
                                .backends
                                .get(decision.backend_index)
                                .map(|b| b.url.as_str())
                                .unwrap_or("?");
                            tracing::info!(
                                duplicate_id = %dup_id,
                                similarity = %sim,
                                backend,
                                reasoning = %decision.reasoning,
                                "store gate verdict received"
                            );
                            decision.verdict
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "store gate failed — falling back to threshold classifier"
                            );
                            crate::novelty::classify_novelty(
                                sim,
                                &body.effect,
                                dup_id,
                                &existing_cause,
                                &existing_effect,
                                novelty_cfg,
                            )
                        }
                    }
                }
                _ => crate::novelty::classify_novelty(
                    sim,
                    &body.effect,
                    dup_id,
                    &existing_cause,
                    &existing_effect,
                    novelty_cfg,
                ),
            };

            // The gate may declare the new engram not worth storing (noise) —
            // honour it by short-circuiting to Old (no write, return existing id).
            let verdict = if !store_worthy {
                tracing::info!(
                    duplicate_id = %dup_id,
                    "store gate flagged store_worthy=false — skipping write"
                );
                crate::novelty::NoveltyVerdict::Old { id: dup_id }
            } else {
                verdict
            };

            match verdict {
                crate::novelty::NoveltyVerdict::Old { id } => {
                    tracing::info!(
                        engram_id = %id,
                        similarity = %sim,
                        "novelty verdict=old — near-identical exists, skipping write"
                    );
                    return Ok((
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "verdict": "old",
                            "id": id,
                            "similarity": sim,
                        })),
                    ));
                }
                crate::novelty::NoveltyVerdict::Update {
                    id,
                    previous_cause,
                    previous_effect,
                } => {
                    let scope = body.scope.as_deref().unwrap_or(
                        if body.project.is_some() { "project" } else { "global" },
                    );
                    let updated = engrams::update_engram_sdr(
                        state.store.pool(),
                        id,
                        &engrams::EngramSdrParams {
                            cause: &body.cause,
                            effect: &body.effect,
                            sdr_bits: &sdr_bytes,
                            content_hash: &content_hash,
                            tags: &merged_tags,
                            trigger_type,
                            trigger_label: &trigger_label,
                            project: body.project.as_deref(),
                            scope,
                        },
                    )
                    .await?;

                    if !updated {
                        return Err(MimirError::NotFound(format!(
                            "engram {id} flagged for update but row missing"
                        )));
                    }

                    state.sdr_index.remove(id);
                    state.sdr_index.insert_scoped_with_tags(
                        body.project.as_deref(),
                        id,
                        sdr_val,
                        &merged_tags,
                    );

                    let sdr_f32 = sdr::to_bipolar_f32(&sdr_val);
                    let payload = build_qdrant_payload(body.project.as_deref(), scope);
                    state
                        .vectors
                        .upsert("engrams_sdr", id, sdr_f32.clone(), HashMap::new())
                        .await?;
                    state
                        .vectors
                        .upsert(crate::state::V2_SDR_COLLECTION, id, sdr_f32, payload)
                        .await?;

                    tracing::info!(
                        engram_id = %id,
                        similarity = %sim,
                        "novelty verdict=update — overwrote existing engram in place"
                    );
                    return Ok((
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "verdict": "update",
                            "id": id,
                            "similarity": sim,
                            "previous_cause": previous_cause,
                            "previous_effect": previous_effect,
                        })),
                    ));
                }
                crate::novelty::NoveltyVerdict::New => {
                    // Fall through to insert path below.
                }
            }
        }
    }

    // Resolve scope for new insert path (update path resolved above in the if-let)
    let scope = body.scope.as_deref().unwrap_or(
        if body.project.is_some() { "project" } else { "global" }
    );

    // Step 8: Insert into PostgreSQL
    let id = engrams::insert_engram_sdr(
        state.store.pool(),
        &engrams::EngramSdrParams {
            cause: &body.cause,
            effect: &body.effect,
            sdr_bits: &sdr_bytes,
            content_hash: &content_hash,
            tags: &merged_tags,
            trigger_type,
            trigger_label: &trigger_label,
            project: body.project.as_deref(),
            scope,
        },
        MemoryTier::Recall,
    )
    .await?;

    tracing::info!(engram_id = %id, trigger_type, project = ?body.project, scope, "engram stored via SDR");

    // Step 9: Insert into project-scoped in-memory SDR index with merged tags
    // so partition-prefix queries find this engram immediately.
    state
        .sdr_index
        .insert_scoped_with_tags(body.project.as_deref(), id, sdr_val, &merged_tags);

    // Step 10: Upsert into both legacy and v2 Qdrant collections
    let sdr_f32 = sdr::to_bipolar_f32(&sdr_val);

    // Legacy collection (backward compat during migration)
    state
        .vectors
        .upsert("engrams_sdr", id, sdr_f32.clone(), HashMap::new())
        .await?;

    // Also upsert into legacy category collections for backward compat
    // (tags_lower was computed at Step 5 from merged_tags)
    if tags_lower.iter().any(|t| t == "sprint") {
        state.vectors.upsert("sprints", id, embedding.clone(), HashMap::new()).await?;
    } else if tags_lower.iter().any(|t| t == "topology") {
        state.vectors.upsert("topology", id, embedding.clone(), HashMap::new()).await?;
    }

    // V2 collection with payload-based project isolation
    let payload = build_qdrant_payload(body.project.as_deref(), scope);
    state
        .vectors
        .upsert(crate::state::V2_SDR_COLLECTION, id, sdr_f32, payload)
        .await?;

    // Step 11: Fire-and-forget graph linking (Sprint 055)
    crate::linker::spawn_link_engram(
        state.clone(),
        id,
        body.cause.clone(),
        body.effect.clone(),
        sdr_val,
    );

    // Step 12: Return 201 Created — include verdict for client consistency (Sprint 064 P1).
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "verdict": "new", "id": id })),
    ))
}

// ---------------------------------------------------------------------------
// POST /api/v1/sprints/list
// ---------------------------------------------------------------------------

/// List sprint engrams by searching the dedicated `sprints` Qdrant collection.
///
/// Uses dense 384-dim embeddings (not SDR) for semantic search, then fetches
/// full engram records from PostgreSQL.
pub async fn list_sprints(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SprintListRequest>,
) -> Result<Json<serde_json::Value>, MimirError> {
    let query_text = body
        .project
        .as_deref()
        .map(|p| format!("{p} sprint history"))
        .unwrap_or_else(|| "sprint history".to_string());
    let limit = body.limit.unwrap_or(10).min(50) as usize;

    let embedder = state.embedder.clone();
    let qt = query_text.clone();
    let embedding: Vec<f32> =
        tokio::task::spawn_blocking(move || embedder.embed(&qt))
            .await
            .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

    let results = state
        .vectors
        .search("sprints", embedding, limit as u64)
        .await
        .map_err(MimirError::Store)?;

    let ids: Vec<Uuid> = results.iter().map(|(id, _)| *id).collect();
    let sim_map: std::collections::HashMap<Uuid, f64> = results
        .into_iter()
        .map(|(id, sim)| (id, sim as f64))
        .collect();
    let engrams =
        engrams::fetch_engrams_by_ids(state.store.pool(), &ids, &sim_map).await?;

    Ok(Json(serde_json::json!({ "results": engrams })))
}

/// Request body for `POST /api/v1/sprints/list`.
#[derive(Debug, Deserialize)]
pub struct SprintListRequest {
    /// Optional project name filter (e.g. "yggdrasil").
    pub project: Option<String>,
    /// Max results to return (default 10, max 50).
    pub limit: Option<u32>,
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
            .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

    let query_sdr = sdr::binarize(&embedding[..sdr::SDR_BITS]);
    let sdr_f32 = sdr::to_bipolar_f32(&query_sdr);

    // Step 2: Dual-system search (Hamming + Qdrant) in parallel
    // Uses project-scoped search on both systems when project is set.
    let limit = body.limit;
    let filter = build_project_filter(body.project.as_deref(), body.include_global);

    let sys1 = if let Some(ref proj) = body.project {
        state.sdr_index.query_scoped(&query_sdr, proj, body.include_global, limit)
    } else {
        state.sdr_index.query(&query_sdr, limit)
    };

    let sys2 = state
        .vectors
        .search_filtered(crate::state::V2_SDR_COLLECTION, sdr_f32, limit as u64, filter)
        .await?;

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

    // Sort by similarity descending, take extra candidates for composite re-ranking
    let mut ranked: Vec<(Uuid, f64)> = merged.into_iter().collect();
    ranked.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit * 2);

    if ranked.is_empty() {
        return Ok((StatusCode::OK, Json(serde_json::json!({ "results": [] }))));
    }

    // Step 4: Fetch full engram data from PostgreSQL
    let sim_map: HashMap<Uuid, f64> = ranked.iter().cloned().collect();
    let now = chrono::Utc::now();
    let mut results = Vec::with_capacity(ranked.len());
    for (id, _) in &ranked {
        match engrams::get_engram(state.store.pool(), *id).await {
            Ok(mut engram) => {
                let raw_sim = sim_map.get(id).copied().unwrap_or(0.0);
                // Composite scoring: blend similarity with recency and access frequency
                let hours_since_access = (now - engram.last_accessed).num_seconds().max(0) as f64 / 3600.0;
                let recency = (-0.01 * hours_since_access).exp();
                let importance = ((engram.access_count as f64) + 1.0).ln().min(1.0);
                engram.similarity = (0.5 * raw_sim) + (0.15 * recency) + (0.15 * importance) + (0.2 * engram.confidence);
                results.push(engram);
            }
            Err(e) => {
                tracing::warn!(engram_id = %id, error = %e, "skipping engram in query results");
            }
        }
    }

    // Re-sort by composite score and truncate to requested limit
    results.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);

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
            .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

    let query_sdr = sdr::binarize(&embedding[..sdr::SDR_BITS]);
    let sdr_f32 = sdr::to_bipolar_f32(&query_sdr);

    // Steps 3-4: Run System 1 (Hamming), System 2 (Qdrant), System 3 (BM25) in parallel
    let limit = body.limit;
    let filter = build_project_filter(body.project.as_deref(), body.include_global);

    let sys1 = if let Some(ref proj) = body.project {
        state.sdr_index.query_scoped(&query_sdr, proj, body.include_global, limit)
    } else {
        state.sdr_index.query(&query_sdr, limit)
    };

    // System 2 (Qdrant vector) and System 3 (PG fulltext) run concurrently
    let query_text_for_bm25 = body.text.clone();
    let pool = state.store.pool().clone();
    let (sys2, sys3) = tokio::join!(
        state
            .vectors
            .search_filtered(crate::state::V2_SDR_COLLECTION, sdr_f32, limit as u64, filter),
        engrams::fulltext_search(&pool, &query_text_for_bm25, limit),
    );
    let sys2 = sys2?;
    let sys3 = sys3.unwrap_or_default();

    // Step 5: Merge all three systems by UUID — take max similarity.
    //
    // System 1: normalized Hamming similarity in [0.0, 1.0].
    // System 2: raw Qdrant dot-product, normalized by query popcount → [0.0, 1.0].
    // System 3: ts_rank BM25 score, normalized by max rank → [0.0, 1.0].
    let query_pop = sdr::popcount(&query_sdr) as f64;
    let normalizer = if query_pop > 0.0 { query_pop } else { 1.0 };

    // Find max BM25 rank for normalization
    let max_bm25 = sys3.iter().map(|(_, r)| *r).fold(0.0f64, f64::max);
    let bm25_normalizer = if max_bm25 > 0.0 { max_bm25 } else { 1.0 };

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
    // System 3 (BM25): keyword matches boost existing scores or add new candidates
    for (id, rank) in sys3 {
        let normalized = (rank / bm25_normalizer).clamp(0.0, 1.0);
        merged
            .entry(id)
            .and_modify(|s| {
                // BM25 match boosts the score (keyword + semantic = stronger signal)
                *s = (*s + 0.15 * normalized).min(1.0);
            })
            .or_insert(normalized * 0.8); // keyword-only match gets slight penalty
    }

    // Sort by similarity descending, take extra candidates for composite re-ranking
    let mut ranked: Vec<(Uuid, f64)> = merged.into_iter().collect();
    ranked.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    // Fetch extra candidates (2x limit) so composite scoring can re-rank effectively
    ranked.truncate(limit * 2);

    // Step 6: Fetch metadata from PostgreSQL (includes last_accessed for composite scoring)
    let result_ids: Vec<Uuid> = ranked.iter().map(|(id, _)| *id).collect();
    let sim_map: HashMap<Uuid, f64> = ranked.into_iter().collect();

    let event_rows = engrams::fetch_engram_events(state.store.pool(), &result_ids).await?;

    // Step 7: Build EngramEvent list with composite scoring
    // score = (0.6 * similarity) + (0.2 * recency_decay) + (0.2 * importance)
    let now = chrono::Utc::now();
    let mut events: Vec<EngramEvent> = event_rows
        .into_iter()
        .map(|(id, tier_str, tags, trigger_type, trigger_label, created_at, access_count, last_accessed, confidence)| {
            let raw_sim = sim_map.get(&id).copied().unwrap_or(0.0);

            // Composite scoring: blend similarity, recency, importance, and confidence
            let hours_since_access = (now - last_accessed).num_seconds().max(0) as f64 / 3600.0;
            let recency = (-0.01 * hours_since_access).exp(); // decay over ~100 hours
            let importance = ((access_count as f64) + 1.0).ln().min(1.0); // log scale, capped at 1.0
            let composite = (0.5 * raw_sim) + (0.15 * recency) + (0.15 * importance) + (0.2 * confidence);

            EngramEvent {
                id,
                similarity: composite,
                tier: parse_tier(&tier_str),
                tags: tags.clone(),
                trigger: build_trigger(&trigger_type, trigger_label, &tags),
                created_at,
                access_count,
                cause: None,
                effect: None,
            }
        })
        .collect();

    // Re-sort by composite score and truncate to requested limit
    events.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
    events.truncate(limit);

    // Step 7b: Optionally fetch cause/effect text for each event.
    // Backward-compatible: callers that omit include_text get the same metadata-only response.
    let events = if body.include_text.unwrap_or(false) {
        let fetch_ids: Vec<Uuid> = events.iter().map(|e| e.id).collect();
        let sim_map_text: HashMap<Uuid, f64> =
            events.iter().map(|e| (e.id, e.similarity)).collect();
        let full =
            engrams::fetch_engrams_by_ids(state.store.pool(), &fetch_ids, &sim_map_text).await?;
        let text_map: HashMap<Uuid, (String, String)> = full
            .into_iter()
            .map(|e| (e.id, (e.cause, e.effect)))
            .collect();
        events
            .into_iter()
            .map(|mut ev| {
                if let Some((cause, effect)) = text_map.get(&ev.id) {
                    ev.cause = Some(cause.clone());
                    ev.effect = Some(effect.clone());
                }
                ev
            })
            .collect()
    } else {
        events
    };

    // Step 8: Fetch core engrams as events
    let core_rows = engrams::get_core_engram_events(state.store.pool()).await?;
    let mut core_events: Vec<EngramEvent> = core_rows
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
            cause: None,
            effect: None,
        })
        .collect();

    // Sprint 055: Populate Core-tier text when include_text is requested.
    // This enables selective context injection — Core engrams are safe to inject
    // into the LLM prompt because they are manually curated facts.
    if body.include_text.unwrap_or(false) && !core_events.is_empty() {
        let core_ids: Vec<Uuid> = core_events.iter().map(|e| e.id).collect();
        let core_sim: HashMap<Uuid, f64> = core_events.iter().map(|e| (e.id, 1.0)).collect();
        if let Ok(full) = engrams::fetch_engrams_by_ids(state.store.pool(), &core_ids, &core_sim).await {
            let text_map: HashMap<Uuid, (String, String)> = full
                .into_iter()
                .map(|e| (e.id, (e.cause, e.effect)))
                .collect();
            for ev in &mut core_events {
                if let Some((cause, effect)) = text_map.get(&ev.id) {
                    ev.cause = Some(cause.clone());
                    ev.effect = Some(effect.clone());
                }
            }
        }
    }

    // Step 9: Fire-and-forget access count bump for recalled engrams (final set only)
    let final_ids: Vec<Uuid> = events.iter().map(|e| e.id).collect();
    if !final_ids.is_empty() {
        let pool = state.store.pool().clone();
        tokio::spawn(async move {
            if let Err(e) = bump_access_counts(&pool, &final_ids).await {
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
// GET /api/v1/engrams/{id}
// ---------------------------------------------------------------------------

/// Return a single engram by UUID.
pub async fn get_engram_by_id(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<impl IntoResponse, MimirError> {
    let engram = engrams::get_engram(state.store.pool(), id).await?;
    Ok((StatusCode::OK, Json(engram)))
}

// ---------------------------------------------------------------------------
// POST /api/v1/embed
// ---------------------------------------------------------------------------

/// Return the raw ONNX embedding for a text string.
///
/// Used by ygg-sentinel for SDR anomaly detection. Returns a 384-dim
/// L2-normalised float vector from all-MiniLM-L6-v2.
pub async fn embed_text(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EmbedRequest>,
) -> Result<impl IntoResponse, MimirError> {
    if body.text.trim().is_empty() {
        return Err(MimirError::Validation("text must not be empty".into()));
    }

    let embedder = state.embedder.clone();
    let text = body.text;
    let embedding: Vec<f32> =
        tokio::task::spawn_blocking(move || embedder.embed(&text))
            .await
            .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

    Ok((StatusCode::OK, Json(serde_json::json!({ "embedding": embedding }))))
}

#[derive(Debug, Deserialize)]
pub struct EmbedRequest {
    pub text: String,
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
        .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

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
        .map(|(id, tier_str, tags, trigger_type, trigger_label, created_at, access_count, _last_accessed, _confidence)| {
            let similarity = sim_map.get(&id).copied().unwrap_or(0.0);
            EngramEvent {
                id,
                similarity,
                tier: parse_tier(&tier_str),
                tags: tags.clone(),
                trigger: build_trigger(&trigger_type, trigger_label, &tags),
                created_at,
                access_count,
                cause: None,
                effect: None,
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
    // Sprint 055: Also bump confidence (+0.02, capped at 1.0) on each access.
    // Frequently recalled engrams gain confidence over time.
    sqlx::query(
        r#"
        UPDATE yggdrasil.engrams
        SET access_count = access_count + 1,
            last_accessed = NOW(),
            confidence = LEAST(1.0, confidence + 0.02)
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
pub fn truncate_to_word_boundary(text: &str, max_chars: usize) -> String {
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
// POST /api/v1/spine/push — push a labeled task for a model worker
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SpinePushRequest {
    pub title: String,
    pub label: String,
    #[serde(default)]
    pub context: serde_json::Value,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub ttl_secs: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct SpinePushResponse {
    pub id: String,
}

pub async fn spine_push(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SpinePushRequest>,
) -> Result<Json<SpinePushResponse>, MimirError> {
    if req.label.trim().is_empty() {
        return Err(MimirError::Validation("label must not be empty".to_string()));
    }

    let id = ygg_store::postgres::tasks::spine_push(
        state.store.pool(),
        &req.title,
        &req.label,
        &req.context,
        req.priority,
        req.ttl_secs,
    )
    .await?;

    tracing::info!(task_id = %id, label = %req.label, title = %req.title, "spine task pushed");
    Ok(Json(SpinePushResponse { id: id.to_string() }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/spine/pop — claim the next pending task for a label
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SpinePopRequest {
    pub agent: String,
    pub label: String,
}

#[derive(Debug, Serialize)]
pub struct SpinePopResponse {
    pub task: Option<SpineTaskResponse>,
}

#[derive(Debug, Serialize)]
pub struct SpineTaskResponse {
    pub id: String,
    pub title: String,
    pub label: String,
    pub context: serde_json::Value,
    pub priority: i32,
}

pub async fn spine_pop(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SpinePopRequest>,
) -> Result<Json<SpinePopResponse>, MimirError> {
    if req.label.trim().is_empty() {
        return Err(MimirError::Validation("label must not be empty".to_string()));
    }

    let task = ygg_store::postgres::tasks::spine_pop(
        state.store.pool(),
        &req.agent,
        &req.label,
    )
    .await?;

    Ok(Json(SpinePopResponse {
        task: task.map(|t| SpineTaskResponse {
            id: t.id.to_string(),
            title: t.title,
            label: t.label.unwrap_or_default(),
            context: t.context.unwrap_or(serde_json::Value::Null),
            priority: t.priority,
        }),
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
// POST /api/v1/auto-ingest  (Sprint 044)
// ---------------------------------------------------------------------------

/// Autonomous memory ingest endpoint.
///
/// Embeds incoming content via ONNX, compares against pre-loaded dense template
/// embeddings via cosine similarity (dot product on L2-normalized vectors), and
/// auto-generates a cause/effect engram when the best match meets the configured
/// threshold. Designed to be called by PostToolUse hook scripts with near-zero
/// blocking time (fire-and-forget from the hook side).
///
/// Pipeline:
/// 1. Validate content non-empty
/// 2. Check auto_ingest enabled flag
/// 3. Per-workstation cooldown gate
/// 4. SHA-256 content dedup gate
/// 5. Truncate to max_content_length
/// 6. ONNX embed (384-dim, L2-normalized)
/// 7. Dense cosine template matching + binarize to SDR for storage
/// 8. If best match >= threshold: store engram (PG + SDR index + Qdrant), spawn Saga enrichment
pub async fn auto_ingest(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AutoIngestRequest>,
) -> Result<(StatusCode, Json<AutoIngestResponse>), MimirError> {
    // Step 1: Validate content non-empty
    if body.content.trim().is_empty() {
        return Ok(auto_ingest_skip("empty_content"));
    }

    // Step 1b: Skip content too short to be meaningful (e.g. " -> " from empty Bash hooks)
    if body.content.trim().len() < 20 {
        return Ok(auto_ingest_skip("content_too_short"));
    }

    // Step 2: Check enabled flag (default: true when config absent)
    let cfg = state.config.auto_ingest.as_ref();
    let enabled = cfg.map(|c| c.enabled).unwrap_or(true);
    let threshold = cfg.map(|c| c.template_threshold).unwrap_or(0.3);
    let max_content = cfg.map(|c| c.max_content_length).unwrap_or(4096);
    let cooldown_secs = cfg.map(|c| c.cooldown_secs).unwrap_or(5);
    let dedup_window_secs = cfg.map(|c| c.dedup_window_secs).unwrap_or(300);

    if !enabled {
        return Ok(auto_ingest_skip("disabled"));
    }

    // Step 3: Per-workstation cooldown check
    if let Some(entry) = state.cooldown_map.get(&body.workstation) {
        if entry.value().elapsed().as_secs() < cooldown_secs {
            return Ok(auto_ingest_skip("cooldown"));
        }
    }

    // Step 4: SHA-256 hash content, check dedup window
    let content_hash_hex = format!("{:x}", Sha256::digest(body.content.as_bytes()));
    if let Some(entry) = state.content_hashes.get(&content_hash_hex) {
        if entry.value().elapsed().as_secs() < dedup_window_secs {
            return Ok(auto_ingest_skip("duplicate"));
        }
    }

    // Step 5: Truncate content to max_content_length chars
    let content: String = body.content.chars().take(max_content).collect();

    // Step 6: ONNX embed via spawn_blocking (same pattern as store_engram)
    let embedder = state.embedder.clone();
    let content_for_embed = content.clone();
    let embedding: Vec<f32> =
        tokio::task::spawn_blocking(move || embedder.embed(&content_for_embed))
            .await
            .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

    // Step 7: Dense cosine template matching (replaces lossy SDR Hamming).
    // Compare full 384-dim embedding against dense template embeddings.
    // Since both are L2-normalized (all-MiniLM-L6-v2), dot product == cosine similarity.
    // Preserves all magnitude information that SDR binarization discards.
    // Still O(N_templates), sub-microsecond for 6 templates × 384 dims.
    let mut best_sim: f64 = 0.0;
    let mut best_name: Option<String> = None;
    {
        let dense = state
            .template_embeddings
            .read()
            .map_err(|_| MimirError::Internal("template_embeddings lock poisoned".into()))?;
        for (name, template_emb) in dense.iter() {
            let sim = sdr::dot_similarity(&embedding, template_emb);
            if sim > best_sim {
                best_sim = sim;
                best_name = Some(name.clone());
            }
        }
    } // RwLock guard dropped here

    // Step 7b: Binarize → 256-bit SDR (still needed for engram storage path)
    let sdr_val = sdr::binarize(&embedding[..sdr::SDR_BITS]);

    // Step 8: Store engram if best match meets threshold
    if best_sim >= threshold {
        if let Some(matched_name) = best_name {
            let cause_snippet: String = content.chars().take(200).collect();
            let effect_snippet: String = content.chars().take(500).collect();
            let cause = format!("{}: {}", matched_name, cause_snippet);
            let effect = format!(
                "[auto:{}@{}] {}",
                body.source, body.workstation, effect_snippet
            );
            let sdr_bytes = sdr::to_bytes(&sdr_val);
            let pg_content_hash = engram_content_hash(&cause, &effect);
            let trigger_label = truncate_to_word_boundary(&cause, 80);
            let tags = vec![
                "auto_ingest".to_string(),
                matched_name.clone(),
                format!("workstation:{}", body.workstation),
            ];
            let auto_scope = if body.project.is_some() { "project" } else { "global" };
            let id = engrams::insert_engram_sdr(
                state.store.pool(),
                &engrams::EngramSdrParams {
                    cause: &cause,
                    effect: &effect,
                    sdr_bits: &sdr_bytes,
                    content_hash: &pg_content_hash,
                    tags: &tags,
                    trigger_type: "pattern",
                    trigger_label: &trigger_label,
                    project: body.project.as_deref(),
                    scope: auto_scope,
                },
                MemoryTier::Recall,
            )
            .await?;

            state.sdr_index.insert_scoped(body.project.as_deref(), id, sdr_val);
            let sdr_f32 = sdr::to_bipolar_f32(&sdr_val);
            // Legacy collection
            state
                .vectors
                .upsert("engrams_sdr", id, sdr_f32.clone(), HashMap::new())
                .await?;
            // V2 collection with project payload
            let payload = build_qdrant_payload(body.project.as_deref(), auto_scope);
            state
                .vectors
                .upsert(crate::state::V2_SDR_COLLECTION, id, sdr_f32, payload)
                .await?;

            // Update cooldown and dedup maps
            state
                .cooldown_map
                .insert(body.workstation.clone(), std::time::Instant::now());
            state
                .content_hashes
                .insert(content_hash_hex, std::time::Instant::now());

            tracing::info!(
                engram_id = %id,
                template = %matched_name,
                similarity = %best_sim,
                workstation = %body.workstation,
                "auto_ingest stored"
            );

            return Ok((
                StatusCode::CREATED,
                Json(AutoIngestResponse {
                    stored: true,
                    engram_id: Some(id),
                    matched_template: Some(matched_name),
                    similarity: Some(best_sim),
                    skipped_reason: None,
                }),
            ));
        }
    }

    // Step 9: Below threshold — return without storing
    Ok((
        StatusCode::OK,
        Json(AutoIngestResponse {
            stored: false,
            engram_id: None,
            matched_template: best_name,
            similarity: if best_sim > 0.0 { Some(best_sim) } else { None },
            skipped_reason: Some("below_threshold".into()),
        }),
    ))
}

// ---------------------------------------------------------------------------
// POST /api/v1/smart-ingest  (Sprint 045 — Memory Sidecar v2)
// ---------------------------------------------------------------------------

/// LLM-judged memory ingest endpoint.
///
/// Replaces template matching with a lightweight LLM (LFM2.5 1.2B) that decides
/// whether a code change is worth remembering. Falls back to the existing
/// template-based `auto_ingest` pipeline if the LLM is unavailable.
///
/// Pipeline:
/// 1. Validate content length (>= 50 chars)
/// 2. Per-workstation cooldown gate (5s)
/// 3. SHA-256 content dedup gate (300s window)
/// 4. Call llama-server LLM to classify STORE vs SKIP
/// 5. On STORE: embed → SDR → PG + Qdrant
/// 6. On LLM failure: fall back to template matching via auto_ingest logic

#[derive(Debug, Deserialize)]
pub struct SmartIngestRequest {
    pub content: String,
    pub file_path: String,
    pub workstation: String,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct SmartIngestResponse {
    pub stored: bool,
    pub cause: Option<String>,
    pub effect: Option<String>,
    pub skipped_reason: Option<String>,
}

/// OpenAI-compatible /v1/chat/completions response (non-streaming).
#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionResponse {
    pub(crate) choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatChoice {
    pub(crate) message: ChatMsg,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatMsg {
    pub(crate) content: Option<String>,
}

/// Default model for smart-ingest LLM calls (fallback if saga config has no model).
const DEFAULT_INGEST_MODEL: &str = "hf.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF:Q4_K_M";

/// Smart-ingest unified JSON response from the Saga model.
#[derive(Debug, Deserialize)]
struct SmartIngestLlmResponse {
    store: bool,
    #[serde(default)]
    cause: Option<String>,
    #[serde(default)]
    effect: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    reason: Option<String>,
}

/// Strip `<think>...</think>` tags and extract the first JSON object from LLM output.
fn extract_json(text: &str) -> Option<String> {
    let mut cleaned = text.to_string();
    while let Some(start) = cleaned.find("<think>") {
        if let Some(end) = cleaned.find("</think>") {
            cleaned.replace_range(start..end + "</think>".len(), "");
        } else {
            cleaned.truncate(start);
            break;
        }
    }
    let cleaned = cleaned.trim();
    let start = cleaned.find('{')?;
    let end = cleaned.rfind('}')?;
    if end > start {
        Some(cleaned[start..=end].to_string())
    } else {
        None
    }
}

/// Call an OpenAI-compatible /v1/chat/completions endpoint (Odin, llama-server, etc).
///
/// Returns the raw text response on success, or an error string on failure.
/// Uses the shared `reqwest::Client` from `AppState` — do NOT create per-call clients.
pub(crate) async fn llm_chat_completion(
    client: &reqwest::Client,
    llm_url: &str,
    model: &str,
    prompt: &str,
) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.1,
        "max_tokens": 128,
        "stream": false
    });

    let resp = client
        .post(format!("{}/v1/chat/completions", llm_url))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("llm request failed: {e:?}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("llm returned {status}: {body}"));
    }

    let chat_resp: ChatCompletionResponse = resp
        .json()
        .await
        .map_err(|e| format!("llm response parse failed: {e}"))?;

    chat_resp
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| "empty llm response".to_string())
}

/// Resolve llama-server URL and model from config, with smart-ingest defaults.
fn resolve_llm_config(state: &AppState) -> (String, String) {
    let saga_cfg = state
        .config
        .auto_ingest
        .as_ref()
        .and_then(|c| c.saga.as_ref());

    let llm_url = saga_cfg
        .map(|c| c.llm_url.clone())
        .unwrap_or_else(|| "http://127.0.0.1:8080".to_string());

    // Use saga config model if set, otherwise fall back to default ingest model.
    let model = saga_cfg
        .map(|c| c.model.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| DEFAULT_INGEST_MODEL.to_string());

    (llm_url, model)
}

pub async fn smart_ingest(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SmartIngestRequest>,
) -> Result<(StatusCode, Json<SmartIngestResponse>), MimirError> {
    let skip = |reason: &str| -> (StatusCode, Json<SmartIngestResponse>) {
        (
            StatusCode::OK,
            Json(SmartIngestResponse {
                stored: false,
                cause: None,
                effect: None,
                skipped_reason: Some(reason.into()),
            }),
        )
    };

    // Step 1: Validate content length
    if body.content.trim().len() < 50 {
        return Ok(skip("content_too_short"));
    }

    // Step 2: Per-workstation cooldown gate (5s)
    if let Some(entry) = state.cooldown_map.get(&body.workstation)
        && entry.value().elapsed().as_secs() < 5
    {
        return Ok(skip("cooldown"));
    }

    // Step 3: SHA-256 content dedup gate (300s window)
    let content_hash_hex = format!("{:x}", Sha256::digest(body.content.as_bytes()));
    if let Some(entry) = state.content_hashes.get(&content_hash_hex)
        && entry.value().elapsed().as_secs() < 300
    {
        return Ok(skip("duplicate"));
    }

    // Step 4: Call Saga model to classify STORE vs SKIP and extract structured data
    let (llm_url, model) = resolve_llm_config(&state);
    let content_truncated: String = body.content.chars().take(2000).collect();

    let prompt = format!(
        "You curate memories for a software engineer. Analyze this code change.\n\n\
         Rules:\n\
         - STORE: bugs, architecture decisions, deployment changes, gotchas, user preferences\n\
         - SKIP: formatting, imports, comments, trivial whitespace\n\
         - Include specific details (file names, error messages, flag values)\n\n\
         If STORE, respond as JSON:\n\
         {{\"store\": true, \"cause\": \"what triggered this\", \"effect\": \"what happened\", \"tags\": [\"category\"]}}\n\n\
         If SKIP, respond as JSON:\n\
         {{\"store\": false, \"reason\": \"why\"}}\n\n\
         File: {}\n\
         Change:\n\
         {}",
        body.file_path, content_truncated
    );

    let llm_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        llm_chat_completion(&state.http_client, &llm_url, &model, &prompt),
    )
    .await
    .map_err(|_| "llm call timed out".to_string())
    .and_then(|r| r);

    match llm_result {
        Ok(response) => {
            // Parse JSON response (strip <think> tags if present)
            let json_str = match extract_json(&response) {
                Some(s) => s,
                None => {
                    tracing::warn!(
                        raw = %response.chars().take(200).collect::<String>(),
                        "smart_ingest: no JSON in LLM response, falling back"
                    );
                    return smart_ingest_fallback(state, body, content_hash_hex).await;
                }
            };

            match serde_json::from_str::<SmartIngestLlmResponse>(&json_str) {
                Ok(parsed) if parsed.store => {
                    let cause = parsed.cause.unwrap_or_default();
                    let effect = parsed.effect.unwrap_or_default();

                    if cause.len() < 5 || effect.len() < 5 {
                        tracing::warn!("smart_ingest: LLM returned short cause/effect, falling back");
                        return smart_ingest_fallback(state, body, content_hash_hex).await;
                    }

                    // Step 5: Store the engram with LLM-extracted tags
                    let result = smart_ingest_store(
                        &state,
                        &body,
                        &cause,
                        &effect,
                        &parsed.tags,
                        &content_hash_hex,
                    )
                    .await?;

                    Ok(result)
                }
                Ok(parsed) => {
                    // store == false → SKIP
                    let reason = parsed.reason.unwrap_or_else(|| "llm_skip".to_string());

                    tracing::info!(
                        workstation = %body.workstation,
                        file_path = %body.file_path,
                        reason = %reason,
                        "smart_ingest: Saga decided SKIP"
                    );

                    state
                        .cooldown_map
                        .insert(body.workstation.clone(), std::time::Instant::now());

                    Ok(skip(&reason))
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        raw = %json_str.chars().take(200).collect::<String>(),
                        "smart_ingest: JSON parse failed, falling back"
                    );
                    smart_ingest_fallback(state, body, content_hash_hex).await
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "smart_ingest: Saga call failed, falling back to template matching"
            );
            smart_ingest_fallback(state, body, content_hash_hex).await
        }
    }
}

/// Store an engram from smart-ingest (embed → SDR → PG + Qdrant).
async fn smart_ingest_store(
    state: &Arc<AppState>,
    body: &SmartIngestRequest,
    cause: &str,
    effect: &str,
    llm_tags: &[String],
    content_hash_hex: &str,
) -> Result<(StatusCode, Json<SmartIngestResponse>), MimirError> {
    // Embed cause text
    let embedder = state.embedder.clone();
    let cause_for_embed = cause.to_string();
    let embedding: Vec<f32> =
        tokio::task::spawn_blocking(move || embedder.embed(&cause_for_embed))
            .await
            .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

    let sdr_val = sdr::binarize(&embedding[..sdr::SDR_BITS]);
    let sdr_bytes = sdr::to_bytes(&sdr_val);
    let pg_content_hash = engram_content_hash(cause, effect);
    let trigger_label = truncate_to_word_boundary(cause, 80);

    let mut tags = vec![
        "auto_ingest".to_string(),
        "smart_ingest".to_string(),
        format!("workstation:{}", body.workstation),
        format!("source:{}", body.source),
    ];
    for t in llm_tags {
        if !t.is_empty() && !tags.contains(t) {
            tags.push(t.clone());
        }
    }

    let id = engrams::insert_engram_sdr(
        state.store.pool(),
        &engrams::EngramSdrParams {
            cause,
            effect,
            sdr_bits: &sdr_bytes,
            content_hash: &pg_content_hash,
            tags: &tags,
            trigger_type: "pattern",
            trigger_label: &trigger_label,
            project: None,
            scope: "global",
        },
        MemoryTier::Recall,
    )
    .await?;

    // Insert into in-memory SDR index
    state.sdr_index.insert(id, sdr_val);

    // Upsert to Qdrant (both legacy and v2 collections)
    let sdr_f32 = sdr::to_bipolar_f32(&sdr_val);
    state
        .vectors
        .upsert("engrams_sdr", id, sdr_f32.clone(), HashMap::new())
        .await?;
    let payload = build_qdrant_payload(None, "global");
    state
        .vectors
        .upsert(crate::state::V2_SDR_COLLECTION, id, sdr_f32, payload)
        .await?;

    // Update cooldown and dedup maps
    state
        .cooldown_map
        .insert(body.workstation.clone(), std::time::Instant::now());
    state
        .content_hashes
        .insert(content_hash_hex.to_string(), std::time::Instant::now());

    tracing::info!(
        engram_id = %id,
        workstation = %body.workstation,
        file_path = %body.file_path,
        "smart_ingest stored (LLM classified)"
    );

    Ok((
        StatusCode::CREATED,
        Json(SmartIngestResponse {
            stored: true,
            cause: Some(cause.to_string()),
            effect: Some(effect.to_string()),
            skipped_reason: None,
        }),
    ))
}

/// Fallback to template-based classification when LLM is unavailable.
///
/// Reuses the same dense cosine matching logic as `auto_ingest` but returns
/// a `SmartIngestResponse` instead of `AutoIngestResponse`.
async fn smart_ingest_fallback(
    state: Arc<AppState>,
    body: SmartIngestRequest,
    content_hash_hex: String,
) -> Result<(StatusCode, Json<SmartIngestResponse>), MimirError> {
    let cfg = state.config.auto_ingest.as_ref();
    let threshold = cfg.map(|c| c.template_threshold).unwrap_or(0.3);
    let max_content = cfg.map(|c| c.max_content_length).unwrap_or(4096);

    let content: String = body.content.chars().take(max_content).collect();

    // Embed content
    let embedder = state.embedder.clone();
    let content_for_embed = content.clone();
    let embedding: Vec<f32> =
        tokio::task::spawn_blocking(move || embedder.embed(&content_for_embed))
            .await
            .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

    // Dense cosine template matching
    let mut best_sim: f64 = 0.0;
    let mut best_name: Option<String> = None;
    {
        let dense = state
            .template_embeddings
            .read()
            .map_err(|_| MimirError::Internal("template_embeddings lock poisoned".into()))?;
        for (name, template_emb) in dense.iter() {
            let sim = sdr::dot_similarity(&embedding, template_emb);
            if sim > best_sim {
                best_sim = sim;
                best_name = Some(name.clone());
            }
        }
    }

    let sdr_val = sdr::binarize(&embedding[..sdr::SDR_BITS]);

    if best_sim >= threshold
        && let Some(matched_name) = best_name
    {
        let cause_snippet: String = content.chars().take(200).collect();
        let effect_snippet: String = content.chars().take(500).collect();
        let cause = format!("{}: {}", matched_name, cause_snippet);
        let effect = format!(
            "[auto:{}@{}] {}",
            body.source, body.workstation, effect_snippet
        );
        let sdr_bytes = sdr::to_bytes(&sdr_val);
        let pg_content_hash = engram_content_hash(&cause, &effect);
        let trigger_label = truncate_to_word_boundary(&cause, 80);
        let tags = vec![
            "auto_ingest".to_string(),
            "smart_ingest_fallback".to_string(),
            matched_name.clone(),
            format!("workstation:{}", body.workstation),
        ];

        let id = engrams::insert_engram_sdr(
            state.store.pool(),
            &engrams::EngramSdrParams {
                cause: &cause,
                effect: &effect,
                sdr_bits: &sdr_bytes,
                content_hash: &pg_content_hash,
                tags: &tags,
                trigger_type: "pattern",
                trigger_label: &trigger_label,
                project: None,
                scope: "global",
            },
            MemoryTier::Recall,
        )
        .await?;

        state.sdr_index.insert(id, sdr_val);
        let sdr_f32 = sdr::to_bipolar_f32(&sdr_val);
        state
            .vectors
            .upsert("engrams_sdr", id, sdr_f32.clone(), HashMap::new())
            .await?;
        let payload = build_qdrant_payload(None, "global");
        state
            .vectors
            .upsert(crate::state::V2_SDR_COLLECTION, id, sdr_f32, payload)
            .await?;

        state
            .cooldown_map
            .insert(body.workstation.clone(), std::time::Instant::now());
        state
            .content_hashes
            .insert(content_hash_hex, std::time::Instant::now());

        tracing::info!(
            engram_id = %id,
            template = %matched_name,
            similarity = %best_sim,
            "smart_ingest stored (template fallback)"
        );

        return Ok((
            StatusCode::CREATED,
            Json(SmartIngestResponse {
                stored: true,
                cause: Some(cause),
                effect: Some(effect),
                skipped_reason: None,
            }),
        ));
    }

    // Below threshold
    Ok((
        StatusCode::OK,
        Json(SmartIngestResponse {
            stored: false,
            cause: None,
            effect: None,
            skipped_reason: Some("below_threshold".into()),
        }),
    ))
}

// ---------------------------------------------------------------------------
// POST /api/v1/consolidate  (Sprint 045 — Memory Sidecar v2)
// ---------------------------------------------------------------------------

/// Session memory consolidation ("sleep cycle") endpoint.
///
/// Queries recent engrams for a workstation, sends them to an LLM for
/// consolidation into a concise summary, and stores the summary as a new
/// engram. Designed to be called at the end of a coding session.

#[derive(Debug, Deserialize)]
pub struct ConsolidateRequest {
    pub workstation: String,
    #[serde(default = "default_consolidate_hours")]
    pub hours: Option<u32>,
}

fn default_consolidate_hours() -> Option<u32> {
    Some(12)
}

#[derive(Debug, Serialize)]
pub struct ConsolidateResponse {
    pub summary: String,
    pub engrams_reviewed: usize,
    pub consolidated_id: Option<Uuid>,
}

pub async fn consolidate(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ConsolidateRequest>,
) -> Result<Json<ConsolidateResponse>, MimirError> {
    let hours = body.hours.unwrap_or(12).clamp(1, 168);

    // Step 1: Query recent engrams from PostgreSQL
    let workstation_tag = format!("workstation:{}", body.workstation);
    let rows = sqlx::query_as::<_, ConsolidateRow>(
        r#"
        SELECT id, cause, effect
        FROM yggdrasil.engrams
        WHERE created_at > now() - ($1 * interval '1 hour')
          AND ($2 = ANY(tags) OR 'auto_ingest' = ANY(tags))
        ORDER BY created_at DESC
        LIMIT 20
        "#,
    )
    .bind(hours as f64)
    .bind(&workstation_tag)
    .fetch_all(state.store.pool())
    .await
    .map_err(|e| MimirError::Internal(format!("consolidation query failed: {e}")))?;

    // Step 2: Check minimum count
    if rows.len() < 2 {
        return Ok(Json(ConsolidateResponse {
            summary: "Nothing to consolidate — fewer than 2 recent engrams found.".to_string(),
            engrams_reviewed: rows.len(),
            consolidated_id: None,
        }));
    }

    let engrams_reviewed = rows.len();

    // Step 3: Build LLM prompt
    let memories: String = rows
        .iter()
        .map(|r| format!("- {} -> {}", r.cause, r.effect))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "Review these memories from a coding session. Consolidate them into a concise summary.\n\
         Remove duplicates and noise. Keep the 3-5 most important decisions, changes, or gotchas.\n\
         Output a single paragraph summary.\n\n\
         Memories:\n\
         {}\n\n\
         Summary:",
        memories
    );

    // Step 4: Call LLM via Odin (10s timeout)
    let (llm_url, model) = resolve_llm_config(&state);

    let summary = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        llm_chat_completion(&state.http_client, &llm_url, &model, &prompt),
    )
    .await
    .map_err(|_| "llm call timed out".to_string())
    .and_then(|r| r)
    {
        Ok(text) => {
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                return Err(MimirError::Internal(
                    "consolidation LLM returned empty response".into(),
                ));
            }
            trimmed
        }
        Err(e) => {
            tracing::warn!(error = %e, "consolidation LLM call failed");
            return Err(MimirError::Internal(format!(
                "consolidation LLM unavailable: {e}"
            )));
        }
    };

    // Step 5: Store the consolidated summary as a new engram
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let cause = format!("Session consolidation ({}, {})", body.workstation, today);
    let effect = summary.clone();

    let embedder = state.embedder.clone();
    let cause_for_embed = cause.clone();
    let embedding: Vec<f32> =
        tokio::task::spawn_blocking(move || embedder.embed(&cause_for_embed))
            .await
            .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))?
            .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

    let sdr_val = sdr::binarize(&embedding[..sdr::SDR_BITS]);
    let sdr_bytes = sdr::to_bytes(&sdr_val);
    let pg_content_hash = engram_content_hash(&cause, &effect);
    let trigger_label = truncate_to_word_boundary(&cause, 80);

    let tags = vec![
        "consolidation".to_string(),
        "session_summary".to_string(),
        format!("workstation:{}", body.workstation),
    ];

    let id = engrams::insert_engram_sdr(
        state.store.pool(),
        &engrams::EngramSdrParams {
            cause: &cause,
            effect: &effect,
            sdr_bits: &sdr_bytes,
            content_hash: &pg_content_hash,
            tags: &tags,
            trigger_type: "consolidation",
            trigger_label: &trigger_label,
            project: None,
            scope: "global",
        },
        MemoryTier::Recall,
    )
    .await?;

    // Insert into in-memory SDR index
    state.sdr_index.insert(id, sdr_val);

    // Upsert to Qdrant (both legacy and v2 collections)
    let sdr_f32 = sdr::to_bipolar_f32(&sdr_val);
    state
        .vectors
        .upsert("engrams_sdr", id, sdr_f32.clone(), HashMap::new())
        .await?;
    let payload = build_qdrant_payload(None, "global");
    state
        .vectors
        .upsert(crate::state::V2_SDR_COLLECTION, id, sdr_f32, payload)
        .await?;

    tracing::info!(
        engram_id = %id,
        workstation = %body.workstation,
        engrams_reviewed = engrams_reviewed,
        "consolidation complete"
    );

    Ok(Json(ConsolidateResponse {
        summary,
        engrams_reviewed,
        consolidated_id: Some(id),
    }))
}

/// Row type for consolidation query.
#[derive(sqlx::FromRow)]
struct ConsolidateRow {
    #[allow(dead_code)]
    id: Uuid,
    cause: String,
    effect: String,
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

    // --- Sprint 065 A·P1: partition-prefix tag helpers ---

    #[test]
    fn detect_partition_tags_extracts_sprint_number() {
        assert_eq!(detect_partition_tags("Sprint 065: planning"), vec!["sprint:065"]);
        assert_eq!(detect_partition_tags("sprint-063 retro"), vec!["sprint:063"]);
        assert_eq!(detect_partition_tags("sprint:099 mega"), vec!["sprint:099"]);
        assert_eq!(detect_partition_tags("sprint 001 kickoff"), vec!["sprint:001"]);
    }

    #[test]
    fn detect_partition_tags_ignores_ambient_mentions() {
        assert!(detect_partition_tags("sprinting ahead").is_empty());
        assert!(detect_partition_tags("sprint").is_empty());
        assert!(detect_partition_tags("sprint 9999").is_empty()); // 4 digits
        assert!(detect_partition_tags("sprint a065").is_empty()); // non-digit first
    }

    #[test]
    fn detect_partition_tags_multiple_sprints_in_one_cause() {
        // A cross-sprint reference — both numbers captured, order of first-appearance.
        let tags = detect_partition_tags("merging Sprint 062 into sprint 063");
        assert!(tags.contains(&"sprint:062".to_string()));
        assert!(tags.contains(&"sprint:063".to_string()));
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn detect_partition_tags_dedupes_repeats() {
        let tags = detect_partition_tags("Sprint 065 start; Sprint 065 plan; sprint-065 close");
        assert_eq!(tags, vec!["sprint:065"]);
    }

    #[test]
    fn effective_tags_merges_auto_and_caller() {
        let caller = vec!["decision".to_string(), "core".to_string()];
        let merged = effective_tags(&caller, "Sprint 065: memory P0 fix");
        assert!(merged.contains(&"decision".to_string()));
        assert!(merged.contains(&"core".to_string()));
        assert!(merged.contains(&"sprint:065".to_string()));
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn effective_tags_does_not_duplicate_when_caller_already_has_tag() {
        let caller = vec!["sprint:065".to_string(), "core".to_string()];
        let merged = effective_tags(&caller, "Sprint 065 planning");
        // Should not have two copies of sprint:065.
        assert_eq!(merged.iter().filter(|t| t.to_lowercase() == "sprint:065").count(), 1);
    }

    #[test]
    fn effective_tags_handles_case_insensitive_dedup() {
        let caller = vec!["SPRINT:065".to_string()];
        let merged = effective_tags(&caller, "Sprint 065");
        // Case-insensitive dedup: only the caller's SPRINT:065 remains.
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0], "SPRINT:065");
    }

    #[test]
    fn partition_prefix_tags_filters_to_partition_prefixes_only() {
        let tags = vec![
            "sprint:065".to_string(),
            "decision".to_string(),
            "incident:042".to_string(),
            "release:v1.0".to_string(),
            "coding".to_string(),
        ];
        let filtered = partition_prefix_tags(&tags);
        assert!(filtered.contains(&"sprint:065".to_string()));
        assert!(filtered.contains(&"incident:042".to_string()));
        assert!(filtered.contains(&"release:v1.0".to_string()));
        assert!(!filtered.contains(&"decision".to_string()));
        assert!(!filtered.contains(&"coding".to_string()));
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn partition_prefix_tags_empty_when_no_prefix_tags() {
        let tags = vec!["decision".to_string(), "coding".to_string()];
        assert!(partition_prefix_tags(&tags).is_empty());
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

// ---------------------------------------------------------------------------
// Vault endpoints (Sprint 049)
// ---------------------------------------------------------------------------

/// POST /api/v1/vault — get, set, list, or delete secrets.
///
/// Requires `MIMIR_VAULT_KEY` env var. Returns 503 if vault is not configured.
///
/// Sprint 064 P3: when `MIMIR_VAULT_CLIENT_TOKEN` is set, the request must
/// carry `Authorization: Bearer <token>` matching that env var (constant-time
/// compare). Returns 401 on missing/wrong token. When the env is unset, the
/// endpoint behaves as before (no auth) for backwards-compat.
pub async fn vault_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<VaultRequest>,
) -> Result<Json<serde_json::Value>, MimirError> {
    use ygg_store::postgres::vault;

    crate::vault::verify_vault_auth(&headers)?;

    let vault_key = crate::vault::VaultKey::from_env()
        .map_err(|e| MimirError::Internal(e))?
        .ok_or_else(|| MimirError::Internal(
            "Vault not configured: set MIMIR_VAULT_KEY env var (base64-encoded 32-byte key)".into()
        ))?;

    let scope = body.scope.as_deref().unwrap_or("global");

    match body.action.as_str() {
        "set" => {
            let key_name = body.key.as_deref()
                .ok_or_else(|| MimirError::Validation("key is required for set".into()))?;
            let value = body.value.as_deref()
                .ok_or_else(|| MimirError::Validation("value is required for set".into()))?;

            let encrypted = vault_key.encrypt(value.as_bytes())
                .map_err(|e| MimirError::Internal(e))?;

            let tags = body.tags.as_deref().unwrap_or(&[]);
            let id = vault::set_secret(state.store.pool(), key_name, &encrypted, scope, tags).await?;

            Ok(Json(serde_json::json!({ "id": id, "key": key_name, "scope": scope })))
        }
        "get" => {
            let key_name = body.key.as_deref()
                .ok_or_else(|| MimirError::Validation("key is required for get".into()))?;

            let entry = vault::get_secret(state.store.pool(), key_name, scope).await?
                .ok_or_else(|| MimirError::NotFound(
                    format!("vault key '{key_name}' not found in scope '{scope}'")
                ))?;

            let decrypted = vault_key.decrypt(&entry.encrypted_value)
                .map_err(|e| MimirError::Internal(e))?;

            let value = String::from_utf8(decrypted)
                .map_err(|e| MimirError::Internal(format!("decrypted value is not UTF-8: {e}")))?;

            Ok(Json(serde_json::json!({
                "key": entry.key_name,
                "value": value,
                "scope": entry.scope,
                "tags": entry.tags,
            })))
        }
        "list" => {
            let scope_filter = if scope == "all" { None } else { Some(scope) };
            let entries = vault::list_secrets(state.store.pool(), scope_filter).await?;

            let items: Vec<serde_json::Value> = entries
                .into_iter()
                .map(|e| serde_json::json!({
                    "key": e.key_name,
                    "scope": e.scope,
                    "tags": e.tags,
                    "updated_at": e.updated_at.to_rfc3339(),
                }))
                .collect();

            Ok(Json(serde_json::json!({ "secrets": items, "count": items.len() })))
        }
        "delete" => {
            let key_name = body.key.as_deref()
                .ok_or_else(|| MimirError::Validation("key is required for delete".into()))?;

            let deleted = vault::delete_secret(state.store.pool(), key_name, scope).await?;

            if !deleted {
                return Err(MimirError::NotFound(
                    format!("vault key '{key_name}' not found in scope '{scope}'")
                ));
            }

            Ok(Json(serde_json::json!({ "deleted": key_name, "scope": scope })))
        }
        other => Err(MimirError::Validation(
            format!("unknown vault action '{other}', expected: get, set, list, delete")
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct VaultRequest {
    /// Action: "get", "set", "list", "delete"
    pub action: String,
    /// Secret key name (required for get/set/delete)
    #[serde(default)]
    pub key: Option<String>,
    /// Plaintext value (required for set)
    #[serde(default)]
    pub value: Option<String>,
    /// Scope: "global", "project:xxx", "user:xxx" (default: "global")
    #[serde(default)]
    pub scope: Option<String>,
    /// Tags for categorization
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

// ─────────────────────────────────────────────────────────────────
// Document ingestion (Sprint 056)
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct IngestDocumentRequest {
    pub source_uri: String,
    pub content: String,
    pub doc_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IngestDocumentResponse {
    pub chunks_created: usize,
    pub document_ids: Vec<Uuid>,
}

/// Chunk text into ~512-token segments at paragraph boundaries.
fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for paragraph in text.split("\n\n") {
        let trimmed = paragraph.trim();
        if trimmed.is_empty() {
            continue;
        }
        if current.len() + trimmed.len() + 2 > max_chars && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(trimmed);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    // If no paragraph breaks, split on newlines instead.
    if chunks.is_empty() && !text.trim().is_empty() {
        chunks.push(text.trim().to_string());
    }
    chunks
}

/// `POST /api/v1/documents/ingest` — chunk, embed, and store a document.
pub async fn ingest_document(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IngestDocumentRequest>,
) -> Result<(StatusCode, Json<IngestDocumentResponse>), MimirError> {
    if body.content.trim().is_empty() {
        return Err(MimirError::Validation("content must not be empty".into()));
    }
    if body.source_uri.trim().is_empty() {
        return Err(MimirError::Validation("source_uri must not be empty".into()));
    }

    let chunks = chunk_text(&body.content, 2000);
    let mut document_ids = Vec::with_capacity(chunks.len());

    for (idx, chunk_text) in chunks.iter().enumerate() {
        let chunk_id = Uuid::new_v4();

        // Content hash for dedup.
        let mut hasher = Sha256::new();
        hasher.update(chunk_text.as_bytes());
        let content_hash = hasher.finalize().to_vec();

        // Embed.
        let embedder = state.embedder.clone();
        let text_for_embed = chunk_text.clone();
        let embedding: Vec<f32> =
            tokio::task::spawn_blocking(move || embedder.embed(&text_for_embed))
                .await
                .map_err(|e| MimirError::Internal(format!("embed task panicked: {e}")))?
                .map_err(|e| MimirError::Internal(format!("embedder error: {e}")))?;

        // Insert into PostgreSQL (ON CONFLICT skip for dedup).
        sqlx::query(
            r#"
            INSERT INTO yggdrasil.documents
                (id, source_uri, doc_type, title, chunk_index, content, content_hash, project)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (content_hash) DO NOTHING
            "#,
        )
        .bind(chunk_id)
        .bind(&body.source_uri)
        .bind(&body.doc_type)
        .bind(&body.title)
        .bind(idx as i32)
        .bind(chunk_text)
        .bind(&content_hash)
        .bind(&body.project)
        .execute(state.store.pool())
        .await
        .map_err(|e| MimirError::Internal(format!("pg insert failed: {e}")))?;

        // Upsert to Qdrant.
        let mut payload = HashMap::new();
        payload.insert("source_uri".to_string(), Value::from(body.source_uri.clone()));
        payload.insert("doc_type".to_string(), Value::from(body.doc_type.clone()));
        if let Some(ref p) = body.project {
            payload.insert("project".to_string(), Value::from(p.clone()));
        }
        state.vectors.upsert("research_documents", chunk_id, embedding, payload).await
            .map_err(|e| MimirError::Internal(format!("qdrant upsert failed: {e}")))?;

        document_ids.push(chunk_id);
    }

    tracing::info!(
        source_uri = %body.source_uri,
        chunks = document_ids.len(),
        "document ingested"
    );

    Ok((
        StatusCode::CREATED,
        Json(IngestDocumentResponse {
            chunks_created: document_ids.len(),
            document_ids,
        }),
    ))
}
