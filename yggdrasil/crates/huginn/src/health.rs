/// Huginn health and metrics HTTP listener.
///
/// Huginn is a background daemon with no existing HTTP interface. This module
/// adds a minimal Axum server on a configurable port (default 0.0.0.0:9092)
/// exposing two endpoints:
///
///   GET /health  — JSON health payload with indexing statistics
///   GET /metrics — Prometheus text exposition format
///
/// The server runs as a separate Tokio task on the same runtime as the file
/// watcher and indexer. It shares read-only access to atomic counters updated
/// by the indexer and watcher.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use chrono::{DateTime, Utc};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::Serialize;
use tokio::sync::RwLock;

// ─────────────────────────────────────────────────────────────────────────────
// Shared state
// ─────────────────────────────────────────────────────────────────────────────

/// Shared state between the health HTTP server and the indexer/watcher tasks.
///
/// All counters use `AtomicU64` for lock-free reads from the HTTP handler
/// while the indexer updates them from its own task.
pub struct HealthState {
    /// Handle used by the `/metrics` endpoint to render Prometheus exposition.
    pub prometheus: PrometheusHandle,
    /// Number of watch paths currently monitored by the file watcher.
    pub watch_count: AtomicU64,
    /// Total number of files that have been indexed since startup.
    pub indexed_files: AtomicU64,
    /// Total code chunks stored across all indexed files.
    pub code_chunks: AtomicU64,
    /// Wall-clock time of the most recent successful file index operation.
    pub last_index_at: RwLock<Option<DateTime<Utc>>>,
}

impl HealthState {
    /// Create a new `HealthState` with all counters at zero.
    pub fn new(prometheus: PrometheusHandle) -> Self {
        Self {
            prometheus,
            watch_count: AtomicU64::new(0),
            indexed_files: AtomicU64::new(0),
            code_chunks: AtomicU64::new(0),
            last_index_at: RwLock::new(None),
        }
    }

    /// Increment the indexed-files counter and update the last-index timestamp.
    pub async fn record_file_indexed(&self, chunks: u64) {
        self.indexed_files.fetch_add(1, Ordering::Relaxed);
        self.code_chunks.fetch_add(chunks, Ordering::Relaxed);
        *self.last_index_at.write().await = Some(Utc::now());

        // Keep Prometheus gauges in sync with the atomics.
        metrics::gauge!("ygg_indexed_files_total")
            .set(self.indexed_files.load(Ordering::Relaxed) as f64);
        metrics::gauge!("ygg_code_chunks_total")
            .set(self.code_chunks.load(Ordering::Relaxed) as f64);
    }

    /// Record a file watcher event.
    ///
    /// `event_kind` is one of "modify", "create", or "remove".
    pub fn record_watcher_event(&self, event_kind: &str) {
        metrics::counter!("ygg_watcher_events_total", "event" => event_kind.to_string())
            .increment(1);
    }

    /// Set the number of actively watched paths.
    pub fn set_watch_count(&self, n: u64) {
        self.watch_count.store(n, Ordering::Relaxed);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HTTP handlers
// ─────────────────────────────────────────────────────────────────────────────

/// Response body for `GET /health`.
#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    watching: u64,
    indexed_files: u64,
    last_index_at: Option<DateTime<Utc>>,
}

/// `GET /health` — returns indexing statistics as JSON.
async fn health_handler(State(state): State<Arc<HealthState>>) -> impl IntoResponse {
    let last_index_at = *state.last_index_at.read().await;
    let body = HealthResponse {
        status: "ok",
        watching: state.watch_count.load(Ordering::Relaxed),
        indexed_files: state.indexed_files.load(Ordering::Relaxed),
        last_index_at,
    };
    (StatusCode::OK, Json(body))
}

/// `GET /metrics` — returns Prometheus text exposition format.
async fn metrics_handler(State(state): State<Arc<HealthState>>) -> impl IntoResponse {
    let body = state.prometheus.render();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Server lifecycle
// ─────────────────────────────────────────────────────────────────────────────

/// Start the health HTTP server on `listen_addr`.
///
/// This function binds the TCP listener and drives the Axum server to
/// completion. It is intended to be spawned as a Tokio task:
///
/// ```ignore
/// tokio::spawn(start_health_server(config.listen_addr.clone(), state.clone()));
/// ```
///
/// The server runs until the process exits or the task is cancelled. It does
/// not participate in the graceful-shutdown watch channel because it holds no
/// mutable state and its termination is harmless.
pub async fn start_health_server(
    listen_addr: String,
    state: Arc<HealthState>,
) -> anyhow::Result<()> {
    let router = Router::new()
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .map_err(|e| anyhow::anyhow!("huginn health server failed to bind {listen_addr}: {e}"))?;

    tracing::info!(addr = %listen_addr, "huginn health server listening");

    axum::serve(listener, router)
        .await
        .map_err(|e| anyhow::anyhow!("huginn health server error: {e}"))?;

    Ok(())
}
