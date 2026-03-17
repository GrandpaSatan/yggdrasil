mod config;
mod gpu_pool;
mod orchestrator;
mod proxmox_ext;

use std::path::PathBuf;
use std::process;

use clap::Parser;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser)]
#[command(name = "ygg-gaming", version, about = "GPU-pooled cloud gaming orchestrator")]
struct Cli {
    /// Path to the gaming config JSON file.
    #[arg(
        short,
        long,
        default_value = "configs/gaming/config.json",
        env = "YGG_GAMING_CONFIG"
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Launch a game VM (wake Thor if needed, assign GPU, start VM).
    Launch {
        /// VM name as defined in config (e.g., "harpy", "morrigan").
        vm_name: String,
    },
    /// Stop a game VM and release its GPU.
    Stop {
        /// VM name as defined in config.
        vm_name: String,
    },
    /// Show status of all VMs and GPUs.
    Status,
    /// List GPU pool with availability.
    ListGpus,
    /// Pair a Moonlight client with a VM's Sunshine (enters PIN via SSH).
    Pair {
        /// VM name as defined in config.
        vm_name: String,
        /// 4-digit PIN shown by Moonlight.
        pin: String,
    },
}

#[tokio::main]
async fn main() {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let cfg = match config::load_config(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load config {}: {e}", cli.config.display());
            process::exit(1);
        }
    };

    match cli.command {
        Command::Launch { vm_name } => match orchestrator::launch(&cfg, &vm_name).await {
            Ok(orchestrator::LaunchResult::Started {
                vm_name,
                gpu_name,
                ip,
            }) => {
                println!("✓ {vm_name} started with {gpu_name}");
                if let Some(ip) = ip {
                    println!("  Connect via Moonlight: {ip}");
                }
            }
            Ok(orchestrator::LaunchResult::AlreadyRunning { vm_name, ip }) => {
                println!("✓ {vm_name} is already running");
                if let Some(ip) = ip {
                    println!("  Connect via Moonlight: {ip}");
                }
            }
            Ok(orchestrator::LaunchResult::ServerOffline) => {
                eprintln!("✗ Thor did not wake up within timeout");
                process::exit(1);
            }
            Ok(orchestrator::LaunchResult::NoGpuAvailable { running_vms }) => {
                eprintln!("✗ No GPU available — all in use by: {}", running_vms.join(", "));
                process::exit(1);
            }
            Err(e) => {
                eprintln!("✗ Launch failed: {e}");
                process::exit(1);
            }
        },

        Command::Stop { vm_name } => match orchestrator::stop(&cfg, &vm_name).await {
            Ok(()) => println!("✓ {vm_name} stopped and GPU released"),
            Err(e) => {
                eprintln!("✗ Stop failed: {e}");
                process::exit(1);
            }
        },

        Command::Status => match orchestrator::status_all(&cfg).await {
            Ok(status) => {
                println!(
                    "Thor: {}",
                    if status.thor_online { "ONLINE" } else { "OFFLINE" }
                );
                println!();
                if status.vms.is_empty() {
                    println!("  (no VM data — Thor may be offline)");
                } else {
                    println!("{:<12} {:>6} {:<10} {:<20} {}", "VM", "VMID", "Status", "GPU", "IP");
                    println!("{}", "-".repeat(65));
                    for vm in &status.vms {
                        println!(
                            "{:<12} {:>6} {:<10} {:<20} {}",
                            vm.name,
                            vm.vmid,
                            vm.status,
                            vm.gpu.as_deref().unwrap_or("-"),
                            vm.ip.as_deref().unwrap_or("-"),
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("✗ Status failed: {e}");
                process::exit(1);
            }
        },

        Command::Pair { vm_name, pin } => {
            let vm = cfg.vms.iter().find(|v| v.name == vm_name);
            let Some(vm) = vm else {
                eprintln!("✗ VM '{vm_name}' not found in config");
                process::exit(1);
            };
            let Some(ip) = &vm.ip else {
                eprintln!("✗ VM '{vm_name}' has no IP configured");
                process::exit(1);
            };
            let ssh_user = vm.ssh_user.as_deref().unwrap_or("yggdrasil");
            let creds = vm.sunshine_creds.as_deref().unwrap_or("user:changeme");
            let port = vm.sunshine_port;

            println!("Pairing {vm_name} at {ip} with PIN {pin}...");

            let output = std::process::Command::new("ssh")
                .args([
                    "-o", "StrictHostKeyChecking=accept-new",
                    "-o", "ConnectTimeout=5",
                    &format!("{ssh_user}@{ip}"),
                    &format!(
                        "curl -sk -u {creds} -X POST \"https://localhost:{port}/api/pin\" -H \"Content-Type: application/json\" -d '{{\"pin\": \"{pin}\"}}'"
                    ),
                ])
                .output();

            match output {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    if stdout.contains("\"status\":true") {
                        println!("✓ Pairing successful! Connect via Moonlight to {ip}");
                    } else {
                        eprintln!("✗ Pairing failed: {stdout}");
                        if !out.stderr.is_empty() {
                            eprintln!("  stderr: {}", String::from_utf8_lossy(&out.stderr));
                        }
                        process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("✗ SSH failed: {e}");
                    process::exit(1);
                }
            }
        }

        Command::ListGpus => {
            let client = ygg_energy::proxmox::ProxmoxClient::new(
                cfg.proxmox.url.clone(),
                cfg.proxmox.token.clone(),
            );
            match gpu_pool::gpu_status_all(&cfg.gpus, &client, &cfg.proxmox.node).await {
                Ok(statuses) => {
                    println!("{:<20} {:<16} {:>5} {:<8} {}", "GPU", "PCI Address", "IOMMU", "Vendor", "Status");
                    println!("{}", "-".repeat(70));
                    for s in &statuses {
                        let status = match &s.assigned_to {
                            Some((vmid, name)) => format!("→ {} ({})", name, vmid),
                            None => "FREE".to_string(),
                        };
                        println!(
                            "{:<20} {:<16} {:>5} {:<8} {}",
                            s.gpu.name, s.gpu.pci_address, s.gpu.iommu_group, s.gpu.vendor, status
                        );
                    }
                }
                Err(e) => {
                    eprintln!("✗ GPU list failed: {e}");
                    process::exit(1);
                }
            }
        }
    }
}
