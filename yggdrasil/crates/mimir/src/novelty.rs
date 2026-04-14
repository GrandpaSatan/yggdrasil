//! Three-state novelty verdict: New / Update / Old. Sprint 064 P1.
//!
//! Replaces the binary Sprint 016 dedup gate with a triage that lets the
//! `/api/v1/store` handler decide *server-side* whether to insert, overwrite,
//! or skip — instead of returning 409 and asking the client to retry.

use serde::Serialize;
use uuid::Uuid;
use ygg_domain::config::NoveltyConfig;

/// Verdict returned by the Mimir novelty gate.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "verdict", rename_all = "lowercase")]
pub enum NoveltyVerdict {
    /// No near-duplicate; insert as a new engram.
    New,
    /// Near-duplicate exists with meaningfully different content; overwrite in place.
    Update {
        id: Uuid,
        previous_cause: String,
        previous_effect: String,
    },
    /// Near-identical engram already exists; skip the write.
    Old { id: Uuid },
}

/// Classify a candidate engram against its nearest neighbour in the SDR index.
///
/// Order of precedence: Old → Update → New. The `Old` check requires both the
/// similarity floor *and* near-identical effect text (whitespace-normalised
/// equality OR Levenshtein distance within tolerance) so high-similarity
/// rewrites are still routed to `Update`.
pub fn classify_novelty(
    similarity: f64,
    new_effect: &str,
    existing_id: Uuid,
    existing_cause: &str,
    existing_effect: &str,
    cfg: &NoveltyConfig,
) -> NoveltyVerdict {
    if similarity >= cfg.old_threshold {
        let normalized_match = normalized(new_effect) == normalized(existing_effect);
        let near_match = levenshtein(new_effect, existing_effect, cfg.levenshtein_tolerance)
            <= cfg.levenshtein_tolerance;
        if normalized_match || near_match {
            return NoveltyVerdict::Old { id: existing_id };
        }
    }
    if similarity >= cfg.update_threshold {
        return NoveltyVerdict::Update {
            id: existing_id,
            previous_cause: existing_cause.to_owned(),
            previous_effect: existing_effect.to_owned(),
        };
    }
    NoveltyVerdict::New
}

fn normalized(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Bounded Levenshtein on `chars()`. Returns at most `max + 1` to cap cost.
fn levenshtein(a: &str, b: &str, max: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let len_diff = (a.len() as isize - b.len() as isize).unsigned_abs() as usize;
    if len_diff > max {
        return max + 1;
    }
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m.min(max + 1);
    }
    if m == 0 {
        return n.min(max + 1);
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr: Vec<usize> = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        let mut row_min = curr[0];
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
            if curr[j] < row_min {
                row_min = curr[j];
            }
        }
        if row_min > max {
            return max + 1;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m].min(max + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> NoveltyConfig {
        NoveltyConfig {
            old_threshold: 0.98,
            update_threshold: 0.85,
            levenshtein_tolerance: 8,
        }
    }

    #[test]
    fn new_when_similarity_below_update_threshold() {
        let v = classify_novelty(0.7, "anything", Uuid::new_v4(), "c", "e", &cfg());
        assert!(matches!(v, NoveltyVerdict::New));
    }

    #[test]
    fn update_when_similarity_in_band() {
        let id = Uuid::new_v4();
        let v = classify_novelty(
            0.90,
            "completely different content",
            id,
            "old cause",
            "old",
            &cfg(),
        );
        match v {
            NoveltyVerdict::Update {
                id: out_id,
                previous_cause,
                previous_effect,
            } => {
                assert_eq!(out_id, id);
                assert_eq!(previous_cause, "old cause");
                assert_eq!(previous_effect, "old");
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn old_when_high_similarity_and_normalized_equal() {
        let id = Uuid::new_v4();
        let v = classify_novelty(0.99, "Hello World", id, "c", "  hello   world\n", &cfg());
        assert!(matches!(v, NoveltyVerdict::Old { id: x } if x == id));
    }

    #[test]
    fn old_when_levenshtein_within_tolerance() {
        let id = Uuid::new_v4();
        let v = classify_novelty(0.99, "abc", id, "c", "abd", &cfg());
        assert!(matches!(v, NoveltyVerdict::Old { id: x } if x == id));
    }

    #[test]
    fn update_when_high_similarity_but_text_far() {
        let v = classify_novelty(
            0.99,
            "hi",
            Uuid::new_v4(),
            "c",
            "completely unrelated lengthy text here",
            &cfg(),
        );
        assert!(matches!(v, NoveltyVerdict::Update { .. }));
    }

    #[test]
    fn levenshtein_early_exit_long_difference() {
        assert_eq!(levenshtein("a", "abcdefghij", 3), 4);
    }
}
