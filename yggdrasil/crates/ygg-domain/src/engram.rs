use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Memory tier classification for engrams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    /// Permanent facts, always included in context.
    Core,
    /// Recent interactions, vector-searchable with rolling window.
    Recall,
    /// Summarized history, accessed when Recall doesn't match.
    Archival,
}

impl MemoryTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Recall => "recall",
            Self::Archival => "archival",
        }
    }
}

impl std::fmt::Display for MemoryTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stored cause-effect memory unit.
///
/// Compatible with the Fergus `engram_client.rs` API contract:
/// the query endpoint returns `{ id, cause, effect, similarity }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Engram {
    pub id: Uuid,
    pub cause: String,
    pub effect: String,
    #[serde(default)]
    pub similarity: f64,
    pub tier: MemoryTier,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub access_count: i64,
    pub last_accessed: DateTime<Utc>,
}

/// Payload for storing a new engram (or updating an existing one).
///
/// Matches the Fergus `NewEngram` struct: `{ cause, effect }`.
/// When `id` is provided, performs an update-by-ID (bypasses novelty gate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEngram {
    /// If set, update this existing engram instead of creating a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub cause: String,
    pub effect: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// When true, bypass the novelty gate and force-create a new engram even
    /// when a near-duplicate exists.  Ignored on the update-by-ID path.
    #[serde(default)]
    pub force: bool,
}

/// Response after storing an engram.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreResponse {
    pub id: Uuid,
}

/// Query request for engram retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngramQuery {
    pub text: String,
    #[serde(default = "default_query_limit")]
    pub limit: usize,
}

fn default_query_limit() -> usize {
    5
}

/// Memory system statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub core_count: i64,
    pub recall_count: i64,
    pub archival_count: i64,
    /// Configured maximum Recall tier capacity.
    pub recall_capacity: i64,
    /// Timestamp of the oldest engram in Recall tier (None if empty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_recall_at: Option<DateTime<Utc>>,
}

// --- Sprint 015: Event-based engram types ---

/// Trigger type for event-based memory recall.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngramTrigger {
    /// A known fact — use for confidence boosting or constraint.
    Fact { label: String },
    /// A prior decision — use for consistency checking.
    Decision { label: String },
    /// A behavioral pattern — use for routing or model selection.
    Pattern { label: String, intent_hint: String },
}

/// An event returned by the recall endpoint — no full cause/effect text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngramEvent {
    pub id: Uuid,
    pub similarity: f64,
    pub tier: MemoryTier,
    pub tags: Vec<String>,
    pub trigger: EngramTrigger,
    pub created_at: DateTime<Utc>,
    pub access_count: i64,
}

/// Request for event-based engram recall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallQuery {
    pub text: String,
    #[serde(default = "default_query_limit")]
    pub limit: usize,
}

/// Response from event-based recall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallResponse {
    pub events: Vec<EngramEvent>,
    pub core_events: Vec<EngramEvent>,
    /// Hex-encoded 256-bit SDR of the query text (optional, for session drift tracking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_sdr_hex: Option<String>,
}
