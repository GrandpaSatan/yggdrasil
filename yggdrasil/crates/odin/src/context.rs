/// Priority-based context packing for the LLM context window.
///
/// Fits session history, RAG context, and system prompt into a fixed token
/// budget using a strict priority order:
///
///   1. System prompt          — always included
///   2. Session summary        — if exists (compressed old turns)
///   3. Recent history         — last N user/assistant turn pairs
///   4. RAG code context       — injected as a system message
///   5. Older history turns    — fill remaining space
///
/// Token estimation uses `len / 4` (chars-to-tokens heuristic). This is
/// intentionally conservative — overestimating tokens wastes some context
/// but never causes truncation errors at the Ollama level.
use crate::openai::{ChatMessage, Role};
use crate::session::ConversationSession;

/// Context packing budget.
pub struct ContextBudget {
    /// Total token budget (e.g., 14000 for a 16K context window).
    pub total_budget: usize,
    /// Tokens reserved for the model's generation output.
    pub generation_reserve: usize,
}

impl ContextBudget {
    /// Available tokens for input (system prompt + history + RAG).
    fn available(&self) -> usize {
        self.total_budget.saturating_sub(self.generation_reserve)
    }

    /// Pack a session's history + RAG into a messages array that fits the budget.
    ///
    /// Returns the assembled message array ready for the LLM. The caller should
    /// NOT add a separate system prompt — it's already included in the output.
    ///
    /// Priority order (highest to lowest):
    ///   1. System prompt
    ///   2. Session summary (rolling compressed history)
    ///   3. Recent history (last 8 messages)
    ///   4. RAG code context
    ///   5. Previous project sessions (cross-window context, capped 500 tokens)
    ///   6. Older history turns (fill remaining space)
    pub fn pack(
        &self,
        session: &ConversationSession,
        rag_context: Option<&str>,
        system_prompt: &str,
        previous_sessions: Option<&str>,
    ) -> Vec<ChatMessage> {
        let budget = self.available();
        let mut used: usize = 0;
        let mut output: Vec<ChatMessage> = Vec::new();

        // ── 1. System prompt (always) ────────────────────────────────
        let system_tokens = system_prompt.len() / 4;
        used += system_tokens;
        output.push(ChatMessage {
            role: Role::System,
            content: system_prompt.to_string(),
        });

        // ── 2. Session summary (if exists) ───────────────────────────
        if let Some(ref summary) = session.summary {
            let summary_tokens = summary.len() / 4;
            if used + summary_tokens < budget {
                used += summary_tokens;
                output.push(ChatMessage {
                    role: Role::System,
                    content: format!("Summary of earlier conversation:\n{summary}"),
                });
            }
        }

        // ── 3. Recent history (last 4 turn pairs = 8 messages) ───────
        // These are always included if they fit.
        let recent_count = session.messages.len().min(8);
        let recent_start = session.messages.len().saturating_sub(recent_count);
        let recent = &session.messages[recent_start..];

        let mut recent_tokens: usize = 0;
        let mut recent_msgs: Vec<ChatMessage> = Vec::new();
        for msg in recent {
            recent_tokens += msg.tokens_estimate;
            recent_msgs.push(ChatMessage {
                role: parse_role(&msg.role),
                content: msg.content.clone(),
            });
        }

        // ── 4. RAG code context ──────────────────────────────────────
        // Inserted between system prompt and history as a system message.
        if let Some(rag) = rag_context {
            let rag_tokens = rag.len() / 4;
            if used + rag_tokens + recent_tokens < budget {
                used += rag_tokens;
                output.push(ChatMessage {
                    role: Role::System,
                    content: rag.to_string(),
                });
            }
            // If RAG doesn't fit, skip it — recent history is more important.
        }

        // ── 5. Previous project sessions (cross-window context) ──────
        // Injected as a system message, capped at 500 tokens. Lowest priority
        // above older history — dropped silently if budget is tight.
        const PREV_SESSIONS_TOKEN_CAP: usize = 500;
        if let Some(prev) = previous_sessions {
            let prev_tokens = prev.len() / 4;
            let capped_tokens = prev_tokens.min(PREV_SESSIONS_TOKEN_CAP);
            if used + capped_tokens + recent_tokens < budget {
                used += capped_tokens;
                // Truncate to token cap if needed.
                let content = if prev_tokens > PREV_SESSIONS_TOKEN_CAP {
                    let char_limit = PREV_SESSIONS_TOKEN_CAP * 4;
                    &prev[..prev.len().min(char_limit)]
                } else {
                    prev
                };
                output.push(ChatMessage {
                    role: Role::System,
                    content: content.to_string(),
                });
            }
        }

        // ── 6. Older history turns (fill remaining space) ────────────
        let older = &session.messages[..recent_start];
        for msg in older {
            if used + msg.tokens_estimate + recent_tokens >= budget {
                break;
            }
            used += msg.tokens_estimate;
            output.push(ChatMessage {
                role: parse_role(&msg.role),
                content: msg.content.clone(),
            });
        }

        // ── 6. Append recent history ─────────────────────────────────
        // Recent messages go last (closest to the generation point).
        used += recent_tokens;
        let _ = used; // suppress unused warning
        output.extend(recent_msgs);

        output
    }
}

/// Parse a role string into the Role enum.
fn parse_role(role: &str) -> Role {
    match role {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        _ => Role::User,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::CompactMessage;

    fn make_session(messages: Vec<(&str, &str)>) -> ConversationSession {
        ConversationSession {
            id: "test".to_string(),
            messages: messages
                .into_iter()
                .map(|(role, content)| CompactMessage::new(role, content))
                .collect(),
            summary: None,
            project_id: None,
            created_at: std::time::Instant::now(),
            last_accessed: std::time::Instant::now(),
            session_sdr: ygg_domain::sdr::ZERO,
            sdr_message_count: 0,
        }
    }

    #[test]
    fn pack_fits_system_and_recent_history() {
        let session = make_session(vec![
            ("user", "hello"),
            ("assistant", "hi there"),
            ("user", "how are you"),
        ]);

        let budget = ContextBudget {
            total_budget: 1000,
            generation_reserve: 200,
        };

        let packed = budget.pack(&session, None, "You are helpful.", None);
        // System prompt + 3 messages (all recent, < 8)
        assert_eq!(packed.len(), 4);
        assert_eq!(packed[0].role, Role::System);
        assert_eq!(packed[1].role, Role::User);
        assert_eq!(packed[1].content, "hello");
    }

    #[test]
    fn pack_includes_rag_when_space_available() {
        let session = make_session(vec![
            ("user", "what is rust"),
        ]);

        let budget = ContextBudget {
            total_budget: 1000,
            generation_reserve: 200,
        };

        let packed = budget.pack(&session, Some("## Code Context\nfn main() {}"), "You are helpful.", None);
        // System prompt + RAG system msg + user message
        assert_eq!(packed.len(), 3);
        assert_eq!(packed[0].role, Role::System); // system prompt
        assert_eq!(packed[1].role, Role::System); // RAG context
        assert!(packed[1].content.contains("Code Context"));
        assert_eq!(packed[2].role, Role::User);
    }

    #[test]
    fn pack_includes_summary_when_present() {
        let mut session = make_session(vec![
            ("user", "latest question"),
        ]);
        session.summary = Some("Previously discussed Rust ownership.".to_string());

        let budget = ContextBudget {
            total_budget: 1000,
            generation_reserve: 200,
        };

        let packed = budget.pack(&session, None, "You are helpful.", None);
        assert_eq!(packed.len(), 3);
        assert!(packed[1].content.contains("Summary of earlier conversation"));
    }

    #[test]
    fn pack_drops_rag_when_tight_budget() {
        let session = make_session(vec![
            ("user", &"x".repeat(2000)), // ~500 tokens
            ("assistant", &"y".repeat(2000)), // ~500 tokens
        ]);

        let budget = ContextBudget {
            total_budget: 1200,
            generation_reserve: 200,
        };

        // Budget = 1000 tokens. System (~4) + recent (~1000) = 1004.
        // RAG won't fit, so it should be skipped.
        let large_rag = &"z".repeat(400); // ~100 tokens
        let packed = budget.pack(&session, Some(large_rag), "You are helpful.", None);

        // Should have system + 2 recent messages (no RAG).
        let has_rag = packed.iter().any(|m| m.content.contains("zzz"));
        assert!(!has_rag);
    }
}
