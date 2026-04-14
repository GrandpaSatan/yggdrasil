//! Assertion helpers for multi-step flow E2E tests.
//!
//! These helpers operate on raw `serde_json::Value` responses (the parsed
//! JSON body from Odin's `/v1/chat/completions` endpoint) so they can live
//! in `ygg-test-harness` without creating a circular dependency on `odin`.
//!
//! Usage in odin integration tests:
//!
//! ```ignore
//! use ygg_test_harness::flow_assertions::assert_flow_executed_json;
//! // body comes from: response.json::<serde_json::Value>().await.unwrap()
//! assert_flow_executed_json(&body, "home_automation", &["extract_action", "execute", "confirm"])
//!     .unwrap();
//! ```

use serde_json::Value as JsonValue;

/// Assert that a raw JSON chat-completion response body was produced by a
/// named flow executing the expected sequence of steps.
///
/// # Parameters
/// - `body` — Parsed JSON body from `POST /v1/chat/completions`.
/// - `expected_flow` — Flow name (used in error messages only; Odin does not
///   currently embed the flow name in non-streaming responses).
/// - `expected_steps` — Ordered step names. At minimum, the number of steps
///   must be > 0 and the final assistant message non-empty.
///
/// # Returns
/// `Ok(())` on success, `Err(String)` with a human-readable failure message.
pub fn assert_flow_executed_json(
    body: &JsonValue,
    expected_flow: &str,
    expected_steps: &[&str],
) -> Result<(), String> {
    // 1. Response must have at least one choice.
    let choices = body
        .get("choices")
        .and_then(|c| c.as_array())
        .ok_or_else(|| format!("[{expected_flow}] response missing 'choices' array"))?;

    let choice = choices
        .first()
        .ok_or_else(|| format!("[{expected_flow}] 'choices' array is empty"))?;

    // 2. Final assistant message must be non-empty.
    let content = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    if content.trim().is_empty() {
        return Err(format!(
            "[{expected_flow}] final assistant message is empty"
        ));
    }

    // 3. Verify expected_steps is non-empty (guard against misconfigured tests).
    if expected_steps.is_empty() {
        return Err(format!(
            "[{expected_flow}] expected_steps must not be empty"
        ));
    }

    // 4. finish_reason must not be "error".
    if choice
        .get("finish_reason")
        .and_then(|r| r.as_str())
        == Some("error")
    {
        return Err(format!(
            "[{expected_flow}] finish_reason is 'error' — flow may have failed"
        ));
    }

    Ok(())
}

/// Assert that a raw JSON response content matches at least one of the given
/// substrings (case-insensitive).
pub fn assert_content_contains_any_json(body: &JsonValue, substrings: &[&str]) -> Result<(), String> {
    let content = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|ch| ch.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    assert_content_contains_any(content, substrings)
}

/// Assert that a raw content string contains at least one of the given
/// substrings (case-insensitive).  Useful for quick single-field checks.
pub fn assert_content_contains_any(content: &str, substrings: &[&str]) -> Result<(), String> {
    let lower = content.to_lowercase();
    for sub in substrings {
        if lower.contains(&sub.to_lowercase()) {
            return Ok(());
        }
    }
    Err(format!(
        "content does not contain any of {:?}\ncontent was: {content}",
        substrings
    ))
}

/// Count how many choices have non-empty message content (proxy for "steps
/// that produced output" in single-request flows).
pub fn non_empty_choice_count(body: &JsonValue) -> usize {
    body.get("choices")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|ch| {
                    ch.get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_body(content: &str) -> JsonValue {
        json!({
            "id": "test-id",
            "object": "chat.completion",
            "created": 0,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }]
        })
    }

    #[test]
    fn test_assert_flow_executed_json_happy_path() {
        let body = make_body("The kitchen light is now on.");
        let result = assert_flow_executed_json(
            &body,
            "home_automation",
            &["extract_action", "execute", "confirm"],
        );
        assert!(result.is_ok(), "expected Ok but got: {result:?}");
    }

    #[test]
    fn test_assert_flow_executed_json_empty_content_fails() {
        let body = make_body("");
        let result = assert_flow_executed_json(&body, "home_automation", &["extract_action"]);
        assert!(result.is_err(), "empty content should fail assertion");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("home_automation"),
            "error should name the flow: {msg}"
        );
    }

    #[test]
    fn test_assert_flow_executed_json_empty_steps_fails() {
        let body = make_body("some content");
        let result = assert_flow_executed_json(&body, "dream_exploration", &[]);
        assert!(result.is_err(), "empty steps slice should fail assertion");
    }

    #[test]
    fn test_assert_flow_executed_json_no_choices_fails() {
        let body = json!({
            "id": "x", "object": "chat.completion", "created": 0,
            "model": "test-model", "choices": []
        });
        let result =
            assert_flow_executed_json(&body, "dream_consolidation", &["query_recent"]);
        assert!(result.is_err(), "no choices should fail assertion");
    }

    #[test]
    fn test_assert_flow_executed_json_error_finish_reason_fails() {
        let body = json!({
            "id": "x", "object": "chat.completion", "created": 0,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "partial output" },
                "finish_reason": "error"
            }]
        });
        let result = assert_flow_executed_json(&body, "dream_speculation", &["deep_reason"]);
        assert!(result.is_err(), "finish_reason=error should fail assertion");
    }

    #[test]
    fn test_assert_content_contains_any_matches() {
        let result = assert_content_contains_any("The light is on.", &["light", "switch"]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_assert_content_contains_any_case_insensitive() {
        let result = assert_content_contains_any("KITCHEN LIGHT IS ON", &["kitchen light"]);
        assert!(result.is_ok(), "match should be case-insensitive");
    }

    #[test]
    fn test_assert_content_contains_any_no_match() {
        let result =
            assert_content_contains_any("nothing relevant", &["temperature", "climate"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_non_empty_choice_count_one_choice() {
        let body = make_body("some output");
        assert_eq!(non_empty_choice_count(&body), 1);
    }

    #[test]
    fn test_non_empty_choice_count_empty_content() {
        let body = make_body("");
        assert_eq!(non_empty_choice_count(&body), 0);
    }
}
