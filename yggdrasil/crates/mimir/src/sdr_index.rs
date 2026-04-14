//! In-memory SDR index for System 1 (hot/fast) recall.
//!
//! Stores SDRs in per-project partitions protected by `RwLock` for concurrent
//! access. Queries perform a brute-force XOR + popcount scan. For 1000
//! Recall-tier engrams × 4 u64 words, this completes in ~4μs.
//!
//! Partitioning by project avoids scanning irrelevant engrams during
//! project-scoped queries, saving CPU cycles proportional to the number
//! of off-project engrams.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use crate::sdr::{self, Sdr};

/// The partition key used for engrams with no project (global scope).
const GLOBAL_PARTITION: &str = "__global__";

/// Thread-safe in-memory SDR index, partitioned by project.
///
/// Read-biased: queries take a read lock, stores take a write lock.
/// Each partition is a contiguous `Vec<(Uuid, Sdr)>` — near-zero memory
/// overhead per partition since SDRs are just 32 bytes each.
///
/// Sprint 065 A·P1: a parallel `tag_index` maps engram Uuid → set of
/// partition-prefix tags (e.g. `sprint:065`, `incident:042`). The main
/// `partitions` vector stays at 40 bytes per entry for cache locality;
/// tag lookup is only performed when a caller supplies a non-empty filter.
/// See `query_scoped_with_tags` — without this filter, near-identical
/// sprint-archive SDRs collided at similarity ~1.0, causing the store-gate
/// LLM to overwrite prior sprint UUIDs (engram d6701e4c).
pub struct SdrIndex {
    partitions: RwLock<HashMap<String, Vec<(Uuid, Sdr)>>>,
    tag_index: RwLock<HashMap<Uuid, HashSet<Arc<str>>>>,
}

impl SdrIndex {
    /// Create an empty index with no partitions.
    pub fn new() -> Self {
        Self {
            partitions: RwLock::new(HashMap::new()),
            tag_index: RwLock::new(HashMap::new()),
        }
    }

    /// Insert an SDR into a specific project partition, recording partition-prefix
    /// tags in the parallel tag_index.
    ///
    /// `tags` may contain any tag strings; only partition-prefix tags (those the
    /// caller intends to filter on later via `query_scoped_with_tags`) need be
    /// passed here. The typical caller passes all engram tags — redundant entries
    /// cost ~16 bytes per tag per engram and do not affect query-time performance
    /// (the filter only hits this index when non-empty).
    pub fn insert_scoped_with_tags(
        &self,
        project: Option<&str>,
        id: Uuid,
        sdr: Sdr,
        tags: &[String],
    ) {
        let key = project.unwrap_or(GLOBAL_PARTITION);
        let mut partitions = self.partitions.write().unwrap();
        partitions
            .entry(key.to_string())
            .or_default()
            .push((id, sdr));
        drop(partitions);

        if !tags.is_empty() {
            let mut tag_index = self.tag_index.write().unwrap();
            let entry = tag_index.entry(id).or_default();
            for t in tags {
                entry.insert(Arc::<str>::from(t.as_str()));
            }
        }
    }

    /// Insert an SDR into a specific project partition.
    pub fn insert_scoped(&self, project: Option<&str>, id: Uuid, sdr: Sdr) {
        self.insert_scoped_with_tags(project, id, sdr, &[]);
    }

    /// Insert into the global partition (backward compat).
    pub fn insert(&self, id: Uuid, sdr: Sdr) {
        self.insert_scoped(None, id, sdr);
    }

    /// Replace the tag set for an engram. Used on update-by-ID to keep the
    /// partition tags in sync with the latest row in PostgreSQL.
    pub fn set_tags(&self, id: Uuid, tags: &[String]) {
        let mut tag_index = self.tag_index.write().unwrap();
        if tags.is_empty() {
            tag_index.remove(&id);
            return;
        }
        let entry = tag_index.entry(id).or_default();
        entry.clear();
        for t in tags {
            entry.insert(Arc::<str>::from(t.as_str()));
        }
    }

    /// Remove an engram from ALL partitions (handles project reassignment).
    pub fn remove(&self, id: Uuid) {
        let mut partitions = self.partitions.write().unwrap();
        for entries in partitions.values_mut() {
            entries.retain(|(eid, _)| *eid != id);
        }
        drop(partitions);
        self.tag_index.write().unwrap().remove(&id);
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
        self.query_scoped_with_tags(target, project, include_global, &[], limit)
    }

    /// Query a specific project partition + global partition, then apply an
    /// OR-semantics tag filter before returning the top-K.
    ///
    /// `tag_filter` — candidates pass if their tag set contains AT LEAST ONE
    /// of these tags. Empty filter = pass-all (behavior identical to
    /// `query_scoped`). The typical caller passes partition-prefix tags like
    /// `["sprint:065"]` to prevent cross-sprint SDR collisions.
    ///
    /// Why: sprint-archive engrams ("Sprint NNN: archived") embed to
    /// near-identical SDRs across sprint numbers. Without this filter, the
    /// store-gate LLM sees a similarity ~1.0 match and routes "update",
    /// overwriting the prior sprint's UUID.
    pub fn query_scoped_with_tags(
        &self,
        target: &Sdr,
        project: &str,
        include_global: bool,
        tag_filter: &[String],
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
        if include_global
            && let Some(entries) = partitions.get(GLOBAL_PARTITION)
        {
            scored.extend(
                entries
                    .iter()
                    .map(|(id, s)| (*id, sdr::hamming_similarity(target, s))),
            );
        }

        drop(partitions);

        // Apply tag filter (OR semantics). A candidate passes if its tag set
        // contains AT LEAST ONE of the filter tags. Candidates missing from
        // tag_index entirely are filtered out — they cannot be in the same
        // partition-prefix group as a tagged candidate.
        if !tag_filter.is_empty() {
            let tag_index = self.tag_index.read().unwrap();
            scored.retain(|(id, _)| {
                tag_index
                    .get(id)
                    .map(|tags| tag_filter.iter().any(|t| tags.contains(t.as_str())))
                    .unwrap_or(false)
            });
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
    /// Leaves the tag_index empty — callers that need tag-filtered queries
    /// should use `load_from_rows_scoped_with_tags` instead.
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

    /// Bulk load from PostgreSQL rows: `(id, sdr_bits BYTEA, project, tags)`.
    ///
    /// Sprint 065 A·P1: populates both the partition vector AND the tag_index
    /// so that partition-prefix queries (e.g. `sprint:NNN`) work immediately
    /// after startup without waiting for new writes to re-hydrate the index.
    pub fn load_from_rows_scoped_with_tags(
        &self,
        rows: &[(Uuid, Vec<u8>, Option<String>, Vec<String>)],
    ) {
        let mut partitions = self.partitions.write().unwrap();
        let mut tag_index = self.tag_index.write().unwrap();
        for (id, bytes, project, tags) in rows {
            if bytes.len() >= sdr::SDR_WORDS * 8 {
                let key = project.as_deref().unwrap_or(GLOBAL_PARTITION);
                partitions
                    .entry(key.to_string())
                    .or_default()
                    .push((*id, sdr::from_bytes(bytes)));
                if !tags.is_empty() {
                    let entry = tag_index.entry(*id).or_default();
                    for t in tags {
                        entry.insert(Arc::<str>::from(t.as_str()));
                    }
                }
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

    // --- Sprint 065 A·P1: partition-prefix tag filter tests ---

    #[test]
    fn insert_with_tags_records_in_tag_index() {
        let index = SdrIndex::new();
        let id = Uuid::new_v4();
        let tags = vec!["sprint:065".to_string(), "decision".to_string()];
        index.insert_scoped_with_tags(Some("yggdrasil"), id, make_sdr(0xFF), &tags);

        let guard = index.tag_index.read().unwrap();
        let recorded = guard.get(&id).expect("tag set present");
        assert!(recorded.contains("sprint:065"));
        assert!(recorded.contains("decision"));
    }

    #[test]
    fn query_with_sprint_tag_filter_isolates_sprints() {
        let index = SdrIndex::new();
        let id_062 = Uuid::new_v4();
        let id_063 = Uuid::new_v4();
        let shared_sdr = make_sdr(0xFFFF);

        // Identical SDRs — the exact cross-sprint collision scenario.
        index.insert_scoped_with_tags(
            Some("yggdrasil"),
            id_062,
            shared_sdr,
            &["sprint:062".to_string()],
        );
        index.insert_scoped_with_tags(
            Some("yggdrasil"),
            id_063,
            shared_sdr,
            &["sprint:063".to_string()],
        );

        // Empty filter — legacy behavior returns both.
        let all = index.query_scoped(&shared_sdr, "yggdrasil", false, 10);
        assert_eq!(all.len(), 2, "empty filter should return both identical SDRs");

        // sprint:063 filter — only id_063 passes.
        let only_063 = index.query_scoped_with_tags(
            &shared_sdr,
            "yggdrasil",
            false,
            &["sprint:063".to_string()],
            10,
        );
        assert_eq!(only_063.len(), 1);
        assert_eq!(only_063[0].0, id_063);

        // sprint:062 filter — only id_062 passes.
        let only_062 = index.query_scoped_with_tags(
            &shared_sdr,
            "yggdrasil",
            false,
            &["sprint:062".to_string()],
            10,
        );
        assert_eq!(only_062.len(), 1);
        assert_eq!(only_062[0].0, id_062);
    }

    #[test]
    fn empty_tag_filter_preserves_legacy_behavior() {
        let index = SdrIndex::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        index.insert_scoped(Some("p"), id1, make_sdr(0xFF));
        index.insert_scoped(Some("p"), id2, make_sdr(0x0F));

        let legacy = index.query_scoped(&make_sdr(0xFF), "p", false, 10);
        let tagged = index.query_scoped_with_tags(&make_sdr(0xFF), "p", false, &[], 10);
        assert_eq!(legacy, tagged);
    }

    #[test]
    fn remove_clears_tag_index() {
        let index = SdrIndex::new();
        let id = Uuid::new_v4();
        index.insert_scoped_with_tags(
            Some("p"),
            id,
            make_sdr(0xFF),
            &["sprint:065".to_string()],
        );
        index.remove(id);

        assert!(index.tag_index.read().unwrap().get(&id).is_none());
    }

    #[test]
    fn set_tags_replaces_tag_set() {
        let index = SdrIndex::new();
        let id = Uuid::new_v4();
        let sdr_val = make_sdr(0xFF);
        index.insert_scoped_with_tags(Some("p"), id, sdr_val, &["sprint:065".to_string()]);

        // Before: sprint:065 filter finds it.
        let found = index.query_scoped_with_tags(
            &sdr_val,
            "p",
            false,
            &["sprint:065".to_string()],
            10,
        );
        assert_eq!(found.len(), 1);

        // Replace tag set.
        index.set_tags(id, &["sprint:066".to_string()]);

        // sprint:065 filter no longer finds it.
        let gone = index.query_scoped_with_tags(
            &sdr_val,
            "p",
            false,
            &["sprint:065".to_string()],
            10,
        );
        assert_eq!(gone.len(), 0);

        // sprint:066 filter now finds it.
        let found_again = index.query_scoped_with_tags(
            &sdr_val,
            "p",
            false,
            &["sprint:066".to_string()],
            10,
        );
        assert_eq!(found_again.len(), 1);
    }

    #[test]
    fn tag_filter_matches_any_tag_or_semantics() {
        let index = SdrIndex::new();
        let id = Uuid::new_v4();
        index.insert_scoped_with_tags(
            Some("p"),
            id,
            make_sdr(0xFF),
            &["sprint:065".to_string(), "phase:P1".to_string()],
        );

        // Filter with a non-matching tag AND a matching tag — OR semantics means PASS.
        let results = index.query_scoped_with_tags(
            &make_sdr(0xFF),
            "p",
            false,
            &["sprint:099".to_string(), "phase:P1".to_string()],
            10,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, id);
    }

    #[test]
    fn load_from_rows_scoped_with_tags_populates_tag_index() {
        let index = SdrIndex::new();
        let id = Uuid::new_v4();
        let sdr_val: Sdr = [0xDEAD, 0xBEEF, 0xCAFE, 0x1234];
        let bytes = sdr::to_bytes(&sdr_val);

        index.load_from_rows_scoped_with_tags(&[(
            id,
            bytes,
            Some("yggdrasil".to_string()),
            vec!["sprint:065".to_string(), "core".to_string()],
        )]);

        let found = index.query_scoped_with_tags(
            &sdr_val,
            "yggdrasil",
            false,
            &["sprint:065".to_string()],
            10,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, id);
    }

    #[test]
    fn candidate_without_tags_filtered_out_when_filter_present() {
        // Regression: if tag_filter is non-empty, an engram with no tag_index
        // entry must NOT be returned (cannot be in the partition-prefix group).
        let index = SdrIndex::new();
        let tagged = Uuid::new_v4();
        let untagged = Uuid::new_v4();
        let sdr_val = make_sdr(0xFF);

        index.insert_scoped_with_tags(
            Some("p"),
            tagged,
            sdr_val,
            &["sprint:065".to_string()],
        );
        index.insert_scoped(Some("p"), untagged, sdr_val);

        let filtered = index.query_scoped_with_tags(
            &sdr_val,
            "p",
            false,
            &["sprint:065".to_string()],
            10,
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, tagged);
    }
}
