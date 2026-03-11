//! Local MCP `ServerHandler` for Yggdrasil.
//!
//! `YggdrasilLocalServer` exposes only the tools that require local filesystem
//! access (currently just `sync_docs_tool`). It runs as a stdio server on the
//! developer workstation, while the network tools are served by
//! `YggdrasilServer` over Streamable HTTP.

use reqwest::Client;
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo, Implementation},
    tool, tool_handler, tool_router,
};
use std::time::Duration;
use uuid::Uuid;
use ygg_domain::config::McpServerConfig;

use crate::tools::{SyncDocsParams, sync_docs};

/// Local MCP server for filesystem tools only.
///
/// Runs as stdio transport per IDE window. Calls Odin over HTTP for LLM
/// generation during doc scaffolding, but does not expose any of the
/// network-only tools (those live in `YggdrasilServer` on the remote).
#[derive(Clone)]
pub struct YggdrasilLocalServer {
    client: Client,
    config: McpServerConfig,
    tool_router: ToolRouter<Self>,
    /// Session ID for generate calls within sync_docs.
    session_id: String,
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
        let result = sync_docs(&self.client, &self.config, params, Some(&self.session_id)).await;
        result
            .content
            .into_iter()
            .next()
            .and_then(|c| c.raw.as_text().map(|t| t.text.clone()))
            .unwrap_or_default()
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
        }
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
        .with_server_info(Implementation::new("yggdrasil-local", "0.1.0"))
    }
}
