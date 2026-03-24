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
