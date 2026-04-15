//! Shared types for fabric HTTP + storage layers.

use serde::{Deserialize, Serialize};

/// A single published step record, stored per flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FabricRecord {
    pub flow_id: String,
    pub step_n: u32,
    pub model: String,
    pub text: String,
    /// 384-dim embedding (matches TEI all-MiniLM-L6-v2). Stored as f32
    /// so JSON bodies stay readable; internal storage uses bytes.
    pub embedding: Vec<f32>,
    /// Unix epoch seconds.
    pub ts: i64,
}

// ───────── Publish ─────────

#[derive(Debug, Clone, Deserialize)]
pub struct PublishRequest {
    pub flow_id: String,
    pub step_n: u32,
    pub model: String,
    pub text: String,
    /// If absent, the fabric will compute one via TEI.
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishResponse {
    pub flow_id: String,
    pub step_n: u32,
    pub stored: bool,
    pub embedding_dim: usize,
}

// ───────── Query ─────────

#[derive(Debug, Clone, Deserialize)]
pub struct QueryRequest {
    pub flow_id: String,
    /// Either query_text or embedding must be supplied.
    #[serde(default)]
    pub query_text: Option<String>,
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}
fn default_top_k() -> usize { 3 }

#[derive(Debug, Clone, Serialize)]
pub struct QueryHit {
    pub step_n: u32,
    pub model: String,
    pub text: String,
    pub similarity: f32,
    pub ts: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryResponse {
    pub flow_id: String,
    pub hits: Vec<QueryHit>,
}

// ───────── Done ─────────

#[derive(Debug, Clone, Deserialize)]
pub struct DoneRequest {
    pub flow_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoneResponse {
    pub flow_id: String,
    pub evicted: usize,
}

// ───────── History ─────────

#[derive(Debug, Clone, Serialize)]
pub struct HistoryResponse {
    pub flow_id: String,
    pub records: Vec<FabricRecord>,
}
