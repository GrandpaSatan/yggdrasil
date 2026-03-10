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
}
