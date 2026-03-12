use serde::{Deserialize, Serialize};

/// Energy policy for a node — determines power management behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnergyPolicy {
    /// Node must always be running. Never auto-sleep.
    AlwaysOn,
    /// Node can be woken on demand and put to sleep after idle timeout.
    OnDemand,
    /// Node is excluded from energy management entirely.
    Excluded,
    /// Node is prioritized — woken first, slept last.
    Prioritized,
}

impl Default for EnergyPolicy {
    fn default() -> Self {
        Self::AlwaysOn
    }
}

/// Per-node energy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeEnergyConfig {
    pub node_name: String,
    pub policy: EnergyPolicy,
    /// MAC address for Wake-on-LAN (required for on_demand/prioritized).
    #[serde(default)]
    pub wol_mac: Option<String>,
    /// Broadcast address for WoL (default: 255.255.255.255).
    #[serde(default = "default_broadcast")]
    pub wol_broadcast: String,
    /// Idle timeout in minutes before sleep (default: 30).
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_mins: u64,
    /// Proxmox API URL (if this node is a Proxmox VM/CT).
    #[serde(default)]
    pub proxmox_url: Option<String>,
    /// Proxmox node name (host) the VM runs on.
    #[serde(default)]
    pub proxmox_node: Option<String>,
    /// Proxmox VM ID (if managed via Proxmox).
    #[serde(default)]
    pub proxmox_vmid: Option<u32>,
}

fn default_broadcast() -> String {
    "255.255.255.255".to_string()
}

fn default_idle_timeout() -> u64 {
    30
}

/// Top-level energy configuration (loaded from configs/energy/config.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnergyConfig {
    pub nodes: Vec<NodeEnergyConfig>,
}
