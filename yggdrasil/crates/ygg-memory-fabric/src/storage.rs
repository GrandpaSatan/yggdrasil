//! Storage abstraction for fabric records.
//!
//! Primary backend is Valkey (Redis-compatible) on Munin over USB4.
//! Dev + unit tests use the in-memory DashMap fallback.
//!
//! Records live under key `fabric:flow:<flow_id>:records` as a list
//! (append-only). Query scans the list and cosine-compares embeddings.
//! Flow sizes are small (typically <20 steps) so brute-force is fine.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use redis::{AsyncCommands, aio::ConnectionManager};

use crate::types::FabricRecord;

#[async_trait]
pub trait FabricStore: Send + Sync + 'static {
    /// Append a record to a flow. Enforces max_records via trim.
    async fn append(&self, record: FabricRecord, max_records: usize, ttl_secs: u64) -> Result<()>;

    /// Fetch all records for a flow.
    async fn list(&self, flow_id: &str) -> Result<Vec<FabricRecord>>;

    /// Delete all records for a flow. Returns count evicted.
    async fn evict(&self, flow_id: &str) -> Result<usize>;

    /// Approximate byte count currently stored. Best-effort for metrics.
    async fn bytes_stored(&self) -> u64;
}

// ───────── In-memory backend ─────────

pub struct MemoryStore {
    flows: DashMap<String, Vec<FabricRecord>>,
}

impl MemoryStore {
    pub fn new() -> Arc<Self> { Arc::new(Self { flows: DashMap::new() }) }
}

#[async_trait]
impl FabricStore for MemoryStore {
    async fn append(&self, record: FabricRecord, max_records: usize, _ttl: u64) -> Result<()> {
        let mut entry = self.flows.entry(record.flow_id.clone()).or_default();
        entry.push(record);
        if entry.len() > max_records {
            let drop = entry.len() - max_records;
            entry.drain(..drop);
        }
        Ok(())
    }

    async fn list(&self, flow_id: &str) -> Result<Vec<FabricRecord>> {
        Ok(self.flows.get(flow_id).map(|v| v.clone()).unwrap_or_default())
    }

    async fn evict(&self, flow_id: &str) -> Result<usize> {
        Ok(self.flows.remove(flow_id).map(|(_, v)| v.len()).unwrap_or(0))
    }

    async fn bytes_stored(&self) -> u64 {
        self.flows.iter().map(|e| {
            e.value().iter().map(|r| {
                (r.text.len() + r.model.len() + r.embedding.len() * 4) as u64
            }).sum::<u64>()
        }).sum()
    }
}

// ───────── Valkey (Redis) backend ─────────

pub struct ValkeyStore {
    conn: ConnectionManager,
}

impl ValkeyStore {
    pub async fn connect(url: &str) -> Result<Arc<Self>> {
        let client = redis::Client::open(url)?;
        let conn = ConnectionManager::new(client).await?;
        Ok(Arc::new(Self { conn }))
    }

    fn key(flow_id: &str) -> String { format!("fabric:flow:{flow_id}:records") }
}

#[async_trait]
impl FabricStore for ValkeyStore {
    async fn append(&self, record: FabricRecord, max_records: usize, ttl_secs: u64) -> Result<()> {
        let mut conn = self.conn.clone();
        let key = Self::key(&record.flow_id);
        let blob = serde_json::to_vec(&record)?;
        // RPUSH + LTRIM + EXPIRE in a single pipeline
        let (_, _, _): (i64, (), bool) = redis::pipe()
            .rpush(&key, blob).ignore()
            .cmd("LTRIM").arg(&key).arg(-(max_records as i64)).arg(-1).ignore()
            .expire(&key, ttl_secs as i64).ignore()
            .query_async(&mut conn).await?;
        Ok(())
    }

    async fn list(&self, flow_id: &str) -> Result<Vec<FabricRecord>> {
        let mut conn = self.conn.clone();
        let key = Self::key(flow_id);
        let blobs: Vec<Vec<u8>> = conn.lrange(&key, 0, -1).await?;
        let mut out = Vec::with_capacity(blobs.len());
        for b in blobs {
            match serde_json::from_slice::<FabricRecord>(&b) {
                Ok(r) => out.push(r),
                Err(e) => tracing::warn!(err = %e, "fabric: dropped malformed record"),
            }
        }
        Ok(out)
    }

    async fn evict(&self, flow_id: &str) -> Result<usize> {
        let mut conn = self.conn.clone();
        let key = Self::key(flow_id);
        let n: i64 = conn.llen(&key).await.unwrap_or(0);
        let _: i64 = conn.del(&key).await.unwrap_or(0);
        Ok(n as usize)
    }

    async fn bytes_stored(&self) -> u64 {
        // Valkey doesn't cheaply report per-namespace byte count; return 0
        // and let Prometheus scrape Valkey's own info stats for this.
        0
    }
}
