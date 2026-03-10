//! MCP server binary for Yggdrasil.
//!
//! Communicates with MCP clients (Claude Code, VS Code, Cursor, etc.) over
//! JSON-RPC 2.0 on stdio. Tracing output is directed to **stderr** so it does
//! not corrupt the JSON-RPC channel on stdout.
//!
//! Usage:
//!   ygg-mcp-server [--config <path>]
//!
//! The default config path is `configs/mcp-server/config.yaml` relative to the
//! working directory.

use anyhow::{Context, Result};
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};
use ygg_domain::config::McpServerConfig;
use ygg_mcp::server::YggdrasilServer;

/// Yggdrasil MCP server — bridges IDE clients to the Yggdrasil AI backend.
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

    info!(config = %args.config.display(), "loading MCP server configuration");

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
        "configuration loaded"
    );

    let server = YggdrasilServer::from_config(&config);

    let (stdin, stdout) = stdio();

    info!("starting MCP server on stdio transport");

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
