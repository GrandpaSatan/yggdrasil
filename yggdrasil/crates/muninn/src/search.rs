use uuid::Uuid;
use ygg_domain::chunk::{SearchResult, SearchSource};
use ygg_store::postgres::chunks::{get_chunks_by_ids, search_bm25};

use crate::{
    error::MuninnError,
    fusion::reciprocal_rank_fusion,
    state::AppState,
};

/// Execute hybrid search: embed query, run vector + BM25 in parallel,
/// fuse with RRF, batch-fetch full chunks from PostgreSQL, and return
/// ranked results.
///
/// This function owns the complete search pipeline. No other Muninn module
/// touches the database, vector store, or embedder.
///
/// Tracing spans are emitted for:
/// - Full function wall-clock time (excluding caller's embedding measurement)
/// - Individual parallel search legs
/// - RRF fusion
/// - Batch chunk fetch
pub async fn hybrid_search(
    state: &AppState,
    query: &str,
    limit: usize,
    languages: Option<&[String]>,
) -> Result<Vec<SearchResult>, MuninnError> {
    let span = tracing::info_span!("hybrid_search", query = %query, limit = limit);
    let _enter = span.enter();

    // --- Step 1: Embed the query text ---
    let embedding_start = std::time::Instant::now();
    let query_embedding = state.embedder.embed_single(query).await?;
    tracing::debug!(elapsed_ms = embedding_start.elapsed().as_millis(), "query embedded");

    // Candidate count: fetch 3x limit from each backend to give RRF enough overlap.
    let candidate_limit = limit * 3;

    // --- Step 2: Run vector search and BM25 search in parallel ---
    let search_start = std::time::Instant::now();

    let qdrant_timed = async {
        let qdrant_start = std::time::Instant::now();
        let result = state
            .vectors
            .search("code_chunks", query_embedding, candidate_limit as u64)
            .await;
        crate::metrics::record_qdrant_duration("search", qdrant_start.elapsed().as_secs_f64());
        result
    };

    let (vector_result, bm25_result) = tokio::join!(
        qdrant_timed,
        search_bm25(&state.pool, query, candidate_limit, languages),
    );

    let vector_results = vector_result?;
    let bm25_results = bm25_result?;

    tracing::debug!(
        elapsed_ms = search_start.elapsed().as_millis(),
        vector_hits = vector_results.len(),
        bm25_hits = bm25_results.len(),
        "parallel search complete"
    );

    // --- Step 3: Fuse results with Reciprocal Rank Fusion ---
    let fusion_start = std::time::Instant::now();
    let fused = reciprocal_rank_fusion(&vector_results, &bm25_results, state.search_config.rrf_k);
    tracing::debug!(
        elapsed_us = fusion_start.elapsed().as_micros(),
        fused_candidates = fused.len(),
        "rrf fusion complete"
    );

    // --- Step 4: Take top `limit` candidates ---
    let top: Vec<(Uuid, f64)> = fused.into_iter().take(limit).collect();
    if top.is_empty() {
        tracing::debug!("no candidates after fusion");
        return Ok(vec![]);
    }

    // --- Step 5: Batch fetch full chunks from PostgreSQL ---
    let ids: Vec<Uuid> = top.iter().map(|(id, _)| *id).collect();

    let fetch_start = std::time::Instant::now();
    let chunks = get_chunks_by_ids(&state.pool, &ids).await?;
    tracing::debug!(
        elapsed_ms = fetch_start.elapsed().as_millis(),
        fetched = chunks.len(),
        requested = ids.len(),
        "batch chunk fetch complete"
    );

    // Build a lookup map: UUID -> CodeChunk (O(n) scan is fine for n <= 50).
    let chunk_map: std::collections::HashMap<Uuid, ygg_domain::chunk::CodeChunk> =
        chunks.into_iter().map(|c| (c.id, c)).collect();

    // --- Step 6: Assemble SearchResult vec preserving fused rank order ---
    let mut results = Vec::with_capacity(top.len());
    for (id, rrf_score) in &top {
        match chunk_map.get(id) {
            Some(chunk) => {
                results.push(SearchResult {
                    chunk: chunk.clone(),
                    score: *rrf_score,
                    source: SearchSource::Fused,
                });
            }
            None => {
                // Stale Qdrant point: the chunk was deleted from PostgreSQL after Qdrant
                // returned it. This is a benign race during Huginn re-indexing — skip it.
                tracing::warn!(chunk_id = %id, "stale qdrant id: chunk not found in postgresql, skipping");
            }
        }
    }

    tracing::info!(
        results = results.len(),
        "hybrid search complete"
    );

    Ok(results)
}
