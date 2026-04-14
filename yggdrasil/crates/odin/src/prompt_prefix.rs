//! Deterministic prompt prefix construction (Sprint 061, L3 of the latency
//! strategy).
//!
//! Ollama's built-in prefix cache re-uses KV state byte-for-byte across calls
//! that share the same leading prompt bytes. This module centralises the
//! message-construction used by multi-step swarm flows so that sibling steps
//! on the SAME model (e.g. Munin's nemotron drafter + nemotron refiner) hit
//! the cache and skip prefill on the second call.
//!
//! Key invariants:
//!   1. The system prompt bytes MUST be identical between cache-sharing steps.
//!   2. The user message prefix SHOULD be identical; divergence (review notes,
//!      prior draft) appends AT THE END so the cache hits on the shared prefix.
//!   3. Field order in any serialised struct matters — use
//!      [`build_deterministic_messages`] instead of inline construction.
//!
//! The actual Ollama keep-alive (`keep_alive=-1`) is set at the backend level
//! via the systemd unit `yggdrasil-ollama-warm.service`; this module handles
//! the prompt-shape half of the contract.

use crate::openai::OllamaMessage;

/// Canonical system prompt used by swarm-chat's drafter and refiner steps.
/// Both steps share this verbatim so Ollama's prefix cache hits.
///
/// The refiner's behaviour differs from the drafter's via the user-content
/// suffix (which appends the reviewer's critique), NOT the system prompt.
pub const SWARM_SHARED_SYSTEM: &str = "You are Yggdrasil, a concise and accurate assistant. Answer the user's question directly. Prefer short, well-structured responses over long ones.";

/// Build the refiner's user content so the drafter's input bytes are the
/// prefix. The reviewer's critique and original draft append after the
/// user's question, keeping divergence late in the prompt.
///
/// Shape:
/// ```text
/// <user_message>
///
/// <!-- prior draft -->
/// <draft>
///
/// <!-- reviewer notes -->
/// <review>
/// ```
pub fn format_refiner_input(user_message: &str, draft: &str, review: &str) -> String {
    let mut out = String::with_capacity(user_message.len() + draft.len() + review.len() + 96);
    out.push_str(user_message);
    out.push_str("\n\n<!-- prior draft -->\n");
    out.push_str(draft);
    out.push_str("\n\n<!-- reviewer notes -->\n");
    out.push_str(review);
    out
}

/// Build a deterministic `Vec<OllamaMessage>` for a flow step. System prompt
/// first (if present), then a single user message. Field order is stable so
/// serde_json serialisation is byte-deterministic for identical inputs.
pub fn build_deterministic_messages(
    system: Option<&str>,
    user_content: &str,
) -> Vec<OllamaMessage> {
    let mut msgs = Vec::with_capacity(2);
    if let Some(sys) = system {
        msgs.push(OllamaMessage::new("system", sys));
    }
    msgs.push(OllamaMessage::new("user", user_content));
    msgs
}

/// Return the length in bytes of the shared leading prefix of two strings.
/// Useful for observability / debugging cache-hit behaviour in logs.
pub fn shared_prefix_len(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_is_deterministic_for_same_inputs() {
        let a = build_deterministic_messages(Some(SWARM_SHARED_SYSTEM), "What is a lifetime?");
        let b = build_deterministic_messages(Some(SWARM_SHARED_SYSTEM), "What is a lifetime?");
        let ja = serde_json::to_string(&a).unwrap();
        let jb = serde_json::to_string(&b).unwrap();
        assert_eq!(ja, jb, "identical inputs must produce identical JSON bytes");
    }

    #[test]
    fn drafter_and_refiner_share_system_prefix() {
        let user = "What is a lifetime?";
        let draft = "A lifetime is a region.";
        let review = "Clarify scope vs lifetime.";
        let drafter = build_deterministic_messages(Some(SWARM_SHARED_SYSTEM), user);
        let refiner_user = format_refiner_input(user, draft, review);
        let refiner = build_deterministic_messages(Some(SWARM_SHARED_SYSTEM), &refiner_user);

        // System message is byte-identical.
        assert_eq!(drafter[0].content, refiner[0].content);

        // Refiner's user message begins with the drafter's user message bytes.
        let shared = shared_prefix_len(&drafter[1].content, &refiner[1].content);
        assert_eq!(
            shared,
            drafter[1].content.len(),
            "refiner user content must start with the drafter's full user message"
        );
    }

    #[test]
    fn refiner_input_contains_all_components() {
        let s = format_refiner_input("Q?", "draft-text", "review-text");
        assert!(s.starts_with("Q?"));
        assert!(s.contains("draft-text"));
        assert!(s.contains("review-text"));
        assert!(s.contains("<!-- prior draft -->"));
        assert!(s.contains("<!-- reviewer notes -->"));
    }

    #[test]
    fn shared_prefix_len_identifies_divergence_point() {
        assert_eq!(shared_prefix_len("abcdef", "abcXYZ"), 3);
        assert_eq!(shared_prefix_len("same", "same"), 4);
        assert_eq!(shared_prefix_len("abc", ""), 0);
        assert_eq!(shared_prefix_len("", ""), 0);
    }

    #[test]
    fn no_system_prompt_omits_system_message() {
        let msgs = build_deterministic_messages(None, "hi");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }
}
