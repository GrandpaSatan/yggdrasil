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

/// An event returned by the recall endpoint — no full cause/effect text by default.
///
/// When `include_text: true` is set in the RecallQuery, the `cause` and `effect` fields
/// are populated from PostgreSQL. Otherwise they remain None (metadata-only response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngramEvent {
    pub id: Uuid,
    pub similarity: f64,
    pub tier: MemoryTier,
    pub tags: Vec<String>,
    pub trigger: EngramTrigger,
    pub created_at: DateTime<Utc>,
    pub access_count: i64,
    /// Full cause text — populated only when `include_text: true` in the recall request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// Full effect text — populated only when `include_text: true` in the recall request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
}

/// Request for event-based engram recall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallQuery {
    pub text: String,
    #[serde(default = "default_query_limit")]
    pub limit: usize,
    /// When true, populate `cause` and `effect` on returned EngramEvents.
    /// Backward-compatible: callers that omit this field get the original metadata-only response.
    #[serde(default)]
    pub include_text: Option<bool>,
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

// --- Sprint 044: Autonomous memory pipeline types ---

/// Request for the autonomous memory ingest endpoint (`POST /api/v1/auto-ingest`).
///
/// Hook scripts send this after every Edit/Write/Bash tool invocation.
/// Mimir SDR-encodes the content, matches against insight templates, and stores
/// an engram only when the Hamming similarity exceeds the configured threshold.
#[derive(Debug, Serialize, Deserialize)]
pub struct AutoIngestRequest {
    /// Text to classify and potentially store.
    pub content: String,
    /// Originating tool identifier, e.g. "Edit", "Write", "Bash".
    pub source: String,
    /// "pre_tool" or "post_tool".
    pub event_type: String,
    /// Hostname of the workstation that generated this event.
    pub workstation: String,
    /// File being edited/written, if applicable.
    #[serde(default)]
    pub file_path: Option<String>,
    /// Project identifier for scoping, e.g. "yggdrasil".
    #[serde(default)]
    pub project: Option<String>,
}

/// Response from the autonomous memory ingest endpoint.
#[derive(Debug, Serialize, Deserialize)]
pub struct AutoIngestResponse {
    /// Whether an engram was created.
    pub stored: bool,
    /// UUID of the created engram, if stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engram_id: Option<Uuid>,
    /// Name of the insight template that matched, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_template: Option<String>,
    /// Hamming similarity score against the matched template.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f64>,
    /// Why the engram was not stored, e.g. "below_threshold", "duplicate", "cooldown".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
}
