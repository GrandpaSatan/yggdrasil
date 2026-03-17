use ygg_energy::proxmox::ProxmoxClient;

use crate::config::GpuEntry;
use crate::proxmox_ext;

/// Status of a single GPU in the pool.
#[derive(Debug)]
pub struct GpuStatus {
    pub gpu: GpuEntry,
    pub assigned_to: Option<(u32, String)>, // (vmid, vm_name)
}

/// Find the first available GPU, preferring the given vendor.
/// Queries all running VMs and checks their hostpci* config entries.
pub async fn find_available_gpu(
    gpus: &[GpuEntry],
    client: &ProxmoxClient,
    node: &str,
    preference: &str,
) -> Result<Option<GpuEntry>, GpuPoolError> {
    let assigned = assigned_pci_addresses(client, node).await?;

    // Filter to free GPUs, sort by: preferred vendor first, then priority
    let mut free: Vec<&GpuEntry> = gpus
        .iter()
        .filter(|g| !assigned.iter().any(|a| gpu_matches(g, a)))
        .collect();

    free.sort_by(|a, b| {
        let a_pref = if a.vendor == preference { 0 } else { 1 };
        let b_pref = if b.vendor == preference { 0 } else { 1 };
        a_pref.cmp(&b_pref).then(a.priority.cmp(&b.priority))
    });

    Ok(free.first().cloned().cloned())
}

/// Check which GPU (if any) is assigned to a specific VM.
pub async fn gpu_assigned_to_vm(
    gpus: &[GpuEntry],
    client: &ProxmoxClient,
    node: &str,
    vmid: u32,
) -> Result<Option<GpuEntry>, GpuPoolError> {
    let config = proxmox_ext::get_vm_config(client, node, vmid)
        .await
        .map_err(GpuPoolError::Proxmox)?;

    let hostpci_values = extract_hostpci_values(&config);

    for gpu in gpus {
        if hostpci_values
            .iter()
            .any(|v| gpu_matches(gpu, v))
        {
            return Ok(Some(gpu.clone()));
        }
    }

    Ok(None)
}

/// Get the status of all GPUs in the pool (free or assigned to which VM).
pub async fn gpu_status_all(
    gpus: &[GpuEntry],
    client: &ProxmoxClient,
    node: &str,
) -> Result<Vec<GpuStatus>, GpuPoolError> {
    let vms = proxmox_ext::list_vms(client, node)
        .await
        .map_err(GpuPoolError::Proxmox)?;

    let mut results: Vec<GpuStatus> = gpus
        .iter()
        .map(|g| GpuStatus {
            gpu: g.clone(),
            assigned_to: None,
        })
        .collect();

    for vm in &vms {
        if vm.status != "running" {
            continue;
        }
        let config = match proxmox_ext::get_vm_config(client, node, vm.vmid).await {
            Ok(c) => c,
            Err(_) => continue,
        };

        let hostpci_values = extract_hostpci_values(&config);

        for result in &mut results {
            if hostpci_values
                .iter()
                .any(|v| gpu_matches(&result.gpu, v))
            {
                result.assigned_to = Some((
                    vm.vmid,
                    vm.name.clone().unwrap_or_else(|| format!("VM {}", vm.vmid)),
                ));
            }
        }
    }

    Ok(results)
}

/// Collect all PCI addresses assigned to running VMs via hostpci* entries.
async fn assigned_pci_addresses(
    client: &ProxmoxClient,
    node: &str,
) -> Result<Vec<String>, GpuPoolError> {
    let vms = proxmox_ext::list_vms(client, node)
        .await
        .map_err(GpuPoolError::Proxmox)?;

    let mut assigned = Vec::new();

    for vm in &vms {
        if vm.status != "running" {
            continue;
        }
        let config = match proxmox_ext::get_vm_config(client, node, vm.vmid).await {
            Ok(c) => c,
            Err(_) => continue,
        };

        assigned.extend(extract_hostpci_values(&config));
    }

    Ok(assigned)
}

/// Extract all hostpci* values from a VM config JSON object.
fn extract_hostpci_values(config: &serde_json::Value) -> Vec<String> {
    let Some(obj) = config.as_object() else {
        return Vec::new();
    };

    obj.iter()
        .filter(|(k, _)| k.starts_with("hostpci"))
        .filter_map(|(_, v)| v.as_str().map(|s| s.to_string()))
        .collect()
}

/// Check if a GPU matches a hostpci config value.
/// Handles both raw PCI ("0000:43:00.0,pcie=1") and mapped ("mapping=rtx3060,pcie=1") formats.
fn gpu_matches(gpu: &GpuEntry, hostpci_value: &str) -> bool {
    // Check for mapping= format first
    for part in hostpci_value.split(',') {
        if let Some(mapping_id) = part.strip_prefix("mapping=")
            && mapping_id == gpu.mapping_id
        {
            return true;
        }
    }
    // Fall back to raw PCI address matching
    let hostpci_addr = hostpci_value.split(',').next().unwrap_or("");
    let gpu_base = gpu.pci_address.rsplit_once('.').map(|(b, _)| b).unwrap_or(&gpu.pci_address);
    let host_base = hostpci_addr
        .rsplit_once('.')
        .map(|(b, _)| b)
        .unwrap_or(hostpci_addr);
    gpu_base == host_base
}

#[derive(Debug, thiserror::Error)]
pub enum GpuPoolError {
    #[error("Proxmox error: {0}")]
    Proxmox(#[from] ygg_energy::proxmox::ProxmoxError),
}
