//! Yggdrasil Installer — cross-platform installation and setup tool.
//!
//! Detects the host OS, downloads or builds binaries, configures services,
//! and installs systemd/launchd/Windows Service definitions.
//!
//! Usage:
//!   ygg-installer install [--services <list>] [--data-dir <path>]
//!   ygg-installer uninstall
//!   ygg-installer status

mod platform;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Debug, Parser)]
#[command(name = "ygg-installer", version, about = "Yggdrasil installer")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install Yggdrasil services on this machine.
    Install {
        /// Services to install (comma-separated). Default: all.
        #[arg(short, long, default_value = "odin,mimir,huginn,muninn,ygg-node,ygg-sentinel,ygg-voice,ygg-agent")]
        services: String,

        /// Data directory for configs and state.
        #[arg(short, long, default_value = "/opt/yggdrasil")]
        data_dir: String,

        /// Binary directory.
        #[arg(short, long, default_value = "/opt/yggdrasil/bin")]
        bin_dir: String,
    },

    /// Remove Yggdrasil services from this machine.
    Uninstall,

    /// Show status of installed Yggdrasil services.
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let os = platform::detect_os();

    info!(os = %os, "ygg-installer starting");

    match args.command {
        Command::Install {
            services,
            data_dir,
            bin_dir,
        } => {
            let svc_list: Vec<&str> = services.split(',').map(str::trim).collect();
            info!(services = ?svc_list, data_dir = %data_dir, "installing");

            // Create directories
            platform::ensure_dirs(&data_dir, &bin_dir)?;

            // Install each service
            for svc in &svc_list {
                info!(service = svc, "installing service");
                platform::install_service(svc, &bin_dir, &data_dir, &os)?;
            }

            info!("installation complete");
        }

        Command::Uninstall => {
            info!("uninstalling Yggdrasil services");
            platform::uninstall_all(&os)?;
            info!("uninstall complete");
        }

        Command::Status => {
            let statuses = platform::service_status(&os)?;
            for (name, status) in &statuses {
                println!("{:<20} {}", name, status);
            }
        }
    }

    Ok(())
}
