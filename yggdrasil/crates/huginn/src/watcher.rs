use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use notify::{EventKind, RecursiveMode, Watcher};
use tracing::{debug, info, warn};
use ygg_domain::chunk::Language;

use crate::error::HuginnError;
use crate::indexer::{Indexer, IGNORE_DIRS};

/// File watcher with debounced re-indexing.
///
/// Uses a `DashMap<PathBuf, Instant>` as a debounce buffer. A background tick
/// task drains entries older than `debounce_ms` milliseconds and triggers
/// `Indexer::index_file()` for each.
pub struct FileWatcher {
    indexer: Arc<Indexer>,
    debounce_ms: u64,
}

impl FileWatcher {
    pub fn new(indexer: Arc<Indexer>, debounce_ms: u64) -> Self {
        Self {
            indexer,
            debounce_ms,
        }
    }

    /// Start watching `paths` recursively. Blocks until a shutdown signal is received
    /// (SIGINT / SIGTERM via `tokio::signal::ctrl_c`).
    pub async fn run(&self, paths: &[String]) -> Result<(), HuginnError> {
        let pending: Arc<DashMap<PathBuf, Instant>> = Arc::new(DashMap::new());
        let debounce = Duration::from_millis(self.debounce_ms);

        // Channel between the notify callback thread and the async event processor.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<notify::Event>(256);

        // Spawn the notify watcher on the blocking thread pool so the callback
        // (which is !Send) never runs on a Tokio async thread.
        let tx_clone = tx.clone();
        let paths_owned: Vec<String> = paths.to_vec();

        // We keep the watcher alive by holding it in a background blocking task.
        // The task blocks until the watcher is dropped (which happens when we shut down).
        let (watcher_tx, watcher_rx) = std::sync::mpsc::channel::<()>();

        let watcher_thread = std::thread::spawn(move || {
            let event_tx = tx_clone;
            let mut watcher = match notify::recommended_watcher(move |result: Result<notify::Event, notify::Error>| {
                match result {
                    Ok(event) => {
                        if event_tx.try_send(event).is_err() {
                            // Channel full — drop the event; debounce handles rapid events.
                        }
                    }
                    Err(e) => {
                        // Cannot use tracing here (on a blocking thread without a subscriber).
                        eprintln!("notify watcher error: {e}");
                    }
                }
            }) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("failed to create file watcher: {e}");
                    return;
                }
            };

            for path in &paths_owned {
                if let Err(e) = watcher.watch(Path::new(path), RecursiveMode::Recursive) {
                    eprintln!("watcher: failed to watch {path}: {e}");
                }
            }

            info!(paths = ?paths_owned, "file watcher active");

            // Block until shutdown signal arrives.
            let _ = watcher_rx.recv();
            info!("watcher thread shutting down");
        });

        let pending_clone = Arc::clone(&pending);

        // Background task: receive notify events and populate the pending map.
        let event_task = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let relevant = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                );
                if !relevant {
                    continue;
                }

                for path in event.paths {
                    if should_ignore(&path) {
                        continue;
                    }
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    let lang = Language::from_extension(ext);
                    if matches!(lang, Language::Unknown) {
                        continue;
                    }
                    debug!(path = ?path, kind = ?event.kind, "queued for re-index");
                    pending_clone.insert(path, Instant::now());
                }
            }
        });

        let indexer_tick = Arc::clone(&self.indexer);
        let pending_tick = Arc::clone(&pending);
        let repo_root = self
            .indexer
            .config
            .watch_paths
            .first()
            .cloned()
            .unwrap_or_default();

        // Background task: drain the pending map on each tick.
        let tick_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                interval.tick().await;

                let due: Vec<PathBuf> = pending_tick
                    .iter()
                    .filter(|entry| entry.value().elapsed() >= debounce)
                    .map(|entry| entry.key().clone())
                    .collect();

                for path in due {
                    pending_tick.remove(&path);

                    if path.exists() {
                        match indexer_tick.index_file(&path, &repo_root, false).await {
                            Ok(Some(n)) => info!(path = ?path, chunks = n, "re-indexed"),
                            Ok(None) => debug!(path = ?path, "hash unchanged — no re-index needed"),
                            Err(e) => warn!(path = ?path, error = %e, "re-index failed"),
                        }
                    } else {
                        let path_str = path.to_string_lossy().to_string();
                        match indexer_tick.remove_file(&path_str).await {
                            Ok(()) => info!(path = %path_str, "removed deleted file from index"),
                            Err(e) => warn!(path = %path_str, error = %e, "failed to remove deleted file"),
                        }
                    }
                }
            }
        });

        // Wait for SIGINT/SIGTERM.
        tokio::signal::ctrl_c()
            .await
            .map_err(|e| HuginnError::Watch(format!("signal handler error: {e}")))?;

        info!("shutdown signal received — stopping watcher");

        // Signal the watcher thread to stop and wait for it.
        let _ = watcher_tx.send(());
        let _ = watcher_thread.join();

        event_task.abort();
        tick_task.abort();

        Ok(())
    }
}

/// Returns true if the path contains an ignored directory component.
fn should_ignore(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        IGNORE_DIRS.iter().any(|d| s == *d)
    })
}
