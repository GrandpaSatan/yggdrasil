/// Muninn Prometheus metrics middleware and recording helpers.
///
/// Installs common HTTP request counters/histograms, plus Muninn-specific
/// metrics for search latency, result counts, and Qdrant operation latency.
///
/// Metrics registered here:
/// - `ygg_http_requests_total`           counter   {service, endpoint, status}
/// - `ygg_http_request_duration_seconds` histogram {service, endpoint}
/// - `ygg_search_duration_seconds`       histogram {}
/// - `ygg_search_results_count`          histogram {}
/// - `ygg_qdrant_duration_seconds`       histogram {operation}
use axum::{extract::Request, middleware::Next, response::Response};
use metrics::{counter, histogram};
use std::time::Instant;

/// Axum middleware that records `ygg_http_requests_total` and
/// `ygg_http_request_duration_seconds` for every request through the Muninn
/// router.
///
/// Install via `axum::middleware::from_fn(muninn::metrics::metrics_middleware)`.
pub async fn metrics_middleware(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let start = Instant::now();

    let response = next.run(req).await;

    let duration = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    counter!(
        "ygg_http_requests_total",
        "service" => "muninn",
        "endpoint" => path.clone(),
        "status" => status
    )
    .increment(1);

    histogram!(
        "ygg_http_request_duration_seconds",
        "service" => "muninn",
        "endpoint" => path
    )
    .record(duration);

    response
}

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
