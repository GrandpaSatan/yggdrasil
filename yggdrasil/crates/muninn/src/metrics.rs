/// Muninn-specific Prometheus metric recording helpers.
///
/// HTTP request metrics (counters, histograms) are handled by
/// `ygg_server::metrics::http_metrics`. This module only contains
/// Muninn-specific metrics for search latency, result counts, and
/// Qdrant operation latency.
///
/// Metrics registered here:
/// - `ygg_search_duration_seconds`       histogram {}
/// - `ygg_search_results_count`          histogram {}
/// - `ygg_qdrant_duration_seconds`       histogram {operation}
use metrics::histogram;

/// Record the end-to-end duration of a search request in seconds.
///
/// This covers the full hybrid search pipeline: embedding + Qdrant vector
/// search + PostgreSQL BM25 + RRF fusion + context assembly.
pub fn record_search_duration(duration_secs: f64) {
    histogram!("ygg_search_duration_seconds").record(duration_secs);
}

/// Record the number of code chunk results returned by a search.
pub fn record_search_results_count(count: f64) {
    histogram!("ygg_search_results_count").record(count);
}

/// Record the duration of a Qdrant operation in seconds.
///
/// `operation` is a short string identifying the type of Qdrant call,
/// e.g. "search", "upsert", "delete".
pub fn record_qdrant_duration(operation: &str, duration_secs: f64) {
    histogram!(
        "ygg_qdrant_duration_seconds",
        "operation" => operation.to_string()
    )
    .record(duration_secs);
}
