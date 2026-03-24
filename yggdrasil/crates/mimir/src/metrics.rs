/// Mimir-specific Prometheus metrics recording helpers.
///
/// HTTP request metrics (ygg_http_requests_total, ygg_http_request_duration_seconds)
/// are handled by `ygg_server::metrics::http_metrics("mimir")`.
///
/// Mimir-specific metrics registered here:
/// - `ygg_engram_count`                  gauge     {tier}
/// - `ygg_embedding_duration_seconds`    histogram {}
/// - `ygg_summarization_total`           counter   {}
/// - `ygg_summarization_engrams_archived` counter  {}
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
