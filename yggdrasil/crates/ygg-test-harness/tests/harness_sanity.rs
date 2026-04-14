//! Sanity tests for the `ygg-test-harness` crate itself.
//!
//! Verifies that all builders compile, spawn successfully, and serve the
//! responses they were configured with.  Also exercises the new Sprint 063
//! additions: `expect_flow_steps`, `mock_mimir_vault`, and flow_assertions.

use serde_json::{json, Value as JsonValue};
use ygg_test_harness::{
    MockMimirBuilder, MockMuninnBuilder, MockOllamaBuilder,
    assert_flow_executed_json, assert_content_contains_any,
    assert_content_contains_any_json, non_empty_choice_count,
};

// ─────────────────────────────────────────────────────────────────────────────
// MockOllamaBuilder
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_mock_ollama_builder_compiles_and_starts() {
    let mock = MockOllamaBuilder::new()
        .with_text_response("hello from mock")
        .start()
        .await;
    assert!(!mock.url.is_empty(), "mock URL should be non-empty");
    assert!(
        mock.url.starts_with("http://127.0.0.1:"),
        "mock URL should be on loopback: {}", mock.url
    );
}

#[tokio::test]
async fn test_mock_ollama_serves_text_response() {
    let mock = MockOllamaBuilder::new()
        .with_text_response("test content")
        .start()
        .await;

    let client = reqwest::Client::new();
    let resp: JsonValue = client
        .post(format!("{}/api/chat", mock.url))
        .json(&json!({"model": "test", "messages": []}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["message"]["content"], "test content");
    assert_eq!(resp["done"], true);
}

#[tokio::test]
async fn test_mock_ollama_serves_tool_call_response() {
    let mock = MockOllamaBuilder::new()
        .with_tool_call("ha_call_service", json!({"domain": "light", "service": "turn_on"}))
        .start()
        .await;

    let client = reqwest::Client::new();
    let resp: JsonValue = client
        .post(format!("{}/api/chat", mock.url))
        .json(&json!({"model": "test", "messages": []}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let tool_calls = resp["message"]["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1, "expected exactly one tool call");
    assert_eq!(tool_calls[0]["function"]["name"], "ha_call_service");
}

#[tokio::test]
async fn test_mock_ollama_falls_back_after_queue_exhausted() {
    let mock = MockOllamaBuilder::new()
        .with_text_response("only response")
        .start()
        .await;

    let client = reqwest::Client::new();

    // First call consumes the queued response.
    let r1: JsonValue = client
        .post(format!("{}/api/chat", mock.url))
        .json(&json!({"model": "test", "messages": []}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r1["message"]["content"], "only response");

    // Second call hits the default fallback.
    let r2: JsonValue = client
        .post(format!("{}/api/chat", mock.url))
        .json(&json!({"model": "test", "messages": []}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r2["message"]["content"], "default fallback");
}

#[tokio::test]
async fn test_mock_ollama_expect_flow_steps_queues_correctly() {
    let steps = vec![
        ("extract_action", "Extracted: turn on kitchen light"),
        ("execute", "Executed: light.turn_on"),
        ("confirm", "The kitchen light is now on."),
    ];

    let mock = MockOllamaBuilder::new()
        .expect_flow_steps(steps)
        .start()
        .await;

    let client = reqwest::Client::new();

    // Consume all three steps in order.
    let r1: JsonValue = client
        .post(format!("{}/api/chat", mock.url))
        .json(&json!({"model": "test", "messages": []}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r1["message"]["content"], "Extracted: turn on kitchen light",
        "step 1 should be extract_action output");
    assert_eq!(r1["model"], "extract_action", "model field should encode step name");

    let r2: JsonValue = client
        .post(format!("{}/api/chat", mock.url))
        .json(&json!({"model": "test", "messages": []}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r2["message"]["content"], "Executed: light.turn_on");
    assert_eq!(r2["model"], "execute");

    let r3: JsonValue = client
        .post(format!("{}/api/chat", mock.url))
        .json(&json!({"model": "test", "messages": []}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r3["message"]["content"], "The kitchen light is now on.");
    assert_eq!(r3["model"], "confirm");
}

// ─────────────────────────────────────────────────────────────────────────────
// MockMimirBuilder
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_mock_mimir_builder_compiles_and_starts() {
    let mock = MockMimirBuilder::new().start().await;
    assert!(!mock.url.is_empty());
}

#[tokio::test]
async fn test_mock_mimir_serves_default_query_response() {
    let mock = MockMimirBuilder::new().start().await;

    let client = reqwest::Client::new();
    let resp: JsonValue = client
        .post(format!("{}/api/v1/query", mock.url))
        .json(&json!({"text": "search query"}))
        .send().await.unwrap().json().await.unwrap();

    assert!(resp["results"].as_array().is_some(), "default response should have results array");
}

#[tokio::test]
async fn test_mock_mimir_serves_queued_responses_fifo() {
    let mock = MockMimirBuilder::new()
        .with_response(json!({"results": [{"cause": "first", "effect": "one", "similarity": 0.9}]}))
        .with_response(json!({"results": [{"cause": "second", "effect": "two", "similarity": 0.8}]}))
        .start()
        .await;

    let client = reqwest::Client::new();

    let r1: JsonValue = client
        .post(format!("{}/api/v1/query", mock.url))
        .json(&json!({"text": "q1"}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r1["results"][0]["cause"], "first");

    let r2: JsonValue = client
        .post(format!("{}/api/v1/query", mock.url))
        .json(&json!({"text": "q2"}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r2["results"][0]["cause"], "second");
}

#[tokio::test]
async fn test_mock_mimir_vault_queues_vault_responses() {
    let mock = MockMimirBuilder::new()
        .mock_mimir_vault()
        .start()
        .await;

    let client = reqwest::Client::new();

    // First call returns the vault list response.
    let r1: JsonValue = client
        .post(format!("{}/api/v1/vault", mock.url))
        .json(&json!({"action": "list"}))
        .send().await.unwrap().json().await.unwrap();

    let entries = r1["entries"].as_array().expect("vault list response should have entries");
    assert_eq!(entries.len(), 1, "expected 1 pre-configured vault entry");
    assert_eq!(entries[0]["key"], "test_vault_key");

    // Second call returns the upsert acknowledgement.
    let r2: JsonValue = client
        .post(format!("{}/api/v1/vault", mock.url))
        .json(&json!({"action": "upsert", "key": "new_key", "value": "new_value"}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r2["ok"], true, "vault upsert should return ok=true");
}

// ─────────────────────────────────────────────────────────────────────────────
// MockMuninnBuilder
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_mock_muninn_builder_compiles_and_starts() {
    let mock = MockMuninnBuilder::new().start().await;
    assert!(!mock.url.is_empty());
}

#[tokio::test]
async fn test_mock_muninn_serves_default_empty_response() {
    let mock = MockMuninnBuilder::new().start().await;

    let client = reqwest::Client::new();
    let resp: JsonValue = client
        .post(format!("{}/api/v1/search", mock.url))
        .json(&json!({"query": "test"}))
        .send().await.unwrap().json().await.unwrap();

    assert_eq!(resp["total"], 0, "default Muninn response should have total=0");
}

// ─────────────────────────────────────────────────────────────────────────────
// flow_assertions
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_assert_flow_executed_json_happy_path() {
    let body = json!({
        "id": "x", "object": "chat.completion", "created": 0, "model": "test",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "light is on" }, "finish_reason": "stop" }]
    });
    assert_flow_executed_json(&body, "home_automation", &["extract_action", "execute", "confirm"])
        .expect("should pass for valid response");
}

#[test]
fn test_assert_flow_executed_json_empty_body_fails() {
    let body = json!({});
    let result = assert_flow_executed_json(&body, "dream_consolidation", &["query_recent"]);
    assert!(result.is_err(), "missing choices should fail");
}

#[test]
fn test_assert_content_contains_any_found() {
    assert_content_contains_any("The kitchen light is on.", &["kitchen", "bedroom"])
        .expect("should find 'kitchen'");
}

#[test]
fn test_assert_content_contains_any_not_found() {
    let result = assert_content_contains_any("nothing relevant here", &["kitchen", "bedroom"]);
    assert!(result.is_err());
}

#[test]
fn test_assert_content_contains_any_json_found() {
    let body = json!({
        "choices": [{ "message": { "content": "deep reasoning complete" } }]
    });
    assert_content_contains_any_json(&body, &["reasoning", "brainstorm"])
        .expect("should find 'reasoning'");
}

#[test]
fn test_non_empty_choice_count_returns_correct_value() {
    let body = json!({
        "choices": [
            { "message": { "content": "first output" } },
            { "message": { "content": "" } },
            { "message": { "content": "third output" } }
        ]
    });
    assert_eq!(non_empty_choice_count(&body), 2, "should count only non-empty choices");
}
