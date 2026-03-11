//! Local MCP server binary for Yggdrasil.
//!
//! Serves only filesystem-dependent tools (sync_docs) over stdio transport.
//! Network-only tools (memory, generate, search, HA) are served by the
//! remote `ygg-mcp-remote` binary over Streamable HTTP.
//!
//! Usage:
//!   ygg-mcp-server [--config <path>]

use anyhow::{Context, Result};
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};
use ygg_domain::config::McpServerConfig;
use ygg_mcp::local_server::YggdrasilLocalServer;

/// Yggdrasil local MCP server — filesystem tools via stdio.
#[derive(Debug, Parser)]
#[command(name = "ygg-mcp-server", version, about)]
struct Args {
    /// Path to the YAML configuration file.
    #[arg(
        short,
        long,
        default_value = "configs/mcp-server/config.yaml",
        env = "YGG_MCP_CONFIG"
    )]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    // IMPORTANT: log to stderr only. stdout is the JSON-RPC channel.
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    info!(config = %args.config.display(), "loading local MCP server configuration");

    let raw = std::fs::read_to_string(&args.config).with_context(|| {
        format!(
            "failed to read config file: {}",
            args.config.display()
        )
    })?;

    let config: McpServerConfig =
        serde_yaml::from_str(&raw).context("failed to parse config YAML")?;

    info!(
        odin_url = %config.odin_url,
        timeout_secs = config.timeout_secs,
        "configuration loaded"
    );

    let server = YggdrasilLocalServer::from_config(&config);

    let (stdin, stdout) = stdio();

    info!("starting local MCP server on stdio transport");

    let running = server
        .serve((stdin, stdout))
        .await
        .context("MCP server failed during initialization handshake")?;

    info!("MCP server initialized, waiting for requests");

    running
        .waiting()
        .await
        .context("MCP server task panicked")?;

    info!("MCP server shut down cleanly");
    Ok(())
}
