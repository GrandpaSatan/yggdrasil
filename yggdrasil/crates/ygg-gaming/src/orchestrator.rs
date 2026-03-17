use std::time::{Duration, Instant};

use tracing::{info, warn};
use ygg_energy::proxmox::ProxmoxClient;
use ygg_energy::wol;

use crate::config::GamingConfig;
use crate::gpu_pool;
use crate::proxmox_ext;

/// Result of a VM launch operation.
#[derive(Debug)]
pub enum LaunchResult {
    Started {
        vm_name: String,
        gpu_name: String,
        ip: Option<String>,
    },
    AlreadyRunning {
        vm_name: String,
        ip: Option<String>,
    },
    ServerOffline,
    NoGpuAvailable {
        running_vms: Vec<String>,
    },
}

/// Status of the entire system.
#[derive(Debug)]
pub struct SystemStatus {
    pub thor_online: bool,
    pub vms: Vec<VmStatusEntry>,
}

#[derive(Debug)]
pub struct VmStatusEntry {
    pub name: String,
    pub vmid: u32,
    pub status: String,
    pub gpu: Option<String>,
    pub ip: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("VM '{0}' not found in config")]
    VmNotFound(String),
    #[error("Proxmox error: {0}")]
    Proxmox(#[from] ygg_energy::proxmox::ProxmoxError),
    #[error("GPU pool error: {0}")]
    GpuPool(#[from] gpu_pool::GpuPoolError),
    #[error("WoL error: {0}")]
    Wol(#[from] wol::WolError),
    #[error("timeout waiting for VM to {action} (waited {secs}s)")]
    Timeout { action: String, secs: u64 },
    #[error("pairing failed: {0}")]
    Pairing(String),
}

fn make_client(config: &GamingConfig) -> ProxmoxClient {
    ProxmoxClient::new(config.proxmox.url.clone(), config.proxmox.token.clone())
}

/// Launch a game VM: WoL Thor if needed → assign GPU → start VM.
pub async fn launch(
    config: &GamingConfig,
    vm_name: &str,
) -> Result<LaunchResult, OrchestratorError> {
    let vm = config
        .vms
        .iter()
        .find(|v| v.name == vm_name)
        .ok_or_else(|| OrchestratorError::VmNotFound(vm_name.to_string()))?;

    let client = make_client(config);
    let node = &config.proxmox.node;

    // Step 1: Check if Thor is online, wake if needed
    let online = proxmox_ext::node_online(&client, node).await?;
    if !online {
        info!(mac = %config.thor_wol.mac, "Thor offline — sending Wake-on-LAN");
        wol::send_wol(&config.thor_wol.mac, &config.thor_wol.broadcast)?;

        let deadline = Instant::now()
            + Duration::from_secs(config.timeouts.wol_poll_secs);
        let interval = Duration::from_secs(config.timeouts.wol_poll_interval_secs);

        loop {
            tokio::time::sleep(interval).await;
            if proxmox_ext::node_online(&client, node).await.unwrap_or(false) {
                info!("Thor is now online");
                break;
            }
            if Instant::now() >= deadline {
                warn!("Thor did not come online within timeout");
                return Ok(LaunchResult::ServerOffline);
            }
        }
    }

    // Step 2: Check if VM is already running
    let vm_status = client.vm_status(node, vm.vmid).await?;
    if vm_status.status == "running" {
        let gpu = gpu_pool::gpu_assigned_to_vm(&config.gpus, &client, node, vm.vmid).await?;
        if gpu.is_some() {
            info!(vm = vm_name, "VM already running with GPU assigned");
            return Ok(LaunchResult::AlreadyRunning {
                vm_name: vm_name.to_string(),
                ip: vm.ip.clone(),
            });
        }
        warn!(vm = vm_name, "VM running but no GPU assigned");
    }

    // Step 3: Find an available GPU
    let gpu = gpu_pool::find_available_gpu(&config.gpus, &client, node, &vm.gpu_preference)
        .await?;

    let Some(gpu) = gpu else {
        // Collect names of running VMs for the error message
        let vms = proxmox_ext::list_vms(&client, node)
            .await?
            .iter()
            .filter(|v| v.status == "running")
            .map(|v| {
                v.name
                    .clone()
                    .unwrap_or_else(|| format!("VM {}", v.vmid))
            })
            .collect();

        return Ok(LaunchResult::NoGpuAvailable { running_vms: vms });
    };

    let gpu_name = gpu.name.clone();

    // Step 4: Assign GPU to VM (VM must be stopped for hostpci changes)
    if vm_status.status != "running" {
        let pci_value = format!("mapping={},pcie=1,x-vga=1", gpu.mapping_id);
        info!(vm = vm_name, gpu = %gpu_name, mapping = %gpu.mapping_id, slot = %vm.hostpci_slot, "assigning GPU");
        proxmox_ext::set_vm_config(
            &client,
            node,
            vm.vmid,
            &[(&vm.hostpci_slot, &pci_value)],
        )
        .await?;
    }

    // Step 5: Start VM
    if vm_status.status != "running" {
        info!(vm = vm_name, "starting VM");
        client.start_vm(node, vm.vmid).await?;
    }

    // Step 6: Poll until running
    let deadline = Instant::now()
        + Duration::from_secs(config.timeouts.vm_start_timeout_secs);
    let interval = Duration::from_secs(config.timeouts.vm_start_poll_interval_secs);

    loop {
        tokio::time::sleep(interval).await;
        let status = client.vm_status(node, vm.vmid).await?;
        if status.status == "running" {
            info!(vm = vm_name, "VM is now running");
            break;
        }
        if Instant::now() >= deadline {
            return Err(OrchestratorError::Timeout {
                action: "start".to_string(),
                secs: config.timeouts.vm_start_timeout_secs,
            });
        }
    }

    // Step 7: Auto-deploy pairing data if configured
    if let (Some(pairing_src), Some(ip), Some(ssh_user)) =
        (&config.pairing_source, &vm.ip, &vm.ssh_user)
    {
        info!(vm = vm_name, "deploying Sunshine pairing data");
        // Wait a bit for SSH to come up
        tokio::time::sleep(Duration::from_secs(10)).await;
        if let Err(e) = deploy_pairing(pairing_src, ssh_user, ip).await {
            warn!(vm = vm_name, error = %e, "failed to deploy pairing (non-fatal)");
        }
    }

    Ok(LaunchResult::Started {
        vm_name: vm_name.to_string(),
        gpu_name,
        ip: vm.ip.clone(),
    })
}

/// Deploy Sunshine pairing data to a VM via SSH/SCP.
/// Copies sunshine_state.json and credentials/ to ~/.config/sunshine/ on the VM,
/// then restarts the Sunshine service.
async fn deploy_pairing(
    source_dir: &str,
    ssh_user: &str,
    vm_ip: &str,
) -> Result<(), OrchestratorError> {
    let dest = format!("{ssh_user}@{vm_ip}");

    // rsync the pairing directory contents
    let status = tokio::process::Command::new("rsync")
        .args([
            "-az", "--timeout=10",
            &format!("{source_dir}/sunshine_state.json"),
            &format!("{dest}:.config/sunshine/sunshine_state.json"),
        ])
        .status()
        .await
        .map_err(|e| OrchestratorError::Pairing(format!("rsync state failed: {e}")))?;

    if !status.success() {
        return Err(OrchestratorError::Pairing("rsync state failed".to_string()));
    }

    let status = tokio::process::Command::new("rsync")
        .args([
            "-az", "--timeout=10",
            &format!("{source_dir}/credentials/"),
            &format!("{dest}:.config/sunshine/credentials/"),
        ])
        .status()
        .await
        .map_err(|e| OrchestratorError::Pairing(format!("rsync creds failed: {e}")))?;

    if !status.success() {
        return Err(OrchestratorError::Pairing("rsync creds failed".to_string()));
    }

    // Restart Sunshine to pick up new pairing data
    let _ = tokio::process::Command::new("ssh")
        .args([
            "-o", "ConnectTimeout=5",
            &dest,
            "systemctl --user restart sunshine.service",
        ])
        .status()
        .await;

    info!("Sunshine pairing data deployed to {vm_ip}");
    Ok(())
}

/// Stop a game VM and release its GPU.
pub async fn stop(
    config: &GamingConfig,
    vm_name: &str,
) -> Result<(), OrchestratorError> {
    let vm = config
        .vms
        .iter()
        .find(|v| v.name == vm_name)
        .ok_or_else(|| OrchestratorError::VmNotFound(vm_name.to_string()))?;

    let client = make_client(config);
    let node = &config.proxmox.node;

    // Check current status
    let vm_status = client.vm_status(node, vm.vmid).await?;
    if vm_status.status == "stopped" {
        info!(vm = vm_name, "VM already stopped");
        // Still try to release GPU in case it was left assigned
        let _ =
            proxmox_ext::delete_vm_config_keys(&client, node, vm.vmid, &[&vm.hostpci_slot]).await;
        return Ok(());
    }

    // Graceful shutdown
    info!(vm = vm_name, "shutting down VM");
    client.stop_vm(node, vm.vmid).await?;

    // Poll until stopped
    let deadline =
        Instant::now() + Duration::from_secs(config.timeouts.vm_stop_timeout_secs);
    let interval = Duration::from_secs(config.timeouts.vm_start_poll_interval_secs);

    loop {
        tokio::time::sleep(interval).await;
        let status = client.vm_status(node, vm.vmid).await?;
        if status.status == "stopped" {
            info!(vm = vm_name, "VM stopped");
            break;
        }
        if Instant::now() >= deadline {
            return Err(OrchestratorError::Timeout {
                action: "stop".to_string(),
                secs: config.timeouts.vm_stop_timeout_secs,
            });
        }
    }

    // Release GPU
    info!(vm = vm_name, slot = %vm.hostpci_slot, "releasing GPU assignment");
    proxmox_ext::delete_vm_config_keys(&client, node, vm.vmid, &[&vm.hostpci_slot]).await?;

    Ok(())
}

/// Get status of all configured VMs and their GPU assignments.
pub async fn status_all(config: &GamingConfig) -> Result<SystemStatus, OrchestratorError> {
    let client = make_client(config);
    let node = &config.proxmox.node;

    let thor_online = proxmox_ext::node_online(&client, node)
        .await
        .unwrap_or(false);

    let mut vms = Vec::new();

    if thor_online {
        for vm_entry in &config.vms {
            let status = match client.vm_status(node, vm_entry.vmid).await {
                Ok(s) => s.status,
                Err(_) => "unknown".to_string(),
            };

            let gpu =
                gpu_pool::gpu_assigned_to_vm(&config.gpus, &client, node, vm_entry.vmid)
                    .await
                    .ok()
                    .flatten()
                    .map(|g| g.name);

            vms.push(VmStatusEntry {
                name: vm_entry.name.clone(),
                vmid: vm_entry.vmid,
                status,
                gpu,
                ip: vm_entry.ip.clone(),
            });
        }
    }

    Ok(SystemStatus { thor_online, vms })
}
