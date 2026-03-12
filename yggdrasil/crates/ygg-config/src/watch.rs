//! Config file hot-reload via filesystem notifications.
//!
//! Watches a config file for changes and sends the new config through a channel
//! when a modification is detected. Debounces rapid filesystem events.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::de::DeserializeOwned;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::load_json;

/// Default debounce duration for config file changes.
const DEBOUNCE_MS: u64 = 500;

/// A config watcher that monitors a JSON config file and sends updates
/// through a `tokio::sync::watch` channel when the file changes.
pub struct ConfigWatcher<T> {
    /// The watch receiver — clone this to get updates in your service.
    pub rx: watch::Receiver<Arc<T>>,
    /// Keep alive — dropping this stops the watcher.
    _watcher: RecommendedWatcher,
}

impl<T: DeserializeOwned + Send + Sync + 'static> ConfigWatcher<T> {
    /// Start watching a config file for changes.
    ///
    /// Returns the watcher and an initial config value loaded from the file.
    /// The `watch::Receiver` will receive new values whenever the file is modified.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, crate::ConfigError> {
        let path = path.as_ref().to_path_buf();

        // Load initial config
        let initial: T = load_json(&path)?;
        let (tx, rx) = watch::channel(Arc::new(initial));

        let watch_path = path.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    if matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_)
                    ) {
                        // Debounce: small sleep to let writes finish
                        std::thread::sleep(Duration::from_millis(DEBOUNCE_MS));

                        match load_json::<T>(&watch_path) {
                            Ok(new_config) => {
                                info!(path = %watch_path.display(), "config reloaded");
                                if tx.send(Arc::new(new_config)).is_err() {
                                    warn!("config watch receivers dropped");
                                }
                            }
                            Err(e) => {
                                error!(
                                    path = %watch_path.display(),
                                    error = %e,
                                    "failed to reload config — keeping previous"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("config watch error: {e}");
                }
            }
        })
        .map_err(|e| crate::ConfigError::ReadFile {
            path: path.display().to_string(),
            source: std::io::Error::other(e.to_string()),
        })?;

        // Watch the parent directory (some editors write temp files then rename)
        let watch_dir = path.parent().unwrap_or(Path::new("."));
        watcher
            .watch(watch_dir, RecursiveMode::NonRecursive)
            .map_err(|e| crate::ConfigError::ReadFile {
                path: path.display().to_string(),
                source: std::io::Error::other(e.to_string()),
            })?;

        info!(path = %path.display(), "config watcher started");

        Ok(Self {
            rx,
            _watcher: watcher,
        })
    }

    /// Get a clone of the current config value.
    pub fn current(&self) -> Arc<T> {
        Arc::clone(&self.rx.borrow())
    }

    /// Get the watch receiver for subscribing to config changes.
    pub fn subscribe(&self) -> watch::Receiver<Arc<T>> {
        self.rx.clone()
    }
}

/// Convenience function: load a config and start watching it.
/// Returns `(current_config, receiver)`.
pub fn load_and_watch<T: DeserializeOwned + Send + Sync + 'static>(
    path: impl AsRef<Path>,
) -> Result<(Arc<T>, watch::Receiver<Arc<T>>), crate::ConfigError> {
    let watcher = ConfigWatcher::new(path)?;
    let current = watcher.current();
    let rx = watcher.subscribe();
    // Leak the watcher so it lives forever (typical for service configs)
    std::mem::forget(watcher);
    Ok((current, rx))
}
