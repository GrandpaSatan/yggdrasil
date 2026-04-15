/// Mimir-specific Prometheus metrics recording helpers.
///
/// HTTP request metrics (ygg_http_requests_total, ygg_http_request_duration_seconds)
/// are handled by `ygg_server::metrics::http_metrics("mimir")`.
///
/// Mimir-specific metrics registered here:
/// - `ygg_engram_count`                   gauge     {tier}
/// - `ygg_embedding_duration_seconds`     histogram {}
/// - `ygg_summarization_total`            counter   {}
/// - `ygg_summarization_engrams_archived` counter   {}
/// - `ygg_novelty_gate_tier_total`        counter   {tier}      (Sprint 069 Phase D)
/// - `ygg_novelty_cosine_similarity`      histogram {}          (Sprint 069 Phase D)
/// - `ygg_novelty_verdict_total`          counter   {verdict}   (Sprint 069 Phase D)
/// - `ygg_novelty_gate_duration_seconds`  histogram {}          (Sprint 069 Phase D)
use metrics::{counter, histogram};

/// Update the gauge for the number of engrams in a given tier.
///
/// `tier` is one of "core", "recall", or "archival".
/// `count` is the current total. Call this after store, promote, or
/// summarization operations that change tier membership.
pub fn set_engram_count(tier: &str, count: f64) {
    metrics::gauge!("ygg_engram_count", "tier" => tier.to_string()).set(count);
}

/// Record the wall-clock duration of an embedding API call in seconds.
pub fn record_embedding_duration(duration_secs: f64) {
    histogram!("ygg_embedding_duration_seconds").record(duration_secs);
}

/// Increment the summarization-batches-completed counter.
pub fn increment_summarization_total() {
    counter!("ygg_summarization_total").increment(1);
}

/// Increment the count of engrams archived by the summarization service.
pub fn increment_summarization_archived(count: u64) {
    counter!("ygg_summarization_engrams_archived").increment(count);
}

// ---------------------------------------------------------------------------
// Sprint 069 Phase D — Three-Tier Dense Cosine Gate metrics
// ---------------------------------------------------------------------------

/// Increment the counter for the tier that resolved a novelty verdict.
///
/// `tier` is one of "0" (SimHash pre-filter), "1" (dense cosine fast path),
/// or "2" (LLM escalation on Ambiguous cosine band).
pub fn increment_gate_tier(tier: &str) {
    counter!("ygg_novelty_gate_tier_total", "tier" => tier.to_string()).increment(1);
}

/// Record the best-match cosine similarity observed on the store path.
///
/// Observed on every store that consults the dense index (including cold-start
/// empty-index cases, which record NaN and are filtered by Prometheus as NaN).
pub fn record_cosine_similarity(sim: f64) {
    // NaN records are legal but Prometheus histograms silently drop them, so a
    // cold-start store with no dense neighbour is a no-op here — intentional.
    if !sim.is_nan() {
        histogram!("ygg_novelty_cosine_similarity").record(sim);
    }
}

/// Increment the novelty verdict distribution counter.
///
/// `verdict` is one of "new", "update", or "old". Recorded after the final
/// verdict is committed (not on fall-through from Tier 1 → Tier 2 → New).
pub fn increment_novelty_verdict(verdict: &str) {
    counter!("ygg_novelty_verdict_total", "verdict" => verdict.to_string()).increment(1);
}

/// Record the wall-clock duration of one novelty gate evaluation in seconds.
///
/// Covers Tier 0 through final verdict commit (includes LLM RTT on Tier 2).
pub fn record_gate_duration(secs: f64) {
    histogram!("ygg_novelty_gate_duration_seconds").record(secs);
}

/// Pre-register the Phase D novelty-gate metrics so they appear in `/metrics`
/// on a fresh process before any store request arrives.
///
/// Without pre-registration, the Prometheus exporter only surfaces a counter
/// family once it has been incremented at least once. Tests and dashboards
/// that probe `/metrics` on boot expect all four families to be present.
///
/// Calling `.absolute(0)` on a counter is a no-op write that registers the
/// metric family with the exporter without mutating its value.
pub fn preregister_novelty_gate_metrics() {
    for tier in ["0", "1", "2"] {
        counter!("ygg_novelty_gate_tier_total", "tier" => tier.to_string()).absolute(0);
    }
    for verdict in ["new", "update", "old"] {
        counter!("ygg_novelty_verdict_total", "verdict" => verdict.to_string()).absolute(0);
    }
    // Histograms surface after their first record; emit a 0.0 sample so the
    // _bucket/_count/_sum lines exist in /metrics at boot.
    histogram!("ygg_novelty_cosine_similarity").record(0.0);
    histogram!("ygg_novelty_gate_duration_seconds").record(0.0);
}

/// Record SDR index health metrics from a periodic stats snapshot.
pub fn record_sdr_health(
    index_size: f64,
    avg_popcount: f64,
    concept_coverage: f64,
    similarity_p50: f64,
    similarity_p90: f64,
) {
    metrics::gauge!("ygg_sdr_index_size").set(index_size);
    metrics::gauge!("ygg_sdr_avg_popcount").set(avg_popcount);
    metrics::gauge!("ygg_sdr_concept_coverage").set(concept_coverage);
    metrics::gauge!("ygg_sdr_pairwise_similarity_p50").set(similarity_p50);
    metrics::gauge!("ygg_sdr_pairwise_similarity_p90").set(similarity_p90);
}
