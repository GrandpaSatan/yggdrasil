//! Yggdrasil Shared Memory Fabric service.
//!
//! Runs on Hugin :11450 by default. Endpoints:
//!   POST /fabric/publish    — append step record (L3)
//!   POST /fabric/query      — cosine-retrieve prior steps
//!   POST /fabric/done       — evict a flow
//!   GET  /fabric/flow/:id/history — full trace for debugging
//!   GET  /health, /metrics

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::{EnvFilter, fmt};
use ygg_memory_fabric::{
    build_router, config::FabricConfig, embed::Embedder, metrics, storage::{FabricStore, MemoryStore, ValkeyStore}, AppState,
};

#[tokio::main]
async fn main() -> Result<()> {
    fmt().with_env_filter(EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ygg_memory_fabric=debug")))
        .init();

    let cfg = FabricConfig::from_env();
    tracing::info!(bind = %cfg.bind_addr, valkey = %cfg.valkey_url, tei = %cfg.tei_url,
                   "ygg-memory-fabric starting");
    metrics::init();

    let store: Arc<dyn FabricStore> = if cfg.valkey_url.is_empty() {
        tracing::warn!("FABRIC_VALKEY_URL empty — using in-memory store (not durable)");
        MemoryStore::new()
    } else {
        match ValkeyStore::connect(&cfg.valkey_url).await {
            Ok(s) => {
                tracing::info!("connected to Valkey at {}", cfg.valkey_url);
                s
            }
            Err(e) => {
                tracing::error!(err = %e,
                    "Valkey unreachable; falling back to in-memory store. \
                     Records will NOT survive restart.");
                MemoryStore::new()
            }
        }
    };

    let embedder = Arc::new(Embedder::new(cfg.tei_url.clone(), cfg.embed_dim));

    let state = Arc::new(AppState { store, embedder, cfg: cfg.clone() });
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await
        .with_context(|| format!("bind {}", cfg.bind_addr))?;
    tracing::info!("listening on {}", cfg.bind_addr);
    axum::serve(listener, app).await?;
    Ok(())
}
