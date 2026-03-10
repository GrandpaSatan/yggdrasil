/// Odin Prometheus metrics middleware and recording helpers.
///
/// Installs common HTTP request counters/histograms, plus Odin-specific
/// metrics for LLM generation latency, routing intent, and active backend
/// requests.
///
/// Metrics registered here:
/// - `ygg_http_requests_total`           counter   {service, endpoint, status}
/// - `ygg_http_request_duration_seconds` histogram {service, endpoint}
/// - `ygg_routing_intent_total`          counter   {intent}
/// - `ygg_llm_generation_duration_seconds` histogram {model}
/// - `ygg_backend_active_requests`       gauge     {backend}
use axum::{extract::Request, middleware::Next, response::Response};
use metrics::{counter, histogram};
use std::time::Instant;

/// Axum middleware that records `ygg_http_requests_total` and
/// `ygg_http_request_duration_seconds` for every request that passes through
/// the router.
///
/// Install via `axum::middleware::from_fn(odin::metrics::metrics_middleware)`.
pub async fn metrics_middleware(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let start = Instant::now();

    let response = next.run(req).await;

    let duration = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    counter!(
        "ygg_http_requests_total",
        "service" => "odin",
        "endpoint" => path.clone(),
        "status" => status
    )
    .increment(1);

    histogram!(
        "ygg_http_request_duration_seconds",
        "service" => "odin",
        "endpoint" => path
    )
    .record(duration);

    response
}

/// Increment the routing intent counter.
///
/// Call this from the chat handler after the semantic router resolves an intent.
/// `intent` matches the rule name from `RoutingConfig` (e.g. "coding",
/// "reasoning", "home_automation").
pub fn record_routing_intent(intent: &str) {
    counter!("ygg_routing_intent_total", "intent" => intent.to_string()).increment(1);
}

/// Record the wall-clock duration of an Ollama LLM generation call.
///
/// `model` is the model name selected by the semantic router (e.g.
/// "qwen3-coder-30b-a3b").
pub fn record_llm_generation(model: &str, duration_secs: f64) {
    histogram!(
        "ygg_llm_generation_duration_seconds",
        "model" => model.to_string()
    )
    .record(duration_secs);
}

/// Increment the active-requests gauge for a backend.
///
/// Call with `delta = 1` before dispatching to a backend and `delta = -1`
/// after the response completes (including errors).
pub fn adjust_backend_active(backend: &str, delta: f64) {
    metrics::gauge!("ygg_backend_active_requests", "backend" => backend.to_string())
        .increment(delta);
}
