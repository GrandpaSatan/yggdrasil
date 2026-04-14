//! Pluggable health check framework.
//!
//! Services register `HealthProbe` implementations for each backend they depend
//! on (PostgreSQL, Qdrant, Ollama, etc.). The `HealthChecker` runs all probes
//! concurrently and returns a structured response.

use axum::{Json, response::IntoResponse};
use serde_json::json;
use std::sync::Arc;

/// A single backend health probe.
///
/// Implement this for each dependency a service needs to check.
#[async_trait::async_trait]
pub trait HealthProbe: Send + Sync {
    /// Human-readable name for this probe (e.g. "postgresql", "qdrant").
    fn name(&self) -> &str;

    /// Check if the backend is reachable. Return `Ok(())` on success or
    /// `Err(message)` describing the failure.
    async fn check(&self) -> Result<(), String>;
}

/// Runs all registered health probes and produces a JSON response.
#[derive(Clone)]
pub struct HealthChecker {
    probes: Arc<Vec<Box<dyn HealthProbe>>>,
}

impl HealthChecker {
    /// Create a new `HealthChecker` with the given probes.
    pub fn new(probes: Vec<Box<dyn HealthProbe>>) -> Self {
        Self {
            probes: Arc::new(probes),
        }
    }

    /// Axum handler that runs all probes and returns the health status.
    ///
    /// Response: `{ "status": "healthy"|"degraded", "checks": { "name": "ok"|"error: ..." } }`
    pub async fn handler(checker: Arc<Self>) -> impl IntoResponse {
        let mut checks = serde_json::Map::new();
        let mut all_ok = true;

        // Run probes concurrently.
        let mut handles = Vec::with_capacity(checker.probes.len());
        for probe in checker.probes.iter() {
            let name = probe.name().to_string();
            // SAFETY: probes are Send + Sync + 'static via trait bounds.
            // We can't move them into a task because they're behind Arc, so we
            // call check() and join below.
            handles.push((name, probe.check()));
        }

        for (name, fut) in handles {
            match fut.await {
                Ok(()) => {
                    checks.insert(name, json!("ok"));
                }
                Err(msg) => {
                    checks.insert(name, json!(format!("error: {msg}")));
                    all_ok = false;
                }
            }
        }

        let status = if all_ok { "healthy" } else { "degraded" };

        Json(json!({
            "status": status,
            "checks": checks,
        }))
    }
}

// ── Built-in probes ──────────────────────────────────────────────────────────

/// PostgreSQL connectivity probe: runs `SELECT 1`.
pub struct PgProbe {
    pool: sqlx::PgPool,
}

impl PgProbe {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl HealthProbe for PgProbe {
    fn name(&self) -> &str {
        "postgresql"
    }

    async fn check(&self) -> Result<(), String> {
        sqlx::query("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Qdrant connectivity probe: checks that the client can list collections.
pub struct QdrantProbe {
    vectors: ygg_store::qdrant::VectorStore,
    collection: String,
}

impl QdrantProbe {
    pub fn new(vectors: ygg_store::qdrant::VectorStore, collection: impl Into<String>) -> Self {
        Self {
            vectors,
            collection: collection.into(),
        }
    }
}

#[async_trait::async_trait]
impl HealthProbe for QdrantProbe {
    fn name(&self) -> &str {
        "qdrant"
    }

    async fn check(&self) -> Result<(), String> {
        self.vectors
            .ensure_collection(&self.collection)
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::response::IntoResponse;

    /// Probe whose result is decided up-front by the test.
    struct FakeProbe {
        n: &'static str,
        result: Result<(), String>,
    }

    #[async_trait::async_trait]
    impl HealthProbe for FakeProbe {
        fn name(&self) -> &str {
            self.n
        }
        async fn check(&self) -> Result<(), String> {
            self.result.clone()
        }
    }

    async fn run(checker: HealthChecker) -> serde_json::Value {
        let resp = HealthChecker::handler(Arc::new(checker)).await.into_response();
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn empty_checker_is_healthy() {
        let v = run(HealthChecker::new(vec![])).await;
        assert_eq!(v["status"], "healthy");
        assert!(v["checks"].as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn all_ok_probes_yield_healthy() {
        let v = run(HealthChecker::new(vec![
            Box::new(FakeProbe { n: "a", result: Ok(()) }),
            Box::new(FakeProbe { n: "b", result: Ok(()) }),
        ]))
        .await;
        assert_eq!(v["status"], "healthy");
        assert_eq!(v["checks"]["a"], "ok");
        assert_eq!(v["checks"]["b"], "ok");
    }

    #[tokio::test]
    async fn any_error_yields_degraded_with_per_probe_detail() {
        let v = run(HealthChecker::new(vec![
            Box::new(FakeProbe { n: "ok-one", result: Ok(()) }),
            Box::new(FakeProbe {
                n: "broken",
                result: Err("connection refused".into()),
            }),
        ]))
        .await;
        assert_eq!(v["status"], "degraded");
        assert_eq!(v["checks"]["ok-one"], "ok");
        assert!(v["checks"]["broken"].as_str().unwrap().contains("connection refused"));
    }

    #[tokio::test]
    async fn all_failing_still_yields_response_not_panic() {
        let v = run(HealthChecker::new(vec![
            Box::new(FakeProbe { n: "x", result: Err("e1".into()) }),
            Box::new(FakeProbe { n: "y", result: Err("e2".into()) }),
        ]))
        .await;
        assert_eq!(v["status"], "degraded");
        assert!(v["checks"]["x"].as_str().unwrap().contains("e1"));
        assert!(v["checks"]["y"].as_str().unwrap().contains("e2"));
    }

    #[tokio::test]
    async fn probe_name_is_used_as_check_key() {
        let v = run(HealthChecker::new(vec![Box::new(FakeProbe {
            n: "custom-name",
            result: Ok(()),
        })]))
        .await;
        assert!(v["checks"].get("custom-name").is_some());
    }
}
