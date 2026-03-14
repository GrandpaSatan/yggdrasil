use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// SDR-based anomaly detector.
///
/// Encodes log text into SDR fingerprints via Odin's embedding pipeline,
/// compares against a rolling baseline, and flags anomalies when similarity
/// drops below the configured threshold.
pub struct AnomalyDetector {
    odin_url: String,
    threshold: f64,
    /// Rolling baseline of recent SDR fingerprints (binary vectors).
    baseline: Arc<RwLock<VecDeque<Vec<u8>>>>,
    client: reqwest::Client,
}

const BASELINE_WINDOW: usize = 100;

impl AnomalyDetector {
    pub fn new(odin_url: String, threshold: f64) -> Self {
        Self {
            odin_url,
            threshold,
            baseline: Arc::new(RwLock::new(VecDeque::with_capacity(BASELINE_WINDOW))),
            client: reqwest::Client::new(),
        }
    }

    /// Encode a log window text into an SDR and check for anomaly.
    /// Returns true if the log window is anomalous (similarity below threshold).
    pub async fn check_anomaly(&self, log_text: &str) -> Result<bool, AnomalyError> {
        let sdr = self.encode_sdr(log_text).await?;

        let mut baseline = self.baseline.write().await;

        if baseline.is_empty() {
            // No baseline yet — add this as the first entry
            baseline.push_back(sdr);
            return Ok(false);
        }

        // Compare against baseline average similarity
        let avg_similarity = baseline
            .iter()
            .map(|b| hamming_similarity(&sdr, b))
            .sum::<f64>()
            / baseline.len() as f64;

        debug!(
            similarity = avg_similarity,
            threshold = self.threshold,
            baseline_size = baseline.len(),
            "SDR anomaly check"
        );

        let is_anomaly = avg_similarity < self.threshold;

        // Add to baseline (only if not anomalous, to avoid poisoning)
        if !is_anomaly {
            if baseline.len() >= BASELINE_WINDOW {
                baseline.pop_front();
            }
            baseline.push_back(sdr);
        }

        Ok(is_anomaly)
    }

    /// Encode text to SDR via Odin's embedding endpoint, with hash-based fallback.
    async fn encode_sdr(&self, text: &str) -> Result<Vec<u8>, AnomalyError> {
        let url = format!("{}/api/v1/embed", self.odin_url);
        let payload = serde_json::json!({
            "text": text
        });

        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "Odin embedding request failed, falling back to hash SDR");
                return Ok(simple_hash_sdr(text, 256));
            }
        };

        if !resp.status().is_success() {
            // Fallback to hash-based SDR if Odin embedding unavailable
            warn!(
                status = resp.status().as_u16(),
                "Odin embedding unavailable, falling back to hash SDR"
            );
            return Ok(simple_hash_sdr(text, 256));
        }

        // Parse embedding response and binarize to SDR
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AnomalyError::Encoding(e.to_string()))?;

        if let Some(embedding) = body.get("embedding").and_then(|e| e.as_array()) {
            let sdr = binarize_embedding(embedding);
            info!(sdr_len = sdr.len(), "encoded SDR via Odin embedding");
            Ok(sdr)
        } else {
            // Fallback if response format is unexpected
            warn!("Odin response missing embedding field, falling back to hash SDR");
            Ok(simple_hash_sdr(text, 256))
        }
    }
}

/// Convert a float embedding vector to a binary SDR by thresholding at the median.
/// Each bit in the output represents whether the corresponding embedding dimension
/// exceeds the median value.
pub fn binarize_embedding(embedding: &[serde_json::Value]) -> Vec<u8> {
    let floats: Vec<f64> = embedding
        .iter()
        .filter_map(|v| v.as_f64())
        .collect();

    if floats.is_empty() {
        return vec![0u8; 256];
    }

    // Find median for threshold
    let mut sorted = floats.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];

    // Pack bits: each byte holds 8 dimensions
    let num_bytes = floats.len().div_ceil(8);
    let mut sdr = vec![0u8; num_bytes];

    for (i, &val) in floats.iter().enumerate() {
        if val > median {
            let byte_idx = i / 8;
            let bit_idx = i % 8;
            sdr[byte_idx] |= 1 << bit_idx;
        }
    }

    sdr
}

/// Compute Hamming similarity between two binary SDR vectors.
/// Returns a value between 0.0 (completely different) and 1.0 (identical).
fn hamming_similarity(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let matching: usize = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x ^ y).count_zeros() as usize)
        .sum();

    matching as f64 / (a.len() * 8) as f64
}

/// Simple hash-based SDR for fallback implementation.
/// Produces a deterministic binary vector from text.
fn simple_hash_sdr(text: &str, dim_bytes: usize) -> Vec<u8> {
    let mut sdr = vec![0u8; dim_bytes];

    // Simple FNV-like hashing to set bits
    let mut hash: u64 = 0xcbf29ce484222325;
    for (i, byte) in text.bytes().enumerate() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);

        // Set a bit based on the hash
        let byte_idx = (hash as usize) % dim_bytes;
        let bit_idx = ((hash >> 8) as usize) % 8;
        sdr[byte_idx] |= 1 << bit_idx;

        // Reset hash periodically for better distribution
        if i % 4 == 0 {
            hash = hash.wrapping_add(i as u64);
        }
    }

    sdr
}

#[derive(Debug, thiserror::Error)]
pub enum AnomalyError {
    #[error("encoding error: {0}")]
    Encoding(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hamming_identical_is_one() {
        let a = vec![0xFF, 0x00, 0xAA];
        assert!((hamming_similarity(&a, &a) - 1.0).abs() < 0.001);
    }

    #[test]
    fn hamming_opposite_is_low() {
        let a = vec![0xFF; 32];
        let b = vec![0x00; 32];
        let sim = hamming_similarity(&a, &b);
        assert!(sim < 0.01);
    }

    #[test]
    fn simple_hash_sdr_deterministic() {
        let a = simple_hash_sdr("test log line", 32);
        let b = simple_hash_sdr("test log line", 32);
        assert_eq!(a, b);
    }

    #[test]
    fn simple_hash_sdr_different_inputs() {
        let a = simple_hash_sdr("normal log", 32);
        let b = simple_hash_sdr("ERROR panic stacktrace", 32);
        let sim = hamming_similarity(&a, &b);
        assert!(sim < 0.95); // Should be somewhat different
    }

    #[tokio::test]
    async fn test_check_anomaly_builds_baseline() {
        // Use a bogus URL — encode_sdr will fail HTTP and fall back to hash SDR
        let detector = AnomalyDetector::new("http://127.0.0.1:1".to_string(), 0.70);

        // First call always returns false (building baseline)
        let result = detector.check_anomaly("normal system log output").await.unwrap();
        assert!(!result, "first call should return false (building baseline)");

        // Baseline should have exactly one entry
        let baseline = detector.baseline.read().await;
        assert_eq!(baseline.len(), 1);
    }

    #[tokio::test]
    async fn test_check_anomaly_detects_anomaly() {
        let detector = AnomalyDetector::new("http://127.0.0.1:1".to_string(), 0.99);

        // Build baseline with similar entries
        for _ in 0..5 {
            let _ = detector.check_anomaly("all systems nominal health ok").await;
        }

        // Inject very different text — with threshold at 0.99, most dissimilar text
        // should be flagged as anomalous
        let result = detector
            .check_anomaly("CRITICAL FAILURE kernel panic segfault 0xDEADBEEF")
            .await
            .unwrap();
        assert!(result, "very different text should be detected as anomaly");
    }

    #[test]
    fn test_binarize_embedding() {
        let embedding: Vec<serde_json::Value> = vec![
            serde_json::json!(0.1),
            serde_json::json!(0.5),
            serde_json::json!(0.9),
            serde_json::json!(0.2),
            serde_json::json!(0.8),
            serde_json::json!(0.3),
            serde_json::json!(0.7),
            serde_json::json!(0.4),
        ];

        let sdr = binarize_embedding(&embedding);

        // 8 dimensions => 1 byte
        assert_eq!(sdr.len(), 1, "8 dimensions should produce 1 byte");

        // Median of sorted [0.1, 0.2, 0.3, 0.4, 0.5, 0.7, 0.8, 0.9] is 0.5
        // Dimensions > 0.5: index 2 (0.9), index 4 (0.8), index 6 (0.7)
        // Bits set: bit 2, bit 4, bit 6
        let expected = (1 << 2) | (1 << 4) | (1 << 6);
        assert_eq!(sdr[0], expected, "bits should be set for values above median");
    }
}
