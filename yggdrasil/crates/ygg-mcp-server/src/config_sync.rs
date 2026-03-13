//! Startup config synchronization with the remote MCP server.
//!
//! On startup, the local MCP server:
//! 1. Checks its version against the remote
//! 2. Pulls updated config files if the remote version is newer
//! 3. Pushes locally-changed config files to the remote
//!
//! All errors are non-fatal — sync failures are logged and the server
//! continues to start normally.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncState {
    pub config_version: String,
    pub last_sync: String,
    pub files: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct VersionResponse {
    pub server_version: String,
    pub client_latest: String,
    pub config_version: String,
}

#[derive(Debug, Deserialize)]
struct ConfigResponse {
    pub content: String,
    pub content_hash: String,
}

/// Known global file types that are always synced.
const GLOBAL_FILE_TYPES: &[&str] = &["global_settings", "global_claude_md"];

/// Project-scoped file types, synced when workspace_path is set.
const PROJECT_FILE_TYPES: &[&str] = &["project_settings", "project_claude_md"];

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub async fn run_startup_sync(
    remote_url: &str,
    workspace_path: Option<&str>,
    project: Option<&str>,
) -> Result<()> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    // 1. Fetch remote version info
    let version_resp = match fetch_version(&client, remote_url).await {
        Some(v) => v,
        None => return Ok(()), // Non-fatal: remote unreachable
    };

    // 2. Warn if client is behind
    let current = env!("CARGO_PKG_VERSION");
    if current != version_resp.client_latest {
        warn!(
            current = current,
            latest = %version_resp.client_latest,
            "client version is behind remote — consider updating"
        );
    }

    // 3. Load local sync state
    let mut state = load_sync_state();

    // 4. Determine which file types to sync
    let mut file_types: Vec<&str> = GLOBAL_FILE_TYPES.to_vec();
    if workspace_path.is_some() {
        file_types.extend_from_slice(PROJECT_FILE_TYPES);
    }

    // 5. Pull from remote if config_version is newer
    if version_resp.config_version != state.config_version {
        info!(
            remote = %version_resp.config_version,
            local = %state.config_version,
            "remote config version differs — pulling updates"
        );

        for ft in &file_types {
            let path = match file_type_to_path(ft, workspace_path) {
                Some(p) => p,
                None => continue,
            };

            if let Some(remote_cfg) = fetch_config(&client, remote_url, ft, project).await {
                // Only write if content actually differs from local
                let local_hash = hash_file(&path).unwrap_or_default();
                if local_hash == remote_cfg.content_hash {
                    // Update state tracking even if file matches
                    state.files.insert(ft.to_string(), remote_cfg.content_hash);
                    continue;
                }

                // Create .bak backup
                if path.exists() {
                    let bak = path.with_extension("bak");
                    if let Err(e) = fs::copy(&path, &bak) {
                        warn!(path = %path.display(), error = %e, "failed to create backup");
                    }
                }

                // Ensure parent dir exists
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }

                match fs::write(&path, &remote_cfg.content) {
                    Ok(()) => {
                        info!(file_type = ft, path = %path.display(), "pulled config from remote");
                        state
                            .files
                            .insert(ft.to_string(), remote_cfg.content_hash);
                    }
                    Err(e) => {
                        warn!(file_type = ft, error = %e, "failed to write pulled config");
                    }
                }
            }
        }

        state.config_version = version_resp.config_version;
    }

    // 6. Push locally-changed files to remote
    let hostname = gethostname();
    for ft in &file_types {
        let path = match file_type_to_path(ft, workspace_path) {
            Some(p) => p,
            None => continue,
        };

        let current_hash = match hash_file(&path) {
            Some(h) => h,
            None => continue, // File doesn't exist locally
        };

        let tracked_hash = state.files.get(*ft).cloned().unwrap_or_default();
        if current_hash == tracked_hash {
            continue; // No local changes
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let body = serde_json::json!({
            "project_id": project,
            "content": content,
            "workstation_id": hostname,
        });

        match client
            .post(format!("{}/api/v1/config/{}", remote_url, ft))
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                info!(file_type = ft, "pushed local config changes to remote");
                state.files.insert(ft.to_string(), current_hash);
            }
            Ok(resp) => {
                warn!(file_type = ft, status = %resp.status(), "failed to push config");
            }
            Err(e) => {
                warn!(file_type = ft, error = %e, "failed to push config");
            }
        }
    }

    // 7. Save updated sync state
    if let Err(e) = save_sync_state(&state) {
        warn!(error = %e, "failed to save sync state");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

async fn fetch_version(client: &Client, remote_url: &str) -> Option<VersionResponse> {
    let resp = client
        .get(format!("{}/api/v1/version", remote_url))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => match r.json::<VersionResponse>().await {
            Ok(v) => Some(v),
            Err(e) => {
                warn!(error = %e, "failed to parse version response");
                None
            }
        },
        Ok(r) => {
            warn!(status = %r.status(), "version endpoint returned error");
            None
        }
        Err(e) => {
            warn!(error = %e, "remote server unreachable for version check");
            None
        }
    }
}

async fn fetch_config(
    client: &Client,
    remote_url: &str,
    file_type: &str,
    project: Option<&str>,
) -> Option<ConfigResponse> {
    let mut url = format!("{}/api/v1/config/{}", remote_url, file_type);
    if let Some(pid) = project {
        url.push_str(&format!("?project_id={}", pid));
    }

    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r.json::<ConfigResponse>().await.ok(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn sync_state_path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("yggdrasil")
        .join("sync-state.json")
}

fn load_sync_state() -> SyncState {
    let path = sync_state_path();
    if let Ok(contents) = fs::read_to_string(&path) {
        if let Ok(state) = serde_json::from_str(&contents) {
            return state;
        }
    }
    SyncState {
        config_version: "0.0.0".to_string(),
        last_sync: String::new(),
        files: HashMap::new(),
    }
}

fn save_sync_state(state: &SyncState) -> Result<()> {
    let path = sync_state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state)?;
    fs::write(&path, json)?;
    Ok(())
}

fn file_type_to_path(file_type: &str, workspace: Option<&str>) -> Option<PathBuf> {
    let home = home_dir();
    match file_type {
        "global_settings" => Some(home.join(".claude").join("settings.json")),
        "global_claude_md" => Some(home.join(".claude").join("CLAUDE.md")),
        "project_settings" => workspace.map(|w| {
            PathBuf::from(w)
                .join(".claude")
                .join("settings.local.json")
        }),
        "project_claude_md" => workspace.map(|w| PathBuf::from(w).join("CLAUDE.md")),
        _ => None,
    }
}

fn hash_file(path: &Path) -> Option<String> {
    let contents = fs::read(path).ok()?;
    Some(format!("{:x}", Sha256::digest(&contents)))
}

fn gethostname() -> String {
    fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "unknown".to_string())
        .trim()
        .to_string()
}
