//! Pre-built response payloads for common test scenarios.

use serde_json::{json, Value as JsonValue};

/// Ollama text response (no tool calls).
pub fn ollama_text_response(content: &str) -> JsonValue {
    json!({
        "model": "test-model",
        "message": { "role": "assistant", "content": content },
        "done": true
    })
}

/// Ollama response containing a single tool call.
pub fn ollama_tool_call_response(name: &str, args: JsonValue) -> JsonValue {
    json!({
        "model": "test-model",
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "function": { "name": name, "arguments": args }
            }]
        },
        "done": true
    })
}

/// Mimir query response with engram results.
///
/// Each tuple is `(cause, effect, similarity)`.
pub fn mimir_query_response(results: &[(&str, &str, f64)]) -> JsonValue {
    let items: Vec<JsonValue> = results
        .iter()
        .map(|(cause, effect, sim)| {
            json!({ "cause": cause, "effect": effect, "similarity": sim })
        })
        .collect();
    json!({ "results": items })
}

/// Muninn search response with code chunks.
///
/// Each tuple is `(file_path, content, score)`.
pub fn muninn_search_response(chunks: &[(&str, &str, f64)]) -> JsonValue {
    let items: Vec<JsonValue> = chunks
        .iter()
        .map(|(path, content, score)| {
            json!({ "file_path": path, "content": content, "score": score })
        })
        .collect();
    json!({ "chunks": items, "total": items.len() })
}

/// Empty Mimir response (no results).
pub fn mimir_empty_response() -> JsonValue {
    json!({ "results": [] })
}

/// Empty Muninn response (no chunks).
pub fn muninn_empty_response() -> JsonValue {
    json!({ "chunks": [], "total": 0 })
}
