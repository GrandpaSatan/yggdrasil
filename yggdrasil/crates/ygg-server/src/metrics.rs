//! HTTP metrics middleware for Axum services.
//!
//! Records `ygg_http_requests_total` (counter) and
//! `ygg_http_request_duration_seconds` (histogram) for every request,
//! tagged with the service name, endpoint path, and status code.
//!
//! Replaces three identical `metrics_middleware` implementations across
//! Odin, Mimir, and Muninn.

use axum::{extract::Request, middleware::Next, response::Response};
use metrics::{counter, histogram};
use std::time::Instant;

/// Create an Axum middleware function that records HTTP metrics for `service`.
///
/// # Usage
///
/// ```rust,ignore
/// use axum::middleware;
///
/// let app = Router::new()
///     .route("/health", get(health))
///     .layer(middleware::from_fn(ygg_server::metrics::http_metrics("mimir")));
/// ```
///
/// The returned closure captures the service name as a `&'static str` so it
/// can be used as an Axum `from_fn` middleware with zero allocations per label.
pub fn http_metrics(
    service: &'static str,
) -> impl Fn(Request, Next) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>
       + Clone
       + Send {
    move |req: Request, next: Next| {
        let service = service;
        Box::pin(async move {
            let path = req.uri().path().to_string();
            let start = Instant::now();

            let response = next.run(req).await;

            let duration = start.elapsed().as_secs_f64();
            let status = response.status().as_u16().to_string();

            counter!(
                "ygg_http_requests_total",
                "service" => service,
                "endpoint" => path.clone(),
                "status" => status
            )
            .increment(1);

            histogram!(
                "ygg_http_request_duration_seconds",
                "service" => service,
                "endpoint" => path
            )
            .record(duration);

            response
        })
    }
}
