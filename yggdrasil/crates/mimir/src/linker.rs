//! Automatic graph linking for newly stored engrams (Sprint 055).
//!
//! After an engram is stored, `spawn_link_engram` fires as a fire-and-forget
//! async task. It:
//! 1. Queries Qdrant for the top-3 nearest neighbors (excluding self).
//! 2. Fetches their cause/effect text from PostgreSQL.
//! 3. Pushes a spine task (label: "linker") with a prompt asking the LLM to
//!    determine relationship types between the new engram and its neighbors.
//!
//! The linker spine worker processes the task and commits edges via the
//! graph_link handler. If no spine worker is running, the task sits in the
//! queue until one is available (or expires via TTL).

use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use crate::state::AppState;
use crate::sdr;
use ygg_store::postgres::engrams;

/// Spawn an async task to find neighbors and push a linker spine task.
///
/// This is fire-and-forget — all errors are logged and swallowed.
pub fn spawn_link_engram(
    state: Arc<AppState>,
    engram_id: Uuid,
    cause: String,
    effect: String,
    sdr_val: sdr::Sdr,
) {
    tokio::spawn(async move {
        if let Err(e) = link_engram(state, engram_id, cause, effect, sdr_val).await {
            tracing::warn!(error = %e, engram_id = %engram_id, "graph linking failed");
        }
    });
}

async fn link_engram(
    state: Arc<AppState>,
    engram_id: Uuid,
    cause: String,
    effect: String,
    sdr_val: sdr::Sdr,
) -> Result<(), String> {
    // Step 1: Query Qdrant for top-3 neighbors (using bipolar f32 SDR)
    let sdr_f32 = sdr::to_bipolar_f32(&sdr_val);
    let results = state
        .vectors
        .search(crate::state::V2_SDR_COLLECTION, sdr_f32, 4)
        .await
        .map_err(|e| format!("qdrant search failed: {e}"))?;

    // Filter out self and low-similarity results
    let neighbors: Vec<(Uuid, f32)> = results
        .into_iter()
        .filter(|(id, sim)| *id != engram_id && *sim > 0.7)
        .take(3)
        .collect();

    if neighbors.is_empty() {
        return Ok(()); // no similar engrams to link
    }

    // Step 2: Fetch neighbor cause/effect from PostgreSQL
    let neighbor_ids: Vec<Uuid> = neighbors.iter().map(|(id, _)| *id).collect();
    let sim_map: HashMap<Uuid, f64> = neighbors
        .iter()
        .map(|(id, sim)| (*id, *sim as f64))
        .collect();
    let neighbor_engrams =
        engrams::fetch_engrams_by_ids(state.store.pool(), &neighbor_ids, &sim_map)
            .await
            .map_err(|e| format!("fetch neighbors failed: {e}"))?;

    if neighbor_engrams.is_empty() {
        return Ok(());
    }

    // Step 3: Build context for the linker spine task
    let mut existing_list = String::new();
    for (i, eng) in neighbor_engrams.iter().enumerate() {
        existing_list.push_str(&format!(
            "{}. [ID: {}] Cause: {} | Effect: {}\n",
            i + 1,
            eng.id,
            eng.cause.chars().take(200).collect::<String>(),
            eng.effect.chars().take(200).collect::<String>(),
        ));
    }

    let context = serde_json::json!({
        "engram_id": engram_id.to_string(),
        "cause": cause.chars().take(200).collect::<String>(),
        "effect": effect.chars().take(200).collect::<String>(),
        "neighbors": existing_list,
        "neighbor_ids": neighbor_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        "prompt": format!(
            "Given these memories, determine relationships.\n\n\
             New memory:\n  Cause: {}\n  Effect: {}\n\n\
             Existing memories:\n{}\n\
             For each related pair, output a JSON array of objects:\n\
             [{{\"target_id\": \"UUID\", \"relation\": \"TYPE\", \"weight\": 0.0-1.0}}]\n\n\
             Valid relations: references, contradicts, temporal_next, semantic_cluster, depends_on, caused_by\n\
             Output ONLY related pairs. Empty array [] if none are related.",
            cause.chars().take(200).collect::<String>(),
            effect.chars().take(200).collect::<String>(),
            existing_list
        )
    });

    // Step 4: Push to spine queue with "linker" label
    ygg_store::postgres::tasks::spine_push(
        state.store.pool(),
        "graph_link",
        "linker",
        &context,
        0, // normal priority
        Some(300), // 5 minute TTL
    )
    .await
    .map_err(|e| format!("spine push failed: {e}"))?;

    tracing::debug!(
        engram_id = %engram_id,
        neighbor_count = neighbors.len(),
        "linker spine task pushed"
    );

    Ok(())
}
