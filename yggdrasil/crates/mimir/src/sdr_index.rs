//! In-memory SDR index for System 1 (hot/fast) recall.
//!
//! Stores SDRs in per-project partitions protected by `RwLock` for concurrent
//! access. Queries perform a brute-force XOR + popcount scan. For 1000
//! Recall-tier engrams × 4 u64 words, this completes in ~4μs.
//!
//! Partitioning by project avoids scanning irrelevant engrams during
//! project-scoped queries, saving CPU cycles proportional to the number
//! of off-project engrams.

use std::collections::HashMap;
use std::sync::RwLock;
use uuid::Uuid;

use crate::sdr::{self, Sdr};

/// The partition key used for engrams with no project (global scope).
const GLOBAL_PARTITION: &str = "__global__";

/// Thread-safe in-memory SDR index, partitioned by project.
///
/// Read-biased: queries take a read lock, stores take a write lock.
/// Each partition is a contiguous `Vec<(Uuid, Sdr)>` — near-zero memory
/// overhead per partition since SDRs are just 32 bytes each.
pub struct SdrIndex {
    partitions: RwLock<HashMap<String, Vec<(Uuid, Sdr)>>>,
}

impl SdrIndex {
    /// Create an empty index with no partitions.
    pub fn new() -> Self {
        Self {
            partitions: RwLock::new(HashMap::new()),
        }
    }

    /// Insert an SDR into a specific project partition.
    pub fn insert_scoped(&self, project: Option<&str>, id: Uuid, sdr: Sdr) {
        let key = project.unwrap_or(GLOBAL_PARTITION);
        let mut partitions = self.partitions.write().unwrap();
        partitions
            .entry(key.to_string())
            .or_default()
            .push((id, sdr));
    }

    /// Insert into the global partition (backward compat).
    pub fn insert(&self, id: Uuid, sdr: Sdr) {
        self.insert_scoped(None, id, sdr);
    }

    /// Remove an engram from ALL partitions (handles project reassignment).
    pub fn remove(&self, id: Uuid) {
        let mut partitions = self.partitions.write().unwrap();
        for entries in partitions.values_mut() {
            entries.retain(|(eid, _)| *eid != id);
        }
    }

    /// Query a specific project partition + global partition, merge and return top-K.
    ///
    /// This is the primary query path for project-scoped searches. It scans
    /// only the project and global partitions, skipping all other projects.
    pub fn query_scoped(
        &self,
        target: &Sdr,
        project: &str,
        include_global: bool,
        limit: usize,
    ) -> Vec<(Uuid, f64)> {
        let partitions = self.partitions.read().unwrap();
        if limit == 0 {
            return Vec::new();
        }

        let mut scored: Vec<(Uuid, f64)> = Vec::new();

        // Scan project partition
        if let Some(entries) = partitions.get(project) {
            scored.extend(
                entries
                    .iter()
                    .map(|(id, s)| (*id, sdr::hamming_similarity(target, s))),
            );
        }

        // Scan global partition
        if include_global {
            if let Some(entries) = partitions.get(GLOBAL_PARTITION) {
                scored.extend(
                    entries
                        .iter()
                        .map(|(id, s)| (*id, sdr::hamming_similarity(target, s))),
                );
            }
        }

        scored.sort_by(|(id_a, sim_a), (id_b, sim_b)| {
            sim_b
                .partial_cmp(sim_a)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| id_a.cmp(id_b))
        });

        scored.truncate(limit);
        scored
    }

    /// Query ALL partitions (backward compat / unscoped search).
    pub fn query(&self, target: &Sdr, limit: usize) -> Vec<(Uuid, f64)> {
        let partitions = self.partitions.read().unwrap();

        if limit == 0 {
            return Vec::new();
        }

        let mut scored: Vec<(Uuid, f64)> = Vec::new();
        for entries in partitions.values() {
            scored.extend(
                entries
                    .iter()
                    .map(|(id, s)| (*id, sdr::hamming_similarity(target, s))),
            );
        }

        scored.sort_by(|(id_a, sim_a), (id_b, sim_b)| {
            sim_b
                .partial_cmp(sim_a)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| id_a.cmp(id_b))
        });

        scored.truncate(limit);
        scored
    }

    /// Total number of entries across all partitions.
    pub fn len(&self) -> usize {
        self.partitions
            .read()
            .unwrap()
            .values()
            .map(|v| v.len())
            .sum()
    }

    /// Whether the index has zero entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bulk load from PostgreSQL rows: `(id, sdr_bits BYTEA, project)`.
    ///
    /// Called once at startup to populate the index from persisted data.
    pub fn load_from_rows_scoped(&self, rows: &[(Uuid, Vec<u8>, Option<String>)]) {
        let mut partitions = self.partitions.write().unwrap();
        for (id, bytes, project) in rows {
            if bytes.len() >= sdr::SDR_WORDS * 8 {
                let key = project.as_deref().unwrap_or(GLOBAL_PARTITION);
                partitions
                    .entry(key.to_string())
                    .or_default()
                    .push((*id, sdr::from_bytes(bytes)));
            } else {
                tracing::warn!(
                    engram_id = %id,
                    bytes_len = bytes.len(),
                    "skipping SDR with invalid byte length"
                );
            }
        }
    }

    /// Bulk load from PostgreSQL rows without project info (backward compat).
    ///
    /// All entries go into the global partition.
    pub fn load_from_rows(&self, rows: &[(Uuid, Vec<u8>)]) {
        let mut partitions = self.partitions.write().unwrap();
        let entries = partitions.entry(GLOBAL_PARTITION.to_string()).or_default();
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

    /// Number of partitions (projects + global).
    pub fn partition_count(&self) -> usize {
        self.partitions.read().unwrap().len()
    }

    /// Number of entries in a specific partition.
    pub fn partition_len(&self, project: Option<&str>) -> usize {
        let key = project.unwrap_or(GLOBAL_PARTITION);
        self.partitions
            .read()
            .unwrap()
            .get(key)
            .map(|v| v.len())
            .unwrap_or(0)
    }
}

/// Aggregate statistics about the SDR index for health monitoring.
#[derive(Debug, Clone)]
pub struct SdrStats {
    /// Total entries across all partitions.
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
    /// Compute aggregate health statistics across all partitions.
    ///
    /// Samples up to 200 random pairs for pairwise similarity estimation.
    /// Runs under a read lock — safe to call from a periodic background task.
    pub fn stats(&self) -> SdrStats {
        let partitions = self.partitions.read().unwrap();

        // Flatten all entries for stats computation
        let all_entries: Vec<&(Uuid, Sdr)> = partitions.values().flat_map(|v| v.iter()).collect();
        let count = all_entries.len();

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
        let total_pop: u64 = all_entries.iter().map(|(_, s)| sdr::popcount(s) as u64).sum();
        let avg_popcount = total_pop as f64 / count as f64;

        // Concept coverage: OR all SDRs together
        let mut global_or = sdr::ZERO;
        for (_, s) in all_entries.iter() {
            for i in 0..sdr::SDR_WORDS {
                global_or[i] |= s[i];
            }
        }
        let concept_coverage = sdr::popcount(&global_or);

        // Sampled pairwise similarity
        let (p50, p90) = if count >= 2 {
            let max_pairs = 200usize;
            let mut sims = Vec::with_capacity(max_pairs);

            let step = if count > 20 { count / 20 } else { 1 };
            'outer: for i in (0..count).step_by(step) {
                for j in (i + 1..count).step_by(step) {
                    sims.push(sdr::hamming_similarity(&all_entries[i].1, &all_entries[j].1));
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
        assert_eq!(results[0].0, id1);
        assert_eq!(results[0].1, 1.0);
    }

    #[test]
    fn insert_scoped_and_query_scoped() {
        let index = SdrIndex::new();
        let id_ygg = Uuid::new_v4();
        let id_fen = Uuid::new_v4();
        let id_global = Uuid::new_v4();

        index.insert_scoped(Some("yggdrasil"), id_ygg, make_sdr(0xFF));
        index.insert_scoped(Some("fenrir"), id_fen, make_sdr(0xFF));
        index.insert_scoped(None, id_global, make_sdr(0xFF));

        // Query yggdrasil + global — should NOT see fenrir
        let results = index.query_scoped(&make_sdr(0xFF), "yggdrasil", true, 10);
        let ids: Vec<Uuid> = results.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&id_ygg));
        assert!(ids.contains(&id_global));
        assert!(!ids.contains(&id_fen));

        // Query fenrir only (no global) — should only see fenrir
        let results = index.query_scoped(&make_sdr(0xFF), "fenrir", false, 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, id_fen);
    }

    #[test]
    fn query_all_spans_partitions() {
        let index = SdrIndex::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        index.insert_scoped(Some("a"), id1, make_sdr(0xFF));
        index.insert_scoped(Some("b"), id2, make_sdr(0xFF));

        let results = index.query(&make_sdr(0xFF), 10);
        assert_eq!(results.len(), 2);
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
    fn remove_crosses_partitions() {
        let index = SdrIndex::new();
        let id = Uuid::new_v4();
        index.insert_scoped(Some("proj"), id, make_sdr(0xFF));
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

    #[test]
    fn load_from_rows_scoped() {
        let index = SdrIndex::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let sdr_val: Sdr = [0xDEAD, 0xBEEF, 0xCAFE, 0x1234];
        let bytes = sdr::to_bytes(&sdr_val);

        index.load_from_rows_scoped(&[
            (id1, bytes.clone(), Some("yggdrasil".to_string())),
            (id2, bytes, None),
        ]);

        assert_eq!(index.len(), 2);
        assert_eq!(index.partition_count(), 2);
        assert_eq!(index.partition_len(Some("yggdrasil")), 1);
        assert_eq!(index.partition_len(None), 1);
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
        assert_eq!(stats.concept_coverage, 8);
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
        assert_eq!(stats.concept_coverage, 32);
        assert_eq!(stats.similarity_p50, 1.0);
        assert_eq!(stats.similarity_p90, 1.0);
    }

    #[test]
    fn stats_disjoint_entries_have_low_similarity() {
        let index = SdrIndex::new();
        let a: Sdr = [u64::MAX, 0, 0, 0];
        let b: Sdr = [0, u64::MAX, 0, 0];
        index.insert(Uuid::new_v4(), a);
        index.insert(Uuid::new_v4(), b);

        let stats = index.stats();
        assert_eq!(stats.count, 2);
        assert_eq!(stats.avg_popcount, 64.0);
        assert_eq!(stats.concept_coverage, 128);
        assert_eq!(stats.similarity_p50, 0.5);
    }

    #[test]
    fn stats_concept_coverage_accumulates() {
        let index = SdrIndex::new();
        index.insert(Uuid::new_v4(), [0xFF, 0, 0, 0]);
        index.insert(Uuid::new_v4(), [0, 0xFF, 0, 0]);
        index.insert(Uuid::new_v4(), [0, 0, 0xFF, 0]);
        index.insert(Uuid::new_v4(), [0, 0, 0, 0xFF]);

        let stats = index.stats();
        assert_eq!(stats.count, 4);
        assert_eq!(stats.avg_popcount, 8.0);
        assert_eq!(stats.concept_coverage, 32);
    }

    #[test]
    fn stats_spans_partitions() {
        let index = SdrIndex::new();
        let sdr_val: Sdr = [0xFF, 0xFF, 0xFF, 0xFF];
        index.insert_scoped(Some("a"), Uuid::new_v4(), sdr_val);
        index.insert_scoped(Some("b"), Uuid::new_v4(), sdr_val);
        index.insert_scoped(None, Uuid::new_v4(), sdr_val);

        let stats = index.stats();
        assert_eq!(stats.count, 3);
        assert_eq!(stats.similarity_p50, 1.0);
    }

    // --- novelty gate pattern test ---

    #[test]
    fn novelty_gate_detects_near_duplicates() {
        let index = SdrIndex::new();
        let sdr_a: Sdr = [u64::MAX, u64::MAX, u64::MAX, u64::MAX];
        index.insert(Uuid::new_v4(), sdr_a);

        let sdr_b: Sdr = [u64::MAX, u64::MAX, u64::MAX, u64::MAX ^ 0x1];
        let results = index.query(&sdr_b, 1);
        assert_eq!(results.len(), 1);
        let similarity = results[0].1;
        assert!(similarity > 0.99, "expected > 0.99, got {similarity}");
        assert!(similarity > 0.85, "novelty gate (threshold 0.85) should catch this");
    }

    #[test]
    fn novelty_gate_passes_distinct_content() {
        let index = SdrIndex::new();
        let sdr_a: Sdr = [u64::MAX, 0, 0, 0];
        index.insert(Uuid::new_v4(), sdr_a);

        let sdr_b: Sdr = [0, 0, 0, u64::MAX];
        let results = index.query(&sdr_b, 1);
        assert_eq!(results.len(), 1);
        let similarity = results[0].1;
        assert!(similarity < 0.85, "distinct content should pass novelty gate, got {similarity}");
    }
}
