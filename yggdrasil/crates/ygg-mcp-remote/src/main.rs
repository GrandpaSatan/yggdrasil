//! Remote MCP server binary for Yggdrasil.
//!
//! Serves the 12 network-only MCP tools (memory, generate, search, HA, sprint
//! history) over Streamable HTTP transport. Designed to run as an always-on
//! systemd service on Munin (<munin-ip>:9093).
//!
//! Local file tools (sync_docs) are NOT included — those run in the local
//! `ygg-mcp-server` binary on the developer workstation via stdio.
//!
//! Usage:
//!   ygg-mcp-remote [--config <path>] [--bind <addr:port>]

mod config_api;
mod session_manager;

use anyhow::{Context, Result};
use axum::routing::{get, post};
use clap::Parser;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::LocalSessionManager,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};
use ygg_domain::config::McpServerConfig;
use ygg_mcp::server::YggdrasilServer;

use crate::session_manager::PersistentSessionManager;

/// Yggdrasil Remote MCP server — always-on HTTP endpoint for IDE clients.
#[derive(Debug, Parser)]
#[command(name = "ygg-mcp-remote", version, about)]
struct Args {
    /// Path to the JSON configuration file.
    #[arg(
        short,
        long,
        default_value = "configs/mcp-remote/config.json",
        env = "YGG_MCP_CONFIG"
    )]
    config: PathBuf,

    /// Address to bind the HTTP server to.
    #[arg(short, long, default_value = "0.0.0.0:9093", env = "YGG_MCP_BIND")]
    bind: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    info!(config = %args.config.display(), "loading MCP remote server configuration");

    let config: McpServerConfig =
        ygg_config::load_json(&args.config)
            .with_context(|| format!("failed to load config: {}", args.config.display()))?;

    info!(
        odin_url = %config.odin_url,
        muninn_url = ?config.muninn_url,
        timeout_secs = config.timeout_secs,
        bind = %args.bind,
        "configuration loaded"
    );

    let ct = CancellationToken::new();

    // --- Session persistence setup ---
    // If database_url is configured, use PG-backed session manager.
    // Otherwise, fall back to in-memory LocalSessionManager.
    let has_db = config.database_url.is_some();

    let router = if has_db {
        let db_url = config.database_url.as_ref().unwrap().clone();
        let store = ygg_store::Store::connect(&db_url)
            .await
            .context("failed to connect to PostgreSQL for session persistence")?;

        // Run migrations if path is configured
        if let Some(ref mig_path) = config.migrations_path {
            store
                .migrate(mig_path)
                .await
                .context("failed to run session migrations")?;
            info!(path = mig_path, "database migrations applied");
        }

        // Spawn session cleanup background task
        PersistentSessionManager::spawn_cleanup_task(store.clone(), ct.clone());

        let store_for_api = Arc::new(store.clone());
        let project_id = config.project.clone();
        let workspace_id = config.workspace_id.clone();
        let session_manager = Arc::new(PersistentSessionManager::new(store, project_id, workspace_id));

        let service: StreamableHttpService<YggdrasilServer, PersistentSessionManager> =
            StreamableHttpService::new(
                move || Ok(YggdrasilServer::from_config(&config)),
                session_manager,
                StreamableHttpServerConfig {
                    stateful_mode: true,
                    cancellation_token: ct.clone(),
                    ..Default::default()
                },
            );

        info!("session persistence enabled (PostgreSQL)");

        // Config sync REST API (requires PG)
        let config_routes = axum::Router::new()
            .route("/api/v1/version", get(config_api::get_version))
            .route("/api/v1/config/{file_type}", get(config_api::get_config))
            .route("/api/v1/config/{file_type}", post(config_api::push_config))
            .route("/api/v1/version/bump", post(config_api::bump_version))
            .with_state(store_for_api);

        axum::Router::new()
            .nest_service("/mcp", service)
            .merge(config_routes)
    } else {
        let service: StreamableHttpService<YggdrasilServer, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(YggdrasilServer::from_config(&config)),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig {
                    stateful_mode: true,
                    cancellation_token: ct.clone(),
                    ..Default::default()
                },
            );

        info!("session persistence disabled (in-memory only)");
        axum::Router::new().nest_service("/mcp", service)
    };

    let tcp_listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("failed to bind to {}", args.bind))?;

    info!(addr = %args.bind, "MCP remote server listening");

    // Graceful shutdown on SIGTERM/SIGINT
    let ct_shutdown = ct.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        info!("shutdown signal received");
        ct_shutdown.cancel();
    });

    axum::serve(tcp_listener, router)
        .with_graceful_shutdown(async move { ct.cancelled().await })
        .await
        .context("HTTP server error")?;

    info!("MCP remote server shut down cleanly");
    Ok(())
}
