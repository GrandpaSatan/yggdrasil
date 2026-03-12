//! In-memory SDR index for System 1 (hot/fast) recall.
//!
//! Stores SDRs in a contiguous `Vec` protected by `RwLock` for concurrent
//! access. Queries perform a brute-force XOR + popcount scan. For 1000
//! Recall-tier engrams × 4 u64 words, this completes in ~4μs.

use std::sync::RwLock;
use uuid::Uuid;

use crate::sdr::{self, Sdr};

/// Thread-safe in-memory SDR index.
///
/// Read-biased: queries take a read lock, stores take a write lock.
/// Queries are vastly more frequent than stores, so write starvation
/// is negligible given sub-millisecond scan times.
pub struct SdrIndex {
    entries: RwLock<Vec<(Uuid, Sdr)>>,
}

impl SdrIndex {
    /// Create an empty index.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }

    /// Insert an SDR for the given engram ID.
    pub fn insert(&self, id: Uuid, sdr: Sdr) {
        let mut entries = self.entries.write().unwrap();
        entries.push((id, sdr));
    }

    /// Remove all entries for the given engram ID.
    pub fn remove(&self, id: Uuid) {
        let mut entries = self.entries.write().unwrap();
        entries.retain(|(eid, _)| *eid != id);
    }

    /// Query the index for the top-K most similar SDRs by Hamming distance.
    ///
    /// Returns `(engram_id, similarity)` pairs sorted by descending similarity.
    pub fn query(&self, target: &Sdr, limit: usize) -> Vec<(Uuid, f64)> {
        let entries = self.entries.read().unwrap();

        if entries.is_empty() || limit == 0 {
            return Vec::new();
        }

        // Brute-force scan: compute Hamming similarity for every entry.
        let mut scored: Vec<(Uuid, f64)> = entries
            .iter()
            .map(|(id, stored_sdr)| (*id, sdr::hamming_similarity(target, stored_sdr)))
            .collect();

        // Sort by similarity descending, then by UUID for deterministic ordering.
        scored.sort_by(|(id_a, sim_a), (id_b, sim_b)| {
            sim_b
                .partial_cmp(sim_a)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| id_a.cmp(id_b))
        });

        scored.truncate(limit);
        scored
    }

    /// Number of entries in the index.
    pub fn len(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bulk load from PostgreSQL rows: `(id, sdr_bits BYTEA)`.
    ///
    /// Called once at startup to populate the index from persisted data.
    pub fn load_from_rows(&self, rows: &[(Uuid, Vec<u8>)]) {
        let mut entries = self.entries.write().unwrap();
        entries.reserve(rows.len());
        for (id, bytes) in rows {
            if bytes.len() >= sdr::SDR_WORDS * 8 {
                entries.push((*id, sdr::from_bytes(bytes)));
            } else {
                tracing::warn!(
                    engram_id = %id,
                    bytes_len = bytes.len(),
                    "skipping SDR with invalid byte length"
                );
            }
        }
    }
}

/// Aggregate statistics about the SDR index for health monitoring.
#[derive(Debug, Clone)]
pub struct SdrStats {
    /// Total entries in the index.
    pub count: usize,
    /// Average popcount across all SDRs (healthy: ~110-140 for 256-bit SDRs).
    pub avg_popcount: f64,
    /// Popcount of OR-ing all SDRs together (concept coverage, max 256).
    pub concept_coverage: u32,
    /// Sampled pairwise similarity percentiles (p50, p90, p99).
    /// Empty if fewer than 2 entries.
    pub similarity_p50: f64,
    pub similarity_p90: f64,
}

impl SdrIndex {
    /// Compute aggregate health statistics for the index.
    ///
    /// Samples up to 200 random pairs for pairwise similarity estimation.
    /// Runs under a read lock — safe to call from a periodic background task.
    pub fn stats(&self) -> SdrStats {
        let entries = self.entries.read().unwrap();
        let count = entries.len();

        if count == 0 {
            return SdrStats {
                count: 0,
                avg_popcount: 0.0,
                concept_coverage: 0,
                similarity_p50: 0.0,
                similarity_p90: 0.0,
            };
        }

        // Average popcount
        let total_pop: u64 = entries.iter().map(|(_, s)| sdr::popcount(s) as u64).sum();
        let avg_popcount = total_pop as f64 / count as f64;

        // Concept coverage: OR all SDRs together
        let mut global_or = sdr::ZERO;
        for (_, s) in entries.iter() {
            for i in 0..sdr::SDR_WORDS {
                global_or[i] |= s[i];
            }
        }
        let concept_coverage = sdr::popcount(&global_or);

        // Sampled pairwise similarity
        let (p50, p90) = if count >= 2 {
            let max_pairs = 200usize;
            let mut sims = Vec::with_capacity(max_pairs);

            // Deterministic pseudo-random sampling using index-based hashing
            let step = if count > 20 { count / 20 } else { 1 };
            'outer: for i in (0..count).step_by(step) {
                for j in (i + 1..count).step_by(step) {
                    sims.push(sdr::hamming_similarity(&entries[i].1, &entries[j].1));
                    if sims.len() >= max_pairs {
                        break 'outer;
                    }
                }
            }

            if sims.is_empty() {
                (0.0, 0.0)
            } else {
                sims.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let p50_idx = sims.len() / 2;
                let p90_idx = (sims.len() * 9) / 10;
                (sims[p50_idx], sims[p90_idx.min(sims.len() - 1)])
            }
        } else {
            (0.0, 0.0)
        };

        SdrStats {
            count,
            avg_popcount,
            concept_coverage,
            similarity_p50: p50,
            similarity_p90: p90,
        }
    }
}

impl Default for SdrIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sdr(val: u64) -> Sdr {
        [val, val, val, val]
    }

    #[test]
    fn insert_and_query() {
        let index = SdrIndex::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        index.insert(id1, make_sdr(0xFF));
        index.insert(id2, make_sdr(0xF0));

        let results = index.query(&make_sdr(0xFF), 2);
        assert_eq!(results.len(), 2);
        // First result should be id1 (exact match)
        assert_eq!(results[0].0, id1);
        assert_eq!(results[0].1, 1.0);
    }

    #[test]
    fn remove_entry() {
        let index = SdrIndex::new();
        let id = Uuid::new_v4();
        index.insert(id, make_sdr(0xFF));
        assert_eq!(index.len(), 1);

        index.remove(id);
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn empty_query_returns_empty() {
        let index = SdrIndex::new();
        let results = index.query(&make_sdr(0xFF), 5);
        assert!(results.is_empty());
    }

    #[test]
    fn load_from_rows() {
        let index = SdrIndex::new();
        let id = Uuid::new_v4();
        let sdr_val: Sdr = [0xDEAD, 0xBEEF, 0xCAFE, 0x1234];
        let bytes = sdr::to_bytes(&sdr_val);

        index.load_from_rows(&[(id, bytes)]);
        assert_eq!(index.len(), 1);

        let results = index.query(&sdr_val, 1);
        assert_eq!(results[0].0, id);
        assert_eq!(results[0].1, 1.0);
    }

    // --- stats() tests ---

    #[test]
    fn stats_empty_index() {
        let index = SdrIndex::new();
        let stats = index.stats();
        assert_eq!(stats.count, 0);
        assert_eq!(stats.avg_popcount, 0.0);
        assert_eq!(stats.concept_coverage, 0);
        assert_eq!(stats.similarity_p50, 0.0);
        assert_eq!(stats.similarity_p90, 0.0);
    }

    #[test]
    fn stats_single_entry() {
        let index = SdrIndex::new();
        let sdr_val: Sdr = [0xFF, 0, 0, 0]; // 8 bits set
        index.insert(Uuid::new_v4(), sdr_val);

        let stats = index.stats();
        assert_eq!(stats.count, 1);
        assert_eq!(stats.avg_popcount, 8.0);
        assert_eq!(stats.concept_coverage, 8); // OR of one SDR = itself
        // No pairwise similarity with only 1 entry
        assert_eq!(stats.similarity_p50, 0.0);
    }

    #[test]
    fn stats_identical_entries_have_full_similarity() {
        let index = SdrIndex::new();
        let sdr_val: Sdr = [0xFF, 0xFF, 0xFF, 0xFF]; // 32 bits set
        index.insert(Uuid::new_v4(), sdr_val);
        index.insert(Uuid::new_v4(), sdr_val);
        index.insert(Uuid::new_v4(), sdr_val);

        let stats = index.stats();
        assert_eq!(stats.count, 3);
        assert_eq!(stats.avg_popcount, 32.0);
        assert_eq!(stats.concept_coverage, 32); // All identical → OR = same
        // All pairwise similarities should be 1.0
        assert_eq!(stats.similarity_p50, 1.0);
        assert_eq!(stats.similarity_p90, 1.0);
    }

    #[test]
    fn stats_disjoint_entries_have_low_similarity() {
        let index = SdrIndex::new();
        // Two SDRs with no overlapping bits
        let a: Sdr = [u64::MAX, 0, 0, 0]; // 64 bits in word 0
        let b: Sdr = [0, u64::MAX, 0, 0]; // 64 bits in word 1
        index.insert(Uuid::new_v4(), a);
        index.insert(Uuid::new_v4(), b);

        let stats = index.stats();
        assert_eq!(stats.count, 2);
        assert_eq!(stats.avg_popcount, 64.0);
        assert_eq!(stats.concept_coverage, 128); // OR covers both words
        // Hamming similarity = 1 - 128/256 = 0.5 (128 bits differ)
        assert_eq!(stats.similarity_p50, 0.5);
    }

    #[test]
    fn stats_concept_coverage_accumulates() {
        let index = SdrIndex::new();
        // 4 SDRs each activating a different word
        index.insert(Uuid::new_v4(), [0xFF, 0, 0, 0]);      // 8 bits in word 0
        index.insert(Uuid::new_v4(), [0, 0xFF, 0, 0]);      // 8 bits in word 1
        index.insert(Uuid::new_v4(), [0, 0, 0xFF, 0]);      // 8 bits in word 2
        index.insert(Uuid::new_v4(), [0, 0, 0, 0xFF]);      // 8 bits in word 3

        let stats = index.stats();
        assert_eq!(stats.count, 4);
        assert_eq!(stats.avg_popcount, 8.0);
        assert_eq!(stats.concept_coverage, 32); // 8 bits × 4 words
    }

    // --- novelty gate pattern test ---

    #[test]
    fn novelty_gate_detects_near_duplicates() {
        let index = SdrIndex::new();
        let sdr_a: Sdr = [u64::MAX, u64::MAX, u64::MAX, u64::MAX]; // all bits set
        index.insert(Uuid::new_v4(), sdr_a);

        // A nearly-identical SDR should have very high similarity
        let sdr_b: Sdr = [u64::MAX, u64::MAX, u64::MAX, u64::MAX ^ 0x1]; // 1 bit different
        let results = index.query(&sdr_b, 1);
        assert_eq!(results.len(), 1);
        let similarity = results[0].1;
        // 1 bit out of 256 different → similarity = 255/256 ≈ 0.996
        assert!(similarity > 0.99, "expected > 0.99, got {similarity}");
        assert!(similarity > 0.90, "novelty gate (threshold 0.90) should catch this");
    }

    #[test]
    fn novelty_gate_passes_distinct_content() {
        let index = SdrIndex::new();
        let sdr_a: Sdr = [u64::MAX, 0, 0, 0]; // word 0 all set
        index.insert(Uuid::new_v4(), sdr_a);

        // Completely different SDR
        let sdr_b: Sdr = [0, 0, 0, u64::MAX]; // word 3 all set
        let results = index.query(&sdr_b, 1);
        assert_eq!(results.len(), 1);
        let similarity = results[0].1;
        // 128 bits differ → similarity = 0.5
        assert!(similarity < 0.90, "distinct content should pass novelty gate, got {similarity}");
    }
}
