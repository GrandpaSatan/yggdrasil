//! Remote MCP server binary for Yggdrasil.
//!
//! Serves the 12 network-only MCP tools (memory, generate, search, HA, sprint
//! history) over Streamable HTTP transport. Designed to run as an always-on
//! systemd service on Munin (REDACTED_MUNIN_IP:9093).
//!
//! Local file tools (sync_docs) are NOT included — those run in the local
//! `ygg-mcp-server` binary on the developer workstation via stdio.
//!
//! Usage:
//!   ygg-mcp-remote [--config <path>] [--bind <addr:port>]

use anyhow::{Context, Result};
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

/// Yggdrasil Remote MCP server — always-on HTTP endpoint for IDE clients.
#[derive(Debug, Parser)]
#[command(name = "ygg-mcp-remote", version, about)]
struct Args {
    /// Path to the YAML configuration file.
    #[arg(
        short,
        long,
        default_value = "configs/mcp-remote/config.yaml",
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

    let raw = std::fs::read_to_string(&args.config).with_context(|| {
        format!(
            "failed to read config file: {}",
            args.config.display()
        )
    })?;

    let mut config: McpServerConfig =
        serde_yaml::from_str(&raw).context("failed to parse config YAML")?;

    // Expand ${ENV_VAR} placeholders that serde_yaml does not resolve itself.
    if let Some(ref mut ha) = config.ha {
        if ha.token.starts_with("${") && ha.token.ends_with('}') {
            let var_name = &ha.token[2..ha.token.len() - 1];
            match std::env::var(var_name) {
                Ok(val) => ha.token = val,
                Err(_) => {
                    tracing::warn!(
                        "HA token placeholder '{}': env var {} is not set",
                        ha.token,
                        var_name
                    );
                }
            }
        }
    }

    info!(
        odin_url = %config.odin_url,
        muninn_url = ?config.muninn_url,
        timeout_secs = config.timeout_secs,
        bind = %args.bind,
        "configuration loaded"
    );

    let ct = CancellationToken::new();

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

    let router = axum::Router::new().nest_service("/mcp", service);

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
