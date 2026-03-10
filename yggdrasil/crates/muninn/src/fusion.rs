use std::collections::HashMap;

use uuid::Uuid;

/// Merge vector search and BM25 search results using Reciprocal Rank Fusion (RRF).
///
/// Each input is a ranked list of `(id, original_score)` pairs, ordered by relevance
/// (index 0 = rank 1, i.e. highest relevance first). Original scores from Qdrant
/// (cosine similarity) and PostgreSQL (`ts_rank`) are **not** used in the computation —
/// only the ordinal rank matters.
///
/// RRF formula: `rrf_score(d) = sum over all rankings R_i of: 1 / (k + rank_i(d))`
///
/// Where `rank_i(d)` is the 1-based position of document `d` in ranking `R_i`.
/// If `d` does not appear in ranking `R_i`, that term contributes 0.
///
/// Standard value for `k` is 60.0 (Cormack, Clarke, Buettcher 2009). The sprint
/// configures it via `SearchConfig::rrf_k`.
///
/// Returns a deduplicated `Vec<(Uuid, f64)>` sorted descending by RRF score.
/// Ties are broken deterministically by UUID ordering.
pub fn reciprocal_rank_fusion(
    vector_results: &[(Uuid, f32)],
    bm25_results: &[(Uuid, f64)],
    k: f64,
) -> Vec<(Uuid, f64)> {
    let mut scores: HashMap<Uuid, f64> = HashMap::new();

    // Accumulate contributions from the vector ranking.
    for (i, (id, _score)) in vector_results.iter().enumerate() {
        let rank = (i + 1) as f64;
        *scores.entry(*id).or_insert(0.0) += 1.0 / (k + rank);
    }

    // Accumulate contributions from the BM25 ranking.
    for (i, (id, _score)) in bm25_results.iter().enumerate() {
        let rank = (i + 1) as f64;
        *scores.entry(*id).or_insert(0.0) += 1.0 / (k + rank);
    }

    let mut fused: Vec<(Uuid, f64)> = scores.into_iter().collect();

    // Sort descending by RRF score; break ties by UUID for deterministic output.
    fused.sort_by(|(id_a, score_a), (id_b, score_b)| {
        score_b
            .partial_cmp(score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| id_a.cmp(id_b))
    });

    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_inputs_return_empty() {
        let result = reciprocal_rank_fusion(&[], &[], 60.0);
        assert!(result.is_empty());
    }

    #[test]
    fn vector_only_results() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let vector = vec![(id_a, 0.9_f32), (id_b, 0.7_f32)];
        let result = reciprocal_rank_fusion(&vector, &[], 60.0);
        assert_eq!(result.len(), 2);
        // id_a is rank 1, so its score should be higher.
        assert_eq!(result[0].0, id_a);
        assert!(result[0].1 > result[1].1);
    }

    #[test]
    fn bm25_only_results() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let bm25 = vec![(id_a, 1.5_f64), (id_b, 0.8_f64)];
        let result = reciprocal_rank_fusion(&[], &bm25, 60.0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, id_a);
    }

    #[test]
    fn fused_scores_are_additive() {
        let shared_id = Uuid::new_v4();
        let vector_only_id = Uuid::new_v4();
        let bm25_only_id = Uuid::new_v4();

        // shared_id appears at rank 1 in both — it should have the highest fused score.
        let vector = vec![(shared_id, 0.95_f32), (vector_only_id, 0.80_f32)];
        let bm25 = vec![(shared_id, 1.5_f64), (bm25_only_id, 0.9_f64)];

        let result = reciprocal_rank_fusion(&vector, &bm25, 60.0);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, shared_id);

        // shared_id score = 1/(60+1) + 1/(60+1) = 2/61 ≈ 0.03279
        let expected = 2.0 / 61.0;
        let actual = result[0].1;
        assert!((actual - expected).abs() < 1e-10, "expected {expected}, got {actual}");
    }

    #[test]
    fn sort_is_descending() {
        let ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        let vector: Vec<(Uuid, f32)> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, 1.0 - i as f32 * 0.1))
            .collect();
        let result = reciprocal_rank_fusion(&vector, &[], 60.0);
        for window in result.windows(2) {
            assert!(
                window[0].1 >= window[1].1,
                "scores not sorted descending: {} < {}",
                window[0].1,
                window[1].1
            );
        }
    }
}
