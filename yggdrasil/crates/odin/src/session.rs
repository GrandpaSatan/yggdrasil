/// In-memory conversation session store.
///
/// Tracks multi-turn conversations so clients can send only new messages
/// instead of resending the full history on every request. Sessions are
/// ephemeral (lost on restart) — clients fall back to the standard OpenAI
/// protocol of resending full history, which Odin handles transparently.
///
/// ## Thread safety
///
/// `SessionStore` wraps a `DashMap` for lock-free concurrent access from
/// Axum handler tasks. The background reaper task uses `retain()` which
/// holds shard locks briefly.
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use uuid::Uuid;

use ygg_domain::config::SessionConfig;
use ygg_domain::sdr as sdr_core;

/// A single message stored in session history.
#[derive(Debug, Clone)]
pub struct CompactMessage {
    pub role: String,
    pub content: String,
    pub tokens_estimate: usize,
}

impl CompactMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        let content = content.into();
        let tokens_estimate = content.len() / 4;
        Self {
            role: role.into(),
            tokens_estimate,
            content,
        }
    }
}

/// A compressed summary of a completed (or summarized) session, stored per project.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    /// Number of turns the session had when summarized.
    pub turn_count: usize,
    /// The rolling summary text.
    pub summary: String,
}

/// A conversation session with accumulated message history.
#[derive(Debug, Clone)]
pub struct ConversationSession {
    pub id: String,
    pub messages: Vec<CompactMessage>,
    /// Compressed summary of old turns (populated by rolling summarization).
    pub summary: Option<String>,
    /// Optional project this session belongs to (for cross-window context).
    pub project_id: Option<String>,
    pub created_at: Instant,
    pub last_accessed: Instant,
    /// Running OR-accumulation of query SDRs for topic drift detection.
    pub session_sdr: sdr_core::Sdr,
    /// Number of messages accumulated into session_sdr.
    pub sdr_message_count: usize,
}

impl ConversationSession {
    fn new(id: String, project_id: Option<String>) -> Self {
        let now = Instant::now();
        Self {
            id,
            messages: Vec::new(),
            summary: None,
            project_id,
            created_at: now,
            last_accessed: now,
            session_sdr: sdr_core::ZERO,
            sdr_message_count: 0,
        }
    }

    /// Update the session SDR with a new query SDR (OR accumulation).
    ///
    /// Returns the drift score: Hamming similarity between the new query SDR
    /// and the accumulated session SDR. Low values (< 0.5) indicate topic drift.
    /// Returns `None` if this is the first message (no drift to compute).
    pub fn update_sdr(&mut self, query_sdr: &sdr_core::Sdr) -> Option<f64> {
        let drift = if self.sdr_message_count > 0 {
            Some(sdr_core::hamming_similarity(query_sdr, &self.session_sdr))
        } else {
            None
        };
        self.session_sdr = sdr_core::or(&self.session_sdr, query_sdr);
        self.sdr_message_count += 1;
        drift
    }

    /// Estimated total tokens across all messages + summary.
    pub fn total_tokens_estimate(&self) -> usize {
        let msg_tokens: usize = self.messages.iter().map(|m| m.tokens_estimate).sum();
        let summary_tokens = self
            .summary
            .as_ref()
            .map(|s| s.len() / 4)
            .unwrap_or(0);
        msg_tokens + summary_tokens
    }
}

/// In-memory session store backed by DashMap.
///
/// Uses `Arc<DashMap>` so that cloning `SessionStore` (which Axum does for
/// each handler invocation) shares the same underlying session data.
///
/// Also maintains a per-project ring-buffer of recent `SessionSummary` entries,
/// populated when sessions are summarized or evicted. This enables cross-window
/// context continuity: a new session in the same project can inject the last
/// N sessions' summaries as low-priority context.
#[derive(Clone)]
pub struct SessionStore {
    sessions: Arc<DashMap<String, ConversationSession>>,
    /// Per-project ring buffer of recent session summaries (max 10 per project).
    project_sessions: Arc<DashMap<String, VecDeque<SessionSummary>>>,
    config: SessionConfig,
}

impl SessionStore {
    pub fn new(config: SessionConfig) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            project_sessions: Arc::new(DashMap::new()),
            config,
        }
    }

    /// Get or create a session by ID, optionally associated with a project.
    ///
    /// If `session_id` is `Some`, look up the existing session. If not found,
    /// create a new one with that ID. If `None`, generate a new UUID session.
    /// `project_id` is stored on the session for cross-window context injection.
    pub fn resolve(&self, session_id: Option<&str>, project_id: Option<&str>) -> String {
        match session_id {
            Some(id) => {
                // Touch existing session or create new one.
                if let Some(mut entry) = self.sessions.get_mut(id) {
                    entry.last_accessed = Instant::now();
                    id.to_string()
                } else {
                    // Enforce max sessions before creating a new one.
                    self.evict_if_full();
                    let session = ConversationSession::new(id.to_string(), project_id.map(str::to_string));
                    self.sessions.insert(id.to_string(), session);
                    id.to_string()
                }
            }
            None => {
                let id = Uuid::new_v4().to_string();
                self.evict_if_full();
                let session = ConversationSession::new(id.clone(), project_id.map(str::to_string));
                self.sessions.insert(id.clone(), session);
                id
            }
        }
    }

    /// Append messages to a session's history.
    pub fn append_messages(&self, session_id: &str, messages: &[CompactMessage]) {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.messages.extend(messages.iter().cloned());
            entry.last_accessed = Instant::now();
        }
    }

    /// Update the session's SDR with a new query SDR from a recall response.
    ///
    /// Returns the drift score (Hamming similarity to accumulated session SDR).
    /// Returns `None` if the session doesn't exist or this is the first message.
    /// If drift is below 0.5, resets the session SDR to just this query.
    pub fn update_session_sdr(
        &self,
        session_id: &str,
        query_sdr: &sdr_core::Sdr,
    ) -> Option<f64> {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            let drift = entry.update_sdr(query_sdr);
            // On high drift, reset to just the current query
            if let Some(d) = drift {
                if d < 0.5 {
                    entry.session_sdr = *query_sdr;
                    entry.sdr_message_count = 1;
                }
            }
            drift
        } else {
            None
        }
    }

    /// Get a snapshot of the session for context packing.
    pub fn get_session(&self, session_id: &str) -> Option<ConversationSession> {
        self.sessions.get(session_id).map(|entry| entry.clone())
    }

    /// Update the session summary (used by rolling summarization).
    ///
    /// If the session belongs to a project, also pushes a `SessionSummary`
    /// entry to the per-project ring buffer so future sessions can load it.
    pub fn set_summary(&self, session_id: &str, summary: String, messages_consumed: usize) {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            // Remove the oldest N messages that were summarized.
            if messages_consumed <= entry.messages.len() {
                entry.messages.drain(..messages_consumed);
            }
            let turn_count = entry.messages.len() / 2 + messages_consumed / 2;
            let project_id = entry.project_id.clone();
            entry.summary = Some(summary.clone());
            entry.last_accessed = Instant::now();
            drop(entry); // release lock before calling push_project_summary

            if let Some(pid) = project_id {
                self.push_project_summary(&pid, SessionSummary {
                    session_id: session_id.to_string(),
                    turn_count,
                    summary,
                });
            }
        }
    }

    /// Push a summary entry to the project ring buffer (max 10 per project).
    fn push_project_summary(&self, project_id: &str, summary: SessionSummary) {
        let mut deque = self.project_sessions
            .entry(project_id.to_string())
            .or_insert_with(VecDeque::new);
        deque.push_front(summary);
        if deque.len() > 10 {
            deque.pop_back();
        }
    }

    /// Return the last `limit` session summaries for a project, formatted as markdown.
    ///
    /// Returns an empty string if no project summaries exist.
    pub fn get_project_history(&self, project_id: &str, limit: usize) -> String {
        let deque = match self.project_sessions.get(project_id) {
            Some(d) => d,
            None => return String::new(),
        };
        if deque.is_empty() {
            return String::new();
        }

        let mut out = String::from("## Previous Session Context\n\n");
        for (i, s) in deque.iter().take(limit).enumerate() {
            if i > 0 {
                out.push_str("---\n");
            }
            out.push_str(&format!(
                "[Session {} — {} turns]\n{}\n\n",
                i + 1,
                s.turn_count,
                s.summary
            ));
        }
        out
    }

    /// Current number of active sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Evict expired sessions (called by the reaper task).
    ///
    /// Sessions that belong to a project and have a rolling summary are moved
    /// into the project ring buffer before eviction, preserving context for
    /// future sessions in the same project.
    pub fn reap_expired(&self) {
        let ttl = std::time::Duration::from_secs(self.config.session_ttl_secs);
        let now = Instant::now();

        // Collect expired sessions before removing them so we can save summaries.
        let expired: Vec<ConversationSession> = self.sessions
            .iter()
            .filter(|e| now.duration_since(e.value().last_accessed) >= ttl)
            .map(|e| e.value().clone())
            .collect();

        let before = self.sessions.len();
        self.sessions.retain(|_, session| {
            now.duration_since(session.last_accessed) < ttl
        });
        let evicted = before.saturating_sub(self.sessions.len());

        // Move project summaries from evicted sessions into the ring buffer.
        for session in expired {
            if let (Some(pid), Some(summary)) = (session.project_id, session.summary) {
                self.push_project_summary(&pid, SessionSummary {
                    session_id: session.id,
                    turn_count: session.messages.len() / 2,
                    summary,
                });
            }
        }

        if evicted > 0 {
            tracing::debug!(evicted, remaining = self.sessions.len(), "session reaper cycle");
        }
    }

    /// Evict the oldest session if at capacity.
    fn evict_if_full(&self) {
        if self.sessions.len() >= self.config.max_sessions {
            // Find the least recently accessed session.
            let oldest = self
                .sessions
                .iter()
                .min_by_key(|entry| entry.value().last_accessed)
                .map(|entry| entry.key().clone());

            if let Some(key) = oldest {
                self.sessions.remove(&key);
                tracing::debug!(session_id = %key, "evicted oldest session (at capacity)");
            }
        }
    }
}

/// Spawn the background session reaper task.
///
/// Runs every 60 seconds, evicts sessions that have been idle longer than
/// `session_ttl_secs`. Returns a `JoinHandle` for clean shutdown.
pub fn spawn_reaper(store: SessionStore) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            store.reap_expired();
            metrics::gauge!("odin_active_sessions").set(store.len() as f64);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SessionConfig {
        SessionConfig {
            max_sessions: 3,
            session_ttl_secs: 1,
            context_budget_tokens: 15000,
            generation_reserve: 2048,
        }
    }

    #[test]
    fn resolve_creates_new_session() {
        let store = SessionStore::new(test_config());
        let id = store.resolve(None, None);
        assert!(!id.is_empty());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn resolve_with_id_reuses_session() {
        let store = SessionStore::new(test_config());
        let id = store.resolve(Some("test-session"), None);
        assert_eq!(id, "test-session");
        assert_eq!(store.len(), 1);

        // Resolve again — should not create a new session.
        let id2 = store.resolve(Some("test-session"), None);
        assert_eq!(id2, "test-session");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn append_and_retrieve_messages() {
        let store = SessionStore::new(test_config());
        store.resolve(Some("s1"), None);
        store.append_messages("s1", &[
            CompactMessage::new("user", "hello"),
            CompactMessage::new("assistant", "hi there"),
        ]);

        let session = store.get_session("s1").unwrap();
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, "user");
        assert_eq!(session.messages[1].content, "hi there");
    }

    #[test]
    fn evicts_oldest_when_full() {
        let store = SessionStore::new(test_config()); // max_sessions = 3
        store.resolve(Some("s1"), None);
        store.resolve(Some("s2"), None);
        store.resolve(Some("s3"), None);
        assert_eq!(store.len(), 3);

        // This should evict the oldest (s1).
        store.resolve(Some("s4"), None);
        assert_eq!(store.len(), 3);
        assert!(store.get_session("s1").is_none());
    }

    #[test]
    fn reap_expired_removes_old_sessions() {
        let store = SessionStore::new(test_config()); // ttl = 1s
        store.resolve(Some("s1"), None);

        // Wait for TTL to expire.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        store.reap_expired();
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn set_summary_replaces_old_messages() {
        let store = SessionStore::new(test_config());
        store.resolve(Some("s1"), None);
        store.append_messages("s1", &[
            CompactMessage::new("user", "msg1"),
            CompactMessage::new("assistant", "resp1"),
            CompactMessage::new("user", "msg2"),
            CompactMessage::new("assistant", "resp2"),
        ]);

        store.set_summary("s1", "summary of first 2 turns".to_string(), 2);

        let session = store.get_session("s1").unwrap();
        assert_eq!(session.summary.as_deref(), Some("summary of first 2 turns"));
        assert_eq!(session.messages.len(), 2); // msg2 + resp2 remain
        assert_eq!(session.messages[0].content, "msg2");
    }

    // --- SDR drift tracking tests ---

    #[test]
    fn first_sdr_update_returns_none() {
        let store = SessionStore::new(test_config());
        store.resolve(Some("s1"), None);

        let sdr: sdr_core::Sdr = [0xFF, 0, 0, 0];
        let drift = store.update_session_sdr("s1", &sdr);
        assert!(drift.is_none(), "first message should return None (no prior state)");
    }

    #[test]
    fn same_topic_has_high_drift_score() {
        let store = SessionStore::new(test_config());
        store.resolve(Some("s1"), None);

        // First message — sets session SDR
        let sdr_a: sdr_core::Sdr = [0xFF, 0xFF, 0, 0];
        store.update_session_sdr("s1", &sdr_a);

        // Second message — identical SDR should give similarity = 1.0
        let drift = store.update_session_sdr("s1", &sdr_a);
        assert_eq!(drift, Some(1.0), "identical SDR should have drift score 1.0");
    }

    #[test]
    fn similar_topic_has_moderate_drift_score() {
        let store = SessionStore::new(test_config());
        store.resolve(Some("s1"), None);

        let sdr_a: sdr_core::Sdr = [0xFF, 0xFF, 0, 0]; // 16 bits in words 0,1
        store.update_session_sdr("s1", &sdr_a);

        // Second message — overlapping but not identical
        let sdr_b: sdr_core::Sdr = [0xFF, 0, 0xFF, 0]; // shares word 0, differs in 1,2
        let drift = store.update_session_sdr("s1", &sdr_b);
        let d = drift.expect("should have drift score");
        // session_sdr after first = [0xFF, 0xFF, 0, 0]
        // sdr_b = [0xFF, 0, 0xFF, 0]
        // hamming distance = bits differing in word1 (8) + word2 (8) = 16
        // similarity = 1 - 16/256 = 0.9375
        assert!(d > 0.5, "similar topic should have drift > 0.5, got {d}");
        assert!(d < 1.0, "not identical, so drift < 1.0, got {d}");
    }

    #[test]
    fn topic_drift_resets_session_sdr() {
        let store = SessionStore::new(test_config());
        store.resolve(Some("s1"), None);

        // First message: bits in words 0,1
        let sdr_a: sdr_core::Sdr = [u64::MAX, u64::MAX, 0, 0];
        store.update_session_sdr("s1", &sdr_a);

        // Second message: completely disjoint — bits only in words 2,3
        let sdr_b: sdr_core::Sdr = [0, 0, u64::MAX, u64::MAX];
        let drift = store.update_session_sdr("s1", &sdr_b);
        let d = drift.expect("should have drift score");
        // hamming distance = 256 (all bits differ) → similarity = 0.0
        assert!(d < 0.5, "disjoint SDR should trigger drift, got {d}");

        // After drift, session SDR should be reset to sdr_b (not accumulated)
        let session = store.get_session("s1").unwrap();
        assert_eq!(session.session_sdr, sdr_b, "session SDR should reset to new topic");
        assert_eq!(session.sdr_message_count, 1, "message count should reset to 1");
    }

    #[test]
    fn or_accumulation_grows_session_sdr() {
        let store = SessionStore::new(test_config());
        store.resolve(Some("s1"), None);

        // Three on-topic messages with different but overlapping bits
        let sdr_a: sdr_core::Sdr = [0xFF, 0, 0, 0]; // 8 bits
        let sdr_b: sdr_core::Sdr = [0xFF, 0xFF, 0, 0]; // 16 bits (superset of a)
        let sdr_c: sdr_core::Sdr = [0xFF, 0xFF, 0xFF, 0]; // 24 bits (superset of b)

        store.update_session_sdr("s1", &sdr_a);
        store.update_session_sdr("s1", &sdr_b);
        store.update_session_sdr("s1", &sdr_c);

        let session = store.get_session("s1").unwrap();
        // OR of all three = sdr_c (since c is superset)
        assert_eq!(session.session_sdr, sdr_c);
        assert_eq!(session.sdr_message_count, 3);
    }

    #[test]
    fn nonexistent_session_returns_none() {
        let store = SessionStore::new(test_config());
        let sdr: sdr_core::Sdr = [0xFF, 0, 0, 0];
        let drift = store.update_session_sdr("nonexistent", &sdr);
        assert!(drift.is_none());
    }
}
