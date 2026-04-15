//! Fabric service configuration — loaded from env vars with sane defaults.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FabricConfig {
    /// HTTP bind address. Defaults to 0.0.0.0:11450 (Hugin).
    pub bind_addr: String,

    /// Valkey / Redis connection URL. Set empty to use the in-memory
    /// fallback store (dev/tests). Defaults to Munin USB4 side:
    /// `redis://10.0.65.8:6479`.
    pub valkey_url: String,

    /// TEI embedding endpoint. Defaults to Munin TEI :11438.
    pub tei_url: String,

    /// Embedding dimension. all-MiniLM-L6-v2 = 384.
    pub embed_dim: usize,

    /// Flow record TTL in seconds. Defaults to 24 hours.
    pub flow_ttl_secs: u64,

    /// Max records retained per flow before LRU eviction. Flows rarely
    /// exceed 20 steps; cap at 64 to be safe.
    pub max_records_per_flow: usize,
}

impl FabricConfig {
    pub fn from_env() -> Self {
        Self {
            bind_addr: std::env::var("FABRIC_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:11450".to_string()),
            valkey_url: std::env::var("FABRIC_VALKEY_URL")
                .unwrap_or_else(|_| "redis://10.0.65.8:6479".to_string()),
            tei_url: std::env::var("FABRIC_TEI_URL")
                .unwrap_or_else(|_| "http://10.0.65.8:11438".to_string()),
            embed_dim: std::env::var("FABRIC_EMBED_DIM")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(384),
            flow_ttl_secs: std::env::var("FABRIC_FLOW_TTL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(86_400),
            max_records_per_flow: std::env::var("FABRIC_MAX_RECORDS_PER_FLOW")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(64),
        }
    }
}
