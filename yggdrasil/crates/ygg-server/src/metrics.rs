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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::middleware;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    async fn ok_handler() -> &'static str {
        "ok"
    }
    async fn error_handler() -> (StatusCode, &'static str) {
        (StatusCode::INTERNAL_SERVER_ERROR, "boom")
    }

    fn app() -> Router {
        Router::new()
            .route("/ok", get(ok_handler))
            .route("/err", get(error_handler))
            .layer(middleware::from_fn(http_metrics("test-svc")))
    }

    #[tokio::test]
    async fn middleware_preserves_2xx_responses() {
        let resp = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/ok")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn middleware_preserves_5xx_responses() {
        let resp = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/err")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn middleware_handles_unknown_routes_with_404() {
        let resp = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
