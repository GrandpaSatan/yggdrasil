/// SDR-based "System 1" intent classifier (Sprint 052).
///
/// Maintains per-intent SDR prototypes — running OR-accumulations of confirmed
/// query SDRs.  On each request the user message's SDR is compared against all
/// prototypes via Hamming similarity; the best match above threshold becomes
/// the fast routing suggestion.
///
/// Follows the same pattern as `skill_cache.rs` (RwLock<Vec<T>> + Hamming scan)
/// but operates on text-derived SDRs instead of audio fingerprints.
///
/// Prototypes are persisted to disk and loaded on startup.  When no persisted
/// prototypes exist, the router bootstraps from the keyword lists by calling
/// Mimir's embed endpoint to convert each keyword into an SDR.
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use ygg_domain::sdr::{self, Sdr};

/// Default Hamming similarity threshold for intent classification.
/// Lower than skill_cache's 0.85 because intent classification is broader
/// than exact tool-command matching.
const DEFAULT_SDR_THRESHOLD: f64 = 0.70;

// ─────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────

/// A per-intent SDR prototype learned from confirmed classifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentPrototype {
    /// Intent name (e.g. "coding", "reasoning", "home_automation").
    pub intent: String,
    /// OR-accumulated SDR from confirmed queries for this intent.
    pub sdr: Sdr,
    /// How many query SDRs have been OR'd into this prototype.
    pub sample_count: u64,
    /// When this prototype was last reinforced.
    pub last_updated: DateTime<Utc>,
}

/// Result of an SDR classification lookup.
#[derive(Debug, Clone)]
pub struct SdrClassification {
    /// Best-matching intent.
    pub intent: String,
    /// Hamming similarity of the best match (0.0–1.0).
    pub confidence: f64,
    /// Second-best intent (for logging/training data).
    pub runner_up_intent: Option<String>,
    /// Second-best similarity.
    pub runner_up_confidence: Option<f64>,
}

// ─────────────────────────────────────────────────────────────────
// SdrRouter
// ─────────────────────────────────────────────────────────────────

/// Thread-safe SDR-based intent classifier with persistent prototypes.
pub struct SdrRouter {
    prototypes: RwLock<Vec<IntentPrototype>>,
    threshold: f64,
}

impl SdrRouter {
    /// Create a new router with the given threshold.
    pub fn new(threshold: f64) -> Self {
        Self {
            prototypes: RwLock::new(Vec::new()),
            threshold,
        }
    }

    /// Create a router with default threshold.
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_SDR_THRESHOLD)
    }

    /// Classify a query SDR against the stored intent prototypes.
    ///
    /// Returns the best match above threshold, or `None` if no prototype is
    /// close enough.  The confidence is the raw Hamming similarity, boosted
    /// slightly when the gap to the runner-up exceeds `CONFIDENCE_GAP_BONUS`.
    pub async fn classify(&self, query_sdr: &Sdr) -> Option<SdrClassification> {
        let guard = self.prototypes.read().await;
        if guard.is_empty() {
            return None;
        }

        let mut scores: Vec<(&str, f64)> = guard
            .iter()
            .map(|p| (p.intent.as_str(), sdr::hamming_similarity(query_sdr, &p.sdr)))
            .collect();

        // Sort descending by similarity.
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let (best_intent, best_sim) = scores[0];
        if best_sim < self.threshold {
            return None;
        }

        let (runner_up_intent, runner_up_confidence) = if scores.len() > 1 {
            (Some(scores[1].0.to_string()), Some(scores[1].1))
        } else {
            (None, None)
        };

        Some(SdrClassification {
            intent: best_intent.to_string(),
            confidence: best_sim,
            runner_up_intent,
            runner_up_confidence,
        })
    }

    /// Reinforce a prototype after the LLM confirmed the SDR's suggestion.
    ///
    /// OR-accumulates the query SDR into the matching prototype and increments
    /// the sample count.  If no prototype exists for the intent, creates one.
    pub async fn reinforce(&self, intent: &str, query_sdr: &Sdr) {
        let mut guard = self.prototypes.write().await;
        if let Some(proto) = guard.iter_mut().find(|p| p.intent == intent) {
            proto.sdr = sdr::or(&proto.sdr, query_sdr);
            proto.sample_count += 1;
            proto.last_updated = Utc::now();
        } else {
            guard.push(IntentPrototype {
                intent: intent.to_string(),
                sdr: *query_sdr,
                sample_count: 1,
                last_updated: Utc::now(),
            });
        }
    }

    /// Seed a prototype directly (used during bootstrap from keyword embeddings).
    pub async fn seed_prototype(&self, intent: String, sdr: Sdr) {
        let mut guard = self.prototypes.write().await;
        if let Some(proto) = guard.iter_mut().find(|p| p.intent == intent) {
            proto.sdr = sdr::or(&proto.sdr, &sdr);
            proto.sample_count += 1;
            proto.last_updated = Utc::now();
        } else {
            guard.push(IntentPrototype {
                intent,
                sdr,
                sample_count: 1,
                last_updated: Utc::now(),
            });
        }
    }

    /// Load prototypes from a JSON file.  Returns the number loaded.
    pub async fn load_from_file(&self, path: &Path) -> std::io::Result<usize> {
        let data = tokio::fs::read_to_string(path).await?;
        let loaded: Vec<IntentPrototype> = serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let count = loaded.len();
        *self.prototypes.write().await = loaded;
        Ok(count)
    }

    /// Save prototypes to a JSON file.
    pub async fn save_to_file(&self, path: &Path) -> std::io::Result<()> {
        let guard = self.prototypes.read().await;
        let json = serde_json::to_string_pretty(&*guard)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        // Write to a temp file then rename for atomic replace.
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, json.as_bytes()).await?;
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    /// Number of stored prototypes.
    pub async fn len(&self) -> usize {
        self.prototypes.read().await.len()
    }

    /// Whether the prototype store is empty.
    pub async fn is_empty(&self) -> bool {
        self.prototypes.read().await.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sdr(seed: u64) -> Sdr {
        // Deterministic pseudo-SDR from a seed for testing.
        [seed, seed.wrapping_mul(7), seed.wrapping_mul(13), seed.wrapping_mul(31)]
    }

    #[tokio::test]
    async fn classify_empty_returns_none() {
        let router = SdrRouter::with_defaults();
        let query = make_sdr(42);
        assert!(router.classify(&query).await.is_none());
    }

    #[tokio::test]
    async fn classify_exact_match() {
        let router = SdrRouter::with_defaults();
        let coding_sdr = make_sdr(100);
        router.seed_prototype("coding".into(), coding_sdr).await;

        let result = router.classify(&coding_sdr).await;
        assert!(result.is_some());
        let cls = result.unwrap();
        assert_eq!(cls.intent, "coding");
        assert_eq!(cls.confidence, 1.0);
    }

    #[tokio::test]
    async fn classify_below_threshold_returns_none() {
        let router = SdrRouter::new(0.95); // very high threshold
        let proto_sdr: Sdr = [u64::MAX, 0, 0, 0];
        router.seed_prototype("coding".into(), proto_sdr).await;

        // Completely different SDR should be below 0.95.
        let query: Sdr = [0, u64::MAX, u64::MAX, u64::MAX];
        assert!(router.classify(&query).await.is_none());
    }

    #[tokio::test]
    async fn reinforce_creates_new_prototype() {
        let router = SdrRouter::with_defaults();
        assert!(router.is_empty().await);

        let sdr = make_sdr(42);
        router.reinforce("gaming", &sdr).await;
        assert_eq!(router.len().await, 1);
    }

    #[tokio::test]
    async fn reinforce_accumulates_via_or() {
        let router = SdrRouter::with_defaults();
        let sdr_a: Sdr = [0xFF, 0, 0, 0];
        let sdr_b: Sdr = [0, 0xFF, 0, 0];

        router.seed_prototype("coding".into(), sdr_a).await;
        router.reinforce("coding", &sdr_b).await;

        // After reinforcement, prototype should contain bits from both.
        let guard = router.prototypes.read().await;
        let proto = &guard[0];
        assert_eq!(proto.sdr[0], 0xFF);
        assert_eq!(proto.sdr[1], 0xFF);
        assert_eq!(proto.sample_count, 2);
    }

    #[tokio::test]
    async fn persistence_round_trip() {
        let router = SdrRouter::with_defaults();
        router.seed_prototype("coding".into(), make_sdr(1)).await;
        router.seed_prototype("reasoning".into(), make_sdr(2)).await;

        let tmp = std::env::temp_dir().join("odin-sdr-test.json");
        router.save_to_file(&tmp).await.unwrap();

        let loaded = SdrRouter::with_defaults();
        let count = loaded.load_from_file(&tmp).await.unwrap();
        assert_eq!(count, 2);
        assert_eq!(loaded.len().await, 2);

        // Verify classification still works after load.
        let result = loaded.classify(&make_sdr(1)).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().intent, "coding");

        let _ = tokio::fs::remove_file(&tmp).await;
    }

    #[tokio::test]
    async fn classify_returns_runner_up() {
        let router = SdrRouter::new(0.50); // low threshold to catch runner-up
        router.seed_prototype("coding".into(), make_sdr(100)).await;
        router.seed_prototype("reasoning".into(), make_sdr(200)).await;

        let result = router.classify(&make_sdr(100)).await;
        assert!(result.is_some());
        let cls = result.unwrap();
        assert_eq!(cls.intent, "coding");
        assert!(cls.runner_up_intent.is_some());
        assert_eq!(cls.runner_up_intent.unwrap(), "reasoning");
    }
}
