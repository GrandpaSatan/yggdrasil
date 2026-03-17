use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GamingConfig {
    pub proxmox: ProxmoxConfig,
    pub thor_wol: WolConfig,
    pub gpus: Vec<GpuEntry>,
    pub vms: Vec<VmEntry>,
    #[serde(default)]
    pub timeouts: TimeoutConfig,
    /// Directory containing master Sunshine pairing data to auto-deploy to VMs.
    /// Should contain: sunshine_state.json, credentials/cacert.pem, credentials/cakey.pem
    pub pairing_source: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProxmoxConfig {
    pub url: String,
    pub token: String,
    pub node: String,
}

#[derive(Debug, Deserialize)]
pub struct WolConfig {
    pub mac: String,
    #[serde(default = "default_broadcast")]
    pub broadcast: String,
}

fn default_broadcast() -> String {
    "<thor-ip>55".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct GpuEntry {
    pub name: String,
    pub pci_address: String,
    /// Proxmox PCI resource mapping ID (e.g., "rtx2070super").
    /// Required for API token auth — raw PCI addresses are root-only.
    pub mapping_id: String,
    pub iommu_group: u32,
    pub vendor: String,
    pub priority: u32,
}

#[derive(Debug, Deserialize)]
pub struct VmEntry {
    pub name: String,
    pub vmid: u32,
    #[serde(default = "default_sunshine_port")]
    pub sunshine_port: u16,
    pub ip: Option<String>,
    #[serde(default = "default_gpu_preference")]
    pub gpu_preference: String,
    #[serde(default = "default_hostpci_slot")]
    pub hostpci_slot: String,
    /// SSH user for pairing (e.g., "yggdrasil").
    pub ssh_user: Option<String>,
    /// Sunshine web UI credentials (user:pass).
    pub sunshine_creds: Option<String>,
}

fn default_sunshine_port() -> u16 {
    47990
}
fn default_gpu_preference() -> String {
    "nvidia".to_string()
}
fn default_hostpci_slot() -> String {
    "hostpci0".to_string()
}

#[derive(Debug, Deserialize)]
pub struct TimeoutConfig {
    #[serde(default = "default_30")]
    pub wol_poll_secs: u64,
    #[serde(default = "default_5")]
    pub wol_poll_interval_secs: u64,
    #[serde(default = "default_300")]
    pub vm_start_timeout_secs: u64,
    #[serde(default = "default_10")]
    pub vm_start_poll_interval_secs: u64,
    #[serde(default = "default_120")]
    pub vm_stop_timeout_secs: u64,
}

fn default_30() -> u64 { 30 }
fn default_5() -> u64 { 5 }
fn default_300() -> u64 { 300 }
fn default_10() -> u64 { 10 }
fn default_120() -> u64 { 120 }

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            wol_poll_secs: 30,
            wol_poll_interval_secs: 5,
            vm_start_timeout_secs: 300,
            vm_start_poll_interval_secs: 10,
            vm_stop_timeout_secs: 120,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("environment variable {var} not set")]
    Env { var: String },
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

/// Load config from a JSON file, expanding `${VAR}` placeholders from env.
pub fn load_config(path: &Path) -> Result<GamingConfig, ConfigError> {
    let raw = std::fs::read_to_string(path)?;

    // Expand ${VAR} patterns
    let mut expanded = raw;
    loop {
        let Some(start) = expanded.find("${") else {
            break;
        };
        let Some(end) = expanded[start..].find('}') else {
            break;
        };
        let var_name = &expanded[start + 2..start + end];
        let value = std::env::var(var_name).map_err(|_| ConfigError::Env {
            var: var_name.to_string(),
        })?;
        expanded = format!("{}{}{}", &expanded[..start], value, &expanded[start + end + 1..]);
    }

    let config: GamingConfig = serde_json::from_str(&expanded)?;
    Ok(config)
}
