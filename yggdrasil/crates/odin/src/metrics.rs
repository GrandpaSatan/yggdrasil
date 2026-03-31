/// Odin-specific Prometheus metric recording helpers.
///
/// HTTP request metrics (`ygg_http_requests_total`, `ygg_http_request_duration_seconds`)
/// are handled by `ygg_server::metrics::http_metrics`. This module contains only
/// Odin-specific metrics:
///
/// - `ygg_routing_intent_total`             counter   {intent}
/// - `ygg_llm_generation_duration_seconds`  histogram {model}
/// - `ygg_backend_active_requests`          gauge     {backend}
/// - `ygg_agent_tool_calls_total`           counter   {tool, status}
/// - `ygg_agent_iterations_total`           counter
/// - `ygg_agent_loop_duration_seconds`      histogram
use metrics::{counter, histogram};

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

// ─────────────────────────────────────────────────────────────────
// Agent loop metrics
// ─────────────────────────────────────────────────────────────────

/// Record a single tool call in the agent loop.
///
/// `status` should be "ok", "error", or "timeout".
pub fn record_agent_tool_call(tool_name: &str, status: &str) {
    counter!(
        "ygg_agent_tool_calls_total",
        "tool" => tool_name.to_string(),
        "status" => status.to_string()
    )
    .increment(1);
}

/// Increment the agent loop iteration counter.
pub fn record_agent_iteration() {
    counter!("ygg_agent_iterations_total").increment(1);
}

/// Record the total duration of an agent loop run.
pub fn record_agent_loop_duration(duration_secs: f64) {
    histogram!("ygg_agent_loop_duration_seconds").record(duration_secs);
}

// ─────────────────────────────────────────────────────────────────
// Backend metrics
// ─────────────────────────────────────────────────────────────────

/// Increment the active-requests gauge for a backend.
///
/// Call with `delta = 1` before dispatching to a backend and `delta = -1`
/// after the response completes (including errors).
pub fn adjust_backend_active(backend: &str, delta: f64) {
    metrics::gauge!("ygg_backend_active_requests", "backend" => backend.to_string())
        .increment(delta);
}

// ─────────────────────────────────────────────────────────────────
// Hybrid router metrics (Sprint 052)
// ─────────────────────────────────────────────────────────────────

/// Record end-to-end request latency from handler entry to response sent.
pub fn record_e2e_latency(intent: &str, duration_secs: f64) {
    histogram!(
        "ygg_e2e_request_duration_seconds",
        "intent" => intent.to_string()
    )
    .record(duration_secs);
}

/// Record the SDR router's classification confidence.
pub fn record_sdr_classification(intent: &str, confidence: f64) {
    histogram!(
        "ygg_sdr_classification_confidence",
        "intent" => intent.to_string()
    )
    .record(confidence);
}

/// Record the LLM router's classification latency.
pub fn record_llm_classification_latency(duration_secs: f64) {
    histogram!("ygg_llm_classification_duration_seconds").record(duration_secs);
}

/// Record the final routing confidence (from either SDR or LLM).
pub fn record_routing_confidence(intent: &str, method: &str, confidence: f64) {
    histogram!(
        "ygg_routing_confidence",
        "intent" => intent.to_string(),
        "method" => method.to_string()
    )
    .record(confidence);
}

/// Record whether the SDR and LLM routers agreed on intent.
pub fn record_router_agreement(agreed: bool) {
    counter!(
        "ygg_router_agreement_total",
        "agreed" => agreed.to_string()
    )
    .increment(1);
}

/// Record a fallback to the keyword router with the reason.
pub fn record_router_fallback(reason: &str) {
    counter!(
        "ygg_router_fallback_total",
        "reason" => reason.to_string()
    )
    .increment(1);
}

/// Record token usage per model and direction.
pub fn record_token_usage(model: &str, direction: &str, tokens: u64) {
    counter!(
        "ygg_token_usage_total",
        "model" => model.to_string(),
        "direction" => direction.to_string()
    )
    .increment(tokens);
}

/// Record RAG fetch latency per source (muninn, mimir, ha).
pub fn record_rag_fetch_latency(source: &str, duration_secs: f64) {
    histogram!(
        "ygg_rag_fetch_duration_seconds",
        "source" => source.to_string()
    )
    .record(duration_secs);
}

/// Record router queue depth per priority tier.
pub fn record_queue_depth(priority: &str, depth: usize) {
    metrics::gauge!(
        "ygg_router_queue_depth",
        "priority" => priority.to_string()
    )
    .set(depth as f64);
}
