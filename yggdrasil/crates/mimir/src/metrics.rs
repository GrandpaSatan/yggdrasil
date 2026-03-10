/// Mimir Prometheus metrics middleware and recording helpers.
///
/// Installs common HTTP request counters/histograms, plus Mimir-specific
/// metrics for engram counts, embedding latency, and summarization progress.
///
/// Metrics registered here:
/// - `ygg_http_requests_total`           counter   {service, endpoint, status}
/// - `ygg_http_request_duration_seconds` histogram {service, endpoint}
/// - `ygg_engram_count`                  gauge     {tier}
/// - `ygg_embedding_duration_seconds`    histogram {}
/// - `ygg_summarization_total`           counter   {}
/// - `ygg_summarization_engrams_archived` counter  {}
use axum::{extract::Request, middleware::Next, response::Response};
use metrics::{counter, histogram};
use std::time::Instant;

/// Axum middleware that records `ygg_http_requests_total` and
/// `ygg_http_request_duration_seconds` for every request through the Mimir
/// router.
///
/// Install via `axum::middleware::from_fn(mimir::metrics::metrics_middleware)`.
pub async fn metrics_middleware(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let start = Instant::now();

    let response = next.run(req).await;

    let duration = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    counter!(
        "ygg_http_requests_total",
        "service" => "mimir",
        "endpoint" => path.clone(),
        "status" => status
    )
    .increment(1);

    histogram!(
        "ygg_http_request_duration_seconds",
        "service" => "mimir",
        "endpoint" => path
    )
    .record(duration);

    response
}

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
