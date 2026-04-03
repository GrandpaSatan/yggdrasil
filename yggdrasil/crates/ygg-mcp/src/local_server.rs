//! Local MCP `ServerHandler` for Yggdrasil.
//!
//! `YggdrasilLocalServer` exposes only the tools that require local filesystem
//! access or local hardware (`sync_docs_tool`, `screenshot_tool`). It runs as a
//! stdio server on the developer workstation, while the network tools are served
//! by `YggdrasilServer` over Streamable HTTP.

use futures::StreamExt;
use reqwest::Client;
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo, Implementation},
    tool, tool_handler, tool_router,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;
use ygg_domain::config::McpServerConfig;

use crate::tools::{ScreenshotParams, SyncDocsParams, screenshot, sync_docs};

// ---------------------------------------------------------------------------
// Browser lifecycle helpers
// ---------------------------------------------------------------------------

/// Launch a headless Chromium instance and spawn its CDP event-loop handler.
///
/// The handler must be driven by a background task; the event loop calls
/// `handler.next()` in a tight loop until the browser exits or the task is
/// dropped. Using `tokio::sync::OnceCell` guarantees the browser is launched
/// at most once per server instance, even under concurrent MCP calls.
async fn init_browser() -> Result<chromiumoxide::Browser, String> {
    use chromiumoxide::BrowserConfig;

    let config = BrowserConfig::builder()
        .no_sandbox()
        // Disable GPU rendering — not available in a headless server environment.
        .arg("--disable-gpu")
        // Avoid /dev/shm exhaustion in low-memory or Docker environments.
        .arg("--disable-dev-shm-usage")
        .build()
        .map_err(|e| format!("Browser config error: {e}"))?;

    let (browser, mut handler) = chromiumoxide::Browser::launch(config)
        .await
        .map_err(|e| format!("Failed to launch Chrome/Chromium: {e}"))?;

    // Spawn the CDP protocol handler as a background task.
    // The handler MUST be driven continuously; dropping it closes the browser.
    tokio::spawn(async move {
        while handler.next().await.is_some() {}
    });

    Ok(browser)
}

// ---------------------------------------------------------------------------
// Local server struct
// ---------------------------------------------------------------------------

/// Local MCP server for filesystem + browser tools.
///
/// Runs as stdio transport per IDE window. Exposes:
/// - `sync_docs_tool`: sprint lifecycle doc scaffolding (calls Odin for LLM generation)
/// - `screenshot_tool`: headless Chromium page capture for visual UI review
///
/// Network-only tools (memory, search, HA) live in `YggdrasilServer` on the remote.
#[derive(Clone)]
pub struct YggdrasilLocalServer {
    client: Client,
    config: McpServerConfig,
    tool_router: ToolRouter<Self>,
    /// Session ID for generate calls within sync_docs.
    session_id: String,
    /// Lazily initialised headless Chromium browser.
    ///
    /// `Arc<OnceCell<...>>` ensures the browser is launched at most once across
    /// all cloned server instances within a single process. The Clone bound on
    /// `YggdrasilLocalServer` (required by rmcp) clones the Arc, so all copies
    /// share the same OnceCell.
    browser: Arc<tokio::sync::OnceCell<chromiumoxide::Browser>>,
}

#[tool_router]
impl YggdrasilLocalServer {
    /// Sprint lifecycle documentation agent.
    ///
    /// On setup: initializes /docs/ and /sprints/ for a new workspace, cleans stale docs.
    /// On sprint_start: auto-runs setup if needed, updates USAGE.md, checks invariants.
    /// On sprint_end: archives sprint to Mimir, appends ARCHITECTURE.md delta, deletes sprint file.
    #[tool(description = "Sprint lifecycle doc agent. Supports three events:\n\
        event='setup': Initialize a new workspace — creates /docs/ and /sprints/, scaffolds \
        required docs (ARCHITECTURE.md, NAMING_CONVENTIONS.md, USAGE.md), cleans stale files. \
        Pass sprint_content as project description for context-aware scaffolding.\n\
        event='sprint_start': Updates USAGE.md via LLM, checks /docs/ + /sprints/ invariants. \
        Auto-runs setup first if /docs/ doesn't exist.\n\
        event='sprint_end': Archives sprint to Mimir, updates ARCHITECTURE.md, deletes sprint file.\n\
        workspace_path: Pass the current project root to override the config default. \
        Resolution order: workspace_path param → config.workspace_path.")]
    async fn sync_docs_tool(
        &self,
        Parameters(params): Parameters<SyncDocsParams>,
    ) -> String {
        let start = Instant::now();
        let result = sync_docs(&self.client, &self.config, params, Some(&self.session_id)).await;
        let text = result
            .content
            .into_iter()
            .next()
            .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        let is_error = text.starts_with("Error:");
        self.emit_event("tool", serde_json::json!({
            "name": "sync_docs",
            "status": if is_error { "error" } else { "ok" },
            "duration_ms": start.elapsed().as_millis() as u64,
        }));
        text
    }

    /// Capture a screenshot of a web page via headless Chromium.
    ///
    /// The browser is launched lazily on the first call and reused for all
    /// subsequent calls within the same MCP server session. Each call opens a
    /// new tab, applies viewport settings, navigates, and saves a PNG to disk.
    #[tool(description = "Capture a screenshot of a web page via headless Chromium. \
        Returns the file path to the saved PNG image. Use the Read tool to view it.\n\
        \n\
        Parameters:\n\
        - url: the page to capture (e.g. \"http://localhost:3000/dashboard\")\n\
        - selector (optional): CSS selector to wait for before capturing — useful for \
        SPAs that render asynchronously. Times out after 10 seconds if not found.\n\
        - full_page (optional, default false): capture the full scrollable page height \
        instead of just the visible viewport.\n\
        - viewport_width (optional, default 1280): viewport width in pixels.\n\
        - viewport_height (optional, default 720): viewport height in pixels.\n\
        \n\
        Screenshots are saved to /tmp/ygg-screenshots/ with a URL-slug + timestamp filename.\n\
        After calling this tool, use the Read tool on the returned path to view the image.")]
    async fn screenshot_tool(
        &self,
        Parameters(params): Parameters<ScreenshotParams>,
    ) -> String {
        let start = Instant::now();
        let browser = match self
            .browser
            .get_or_try_init(init_browser)
            .await
        {
            Ok(b) => b,
            Err(e) => {
                self.emit_event("tool", serde_json::json!({
                    "name": "screenshot",
                    "status": "error",
                    "error": e.to_string(),
                    "duration_ms": start.elapsed().as_millis() as u64,
                }));
                return format!(
                    "Error: {e}\n\n\
                     Ensure Chrome or Chromium is installed:\n\
                     - Ubuntu/Debian: sudo apt install chromium-browser\n\
                     - Or install Google Chrome from https://www.google.com/chrome/"
                );
            }
        };

        let result = screenshot(browser, params).await;
        let text = result
            .content
            .into_iter()
            .next()
            .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        let is_error = text.starts_with("Error:");
        self.emit_event("tool", serde_json::json!({
            "name": "screenshot",
            "status": if is_error { "error" } else { "ok" },
            "duration_ms": start.elapsed().as_millis() as u64,
        }));
        text
    }

}

impl YggdrasilLocalServer {
    /// Construct the local server from a config.
    ///
    /// Only needs `odin_url` (for LLM generation in sync_docs scaffolding),
    /// `workspace_path`, `project`, and `timeout_secs`. HA config is ignored.
    pub fn from_config(config: &McpServerConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_else(|_| Client::new());

        let session_id = Uuid::new_v4().to_string();

        Self {
            client,
            config: config.clone(),
            tool_router: Self::tool_router(),
            session_id,
            browser: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    /// Emit a JSONL event for the VS Code extension's status bar and dashboard.
    ///
    /// Writes to `config.events_file` if set. Fire-and-forget — never fails
    /// the calling tool, never blocks on I/O errors.
    fn emit_event(&self, event: &str, data: serde_json::Value) {
        let Some(ref path) = self.config.events_file else {
            return;
        };
        let line = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "event": event,
            "data": data,
        });
        // Best-effort append — never fail
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "{}", line)
            });
    }
}

#[tool_handler]
impl ServerHandler for YggdrasilLocalServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new("yggdrasil-local", env!("CARGO_PKG_VERSION")))
    }
}
