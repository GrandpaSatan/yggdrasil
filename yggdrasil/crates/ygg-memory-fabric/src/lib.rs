//! Yggdrasil Shared Memory Fabric — cross-architecture working memory
//! for the swarm. Sprint 069 Phase G.
//!
//! # Tiers
//!
//! * **L3 (Semantic)** — embedding-indexed text memory, flow-scoped.
//!   Universal across every model architecture. Ship-first tier.
//! * **L2 (Projected Activation)** — per-layer K+V projection MLPs
//!   trained offline on paired hidden states. Cross-architecture KV
//!   reuse. Added incrementally as projections land.
//! * **L1 (Direct KV)** — raw K/V tensors with trivial scaling for
//!   same-family pairs (LFM2 internal). Fastest path, opt-in per
//!   request via header.
//!
//! This crate ships L3 + the coordinator plumbing. L1/L2 paths hook
//! into the same HTTP surface when their offline training/patching
//! work completes (see docs/research/shared-memory-fabric.md).

pub mod config;
pub mod embed;
pub mod handlers;
pub mod metrics;
pub mod storage;
pub mod types;

use std::sync::Arc;

use axum::{routing::{get, post}, Router};

pub use config::FabricConfig;
pub use storage::FabricStore;

/// Shared application state threaded through every handler.
pub struct AppState {
    pub store: Arc<dyn FabricStore>,
    pub embedder: Arc<embed::Embedder>,
    pub cfg: FabricConfig,
}

/// Build the axum router with all fabric endpoints wired up.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .route("/metrics", get(handlers::metrics))
        .route("/fabric/publish", post(handlers::publish))
        .route("/fabric/query", post(handlers::query))
        .route("/fabric/done", post(handlers::done))
        .route("/fabric/flow/:flow_id/history", get(handlers::history))
        .with_state(state)
}
