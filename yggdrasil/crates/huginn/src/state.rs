use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;

use crate::indexer::Indexer;

/// Shared runtime state used during watch mode.
///
/// `indexer` holds all I/O handles (PostgreSQL pool, Qdrant client, embed client).
/// `pending` is the debounce map: file path -> instant of last observed event.
/// `shutdown` broadcasts a shutdown signal to all background tasks.
pub struct AppState {
    pub indexer: Arc<Indexer>,
    pub pending: DashMap<PathBuf, Instant>,
    pub shutdown: tokio::sync::watch::Sender<bool>,
}

impl AppState {
    pub fn new(indexer: Arc<Indexer>) -> (Self, tokio::sync::watch::Receiver<bool>) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let state = Self {
            indexer,
            pending: DashMap::new(),
            shutdown: tx,
        };
        (state, rx)
    }
}
