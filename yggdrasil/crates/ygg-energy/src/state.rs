use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::policy::{EnergyPolicy, NodeEnergyConfig};
use crate::proxmox::ProxmoxClient;
use crate::wol;

/// Power status state machine: Asleep → Waking → Running → Sleeping → Asleep
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodePowerStatus {
    Asleep,
    Waking,
    Running,
    Sleeping,
}

/// Runtime energy state for a single node.
#[derive(Debug, Clone)]
pub struct NodeEnergyState {
    pub config: NodeEnergyConfig,
    pub status: NodePowerStatus,
    pub last_activity: Instant,
}

/// Manages energy state for all nodes in the cluster.
pub struct EnergyManager {
    nodes: Arc<RwLock<HashMap<String, NodeEnergyState>>>,
    proxmox: Option<ProxmoxClient>,
}

/// Maximum time to wait for a Proxmox VM to reach "running" status.
const PROXMOX_POLL_TIMEOUT: Duration = Duration::from_secs(300);
/// Interval between Proxmox VM status polls.
const PROXMOX_POLL_INTERVAL: Duration = Duration::from_secs(10);

impl EnergyManager {
    pub fn new(configs: Vec<NodeEnergyConfig>, proxmox: Option<ProxmoxClient>) -> Self {
        let mut nodes = HashMap::new();
        for cfg in configs {
            let name = cfg.node_name.clone();
            nodes.insert(
                name,
                NodeEnergyState {
                    config: cfg,
                    status: NodePowerStatus::Running, // assume running at startup
                    last_activity: Instant::now(),
                },
            );
        }

        Self {
            nodes: Arc::new(RwLock::new(nodes)),
            proxmox,
        }
    }

    /// Record activity for a node, resetting its idle timer.
    pub async fn record_activity(&self, node_name: &str) {
        let mut nodes = self.nodes.write().await;
        if let Some(state) = nodes.get_mut(node_name) {
            state.last_activity = Instant::now();
        }
    }

    /// Wake a node if it's asleep. Returns Ok(()) when the node is running.
    pub async fn wake_node(&self, node_name: &str) -> Result<(), EnergyError> {
        // Extract wake info under the lock, then release before I/O
        let wake_info = {
            let mut nodes = self.nodes.write().await;
            let state = nodes
                .get_mut(node_name)
                .ok_or_else(|| EnergyError::UnknownNode(node_name.to_string()))?;

            match state.status {
                NodePowerStatus::Running => {
                    info!(node = node_name, "node already running");
                    return Ok(());
                }
                NodePowerStatus::Waking => {
                    info!(node = node_name, "node already waking");
                    return Ok(());
                }
                NodePowerStatus::Sleeping => {
                    warn!(node = node_name, "node is currently sleeping — waiting");
                    return Err(EnergyError::NodeBusy(node_name.to_string()));
                }
                NodePowerStatus::Asleep => {}
            }

            if state.config.policy == EnergyPolicy::Excluded {
                return Err(EnergyError::PolicyExcluded(node_name.to_string()));
            }

            state.status = NodePowerStatus::Waking;
            info!(node = node_name, "initiating wake sequence");

            (
                state.config.wol_mac.clone(),
                state.config.wol_broadcast.clone(),
                state.config.proxmox_node.clone(),
                state.config.proxmox_vmid,
            )
        }; // lock released

        let (wol_mac, wol_broadcast, pve_node, pve_vmid) = wake_info;

        // Send WoL packet
        if let Some(mac) = &wol_mac {
            wol::send_wol(mac, &wol_broadcast)?;
            info!(node = node_name, mac = %mac, "WoL packet sent");
        }

        // If Proxmox-managed, start the VM and poll until running
        if let (Some(proxmox), Some(pve_node), Some(vmid)) =
            (&self.proxmox, &pve_node, pve_vmid)
        {
            proxmox.start_vm(pve_node, vmid).await?;
            info!(node = node_name, vmid = vmid, "Proxmox VM start requested");

            // Poll Proxmox for VM status until it reaches "running" or timeout
            let deadline = Instant::now() + PROXMOX_POLL_TIMEOUT;
            loop {
                tokio::time::sleep(PROXMOX_POLL_INTERVAL).await;

                let vm_status = proxmox.vm_status(pve_node, vmid).await?;
                if vm_status.status == "running" {
                    info!(node = node_name, vmid = vmid, "Proxmox VM confirmed running");
                    break;
                }

                if Instant::now() >= deadline {
                    // Timeout — revert status to Asleep since the VM never came up
                    let mut nodes = self.nodes.write().await;
                    if let Some(state) = nodes.get_mut(node_name) {
                        state.status = NodePowerStatus::Asleep;
                    }
                    return Err(EnergyError::WakeTimeout(node_name.to_string()));
                }

                info!(
                    node = node_name,
                    vmid = vmid,
                    status = %vm_status.status,
                    "waiting for VM to reach running state"
                );
            }
        }

        // Mark as running now that the VM is confirmed up
        let mut nodes = self.nodes.write().await;
        if let Some(state) = nodes.get_mut(node_name) {
            state.status = NodePowerStatus::Running;
            state.last_activity = Instant::now();
        }

        Ok(())
    }

    /// Check for idle nodes and initiate sleep for those past their timeout.
    pub async fn check_idle(&self) {
        // First pass (read lock): collect nodes that need sleeping
        let nodes_to_sleep: Vec<(String, Option<String>, Option<u32>)> = {
            let nodes = self.nodes.read().await;
            let now = Instant::now();
            let mut to_sleep = Vec::new();

            for (name, state) in nodes.iter() {
                if state.status != NodePowerStatus::Running {
                    continue;
                }

                match state.config.policy {
                    EnergyPolicy::AlwaysOn | EnergyPolicy::Excluded => continue,
                    EnergyPolicy::OnDemand | EnergyPolicy::Prioritized => {}
                }

                let idle = now.duration_since(state.last_activity);
                let timeout = Duration::from_secs(state.config.idle_timeout_mins * 60);

                if idle > timeout {
                    info!(
                        node = %name,
                        idle_mins = idle.as_secs() / 60,
                        "node exceeded idle timeout — requesting sleep"
                    );
                    to_sleep.push((
                        name.clone(),
                        state.config.proxmox_node.clone(),
                        state.config.proxmox_vmid,
                    ));
                }
            }

            to_sleep
        }; // read lock released

        if nodes_to_sleep.is_empty() {
            return;
        }

        // Mark nodes as Sleeping before issuing stop commands
        {
            let mut nodes = self.nodes.write().await;
            for (name, _, _) in &nodes_to_sleep {
                if let Some(state) = nodes.get_mut(name.as_str()) {
                    state.status = NodePowerStatus::Sleeping;
                }
            }
        } // write lock released

        // Issue Proxmox stop commands (no lock held)
        let mut stopped_nodes = Vec::new();
        for (name, pve_node, pve_vmid) in &nodes_to_sleep {
            if let (Some(proxmox), Some(pve_node), Some(vmid)) =
                (&self.proxmox, pve_node, pve_vmid)
            {
                match proxmox.stop_vm(pve_node, *vmid).await {
                    Ok(()) => {
                        info!(node = %name, vmid = vmid, "Proxmox VM stop requested");
                        stopped_nodes.push(name.clone());
                    }
                    Err(e) => {
                        warn!(node = %name, error = %e, "failed to stop Proxmox VM");
                        // Revert to Running on failure — will retry next cycle
                    }
                }
            } else {
                // No Proxmox management — just mark as asleep directly
                stopped_nodes.push(name.clone());
            }
        }

        // Second pass (write lock): update successfully stopped nodes to Asleep,
        // revert failed ones back to Running.
        {
            let mut nodes = self.nodes.write().await;
            for (name, _, _) in &nodes_to_sleep {
                if let Some(state) = nodes.get_mut(name.as_str()) {
                    if stopped_nodes.contains(name) {
                        state.status = NodePowerStatus::Asleep;
                        info!(node = %name, "node transitioned to Asleep");
                    } else {
                        state.status = NodePowerStatus::Running;
                        warn!(node = %name, "reverted node to Running after failed shutdown");
                    }
                }
            }
        }
    }

    /// Get the current power status of all nodes.
    pub async fn status_all(&self) -> HashMap<String, NodePowerStatus> {
        self.nodes
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.status.clone()))
            .collect()
    }

    /// Get the current power status of a single node.
    pub async fn status(&self, node_name: &str) -> Option<NodePowerStatus> {
        self.nodes
            .read()
            .await
            .get(node_name)
            .map(|s| s.status.clone())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnergyError {
    #[error("unknown node: {0}")]
    UnknownNode(String),

    #[error("node is busy (transitioning): {0}")]
    NodeBusy(String),

    #[error("node excluded from energy management: {0}")]
    PolicyExcluded(String),

    #[error("wake timeout — node did not reach running state: {0}")]
    WakeTimeout(String),

    #[error("WoL error: {0}")]
    Wol(#[from] crate::wol::WolError),

    #[error("Proxmox API error: {0}")]
    Proxmox(#[from] crate::proxmox::ProxmoxError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{EnergyPolicy, NodeEnergyConfig};

    fn make_config(name: &str, policy: EnergyPolicy, idle_timeout_mins: u64) -> NodeEnergyConfig {
        NodeEnergyConfig {
            node_name: name.to_string(),
            policy,
            wol_mac: None,
            wol_broadcast: "255.255.255.255".to_string(),
            idle_timeout_mins,
            proxmox_url: None,
            proxmox_node: None,
            proxmox_vmid: None,
        }
    }

    #[tokio::test]
    async fn test_record_activity() {
        let cfg = make_config("node-a", EnergyPolicy::OnDemand, 30);
        let mgr = EnergyManager::new(vec![cfg], None);

        // Record some time passing
        let before = {
            let nodes = mgr.nodes.read().await;
            nodes.get("node-a").unwrap().last_activity
        };

        // Small delay to ensure timestamp differs
        tokio::time::sleep(Duration::from_millis(10)).await;

        mgr.record_activity("node-a").await;

        let after = {
            let nodes = mgr.nodes.read().await;
            nodes.get("node-a").unwrap().last_activity
        };

        assert!(after > before, "last_activity should be updated");
    }

    #[tokio::test]
    async fn test_wake_already_running() {
        let cfg = make_config("node-a", EnergyPolicy::OnDemand, 30);
        let mgr = EnergyManager::new(vec![cfg], None);

        // Node starts as Running
        let result = mgr.wake_node("node-a").await;
        assert!(result.is_ok(), "waking an already-running node should succeed");

        let status = mgr.status("node-a").await.unwrap();
        assert_eq!(status, NodePowerStatus::Running);
    }

    #[tokio::test]
    async fn test_wake_excluded_node() {
        let cfg = make_config("node-x", EnergyPolicy::Excluded, 30);
        let mgr = EnergyManager::new(vec![cfg], None);

        // Force the node to Asleep so wake path is entered
        {
            let mut nodes = mgr.nodes.write().await;
            nodes.get_mut("node-x").unwrap().status = NodePowerStatus::Asleep;
        }

        let result = mgr.wake_node("node-x").await;
        assert!(result.is_err(), "waking an excluded node should fail");

        match result.unwrap_err() {
            EnergyError::PolicyExcluded(_) => {}
            other => panic!("expected PolicyExcluded, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_check_idle_always_on_skipped() {
        let cfg = make_config("node-a", EnergyPolicy::AlwaysOn, 0);
        let mgr = EnergyManager::new(vec![cfg], None);

        // Set last_activity far in the past
        {
            let mut nodes = mgr.nodes.write().await;
            nodes.get_mut("node-a").unwrap().last_activity =
                Instant::now() - Duration::from_secs(9999);
        }

        mgr.check_idle().await;

        let status = mgr.status("node-a").await.unwrap();
        assert_eq!(
            status,
            NodePowerStatus::Running,
            "AlwaysOn node should never be marked sleeping"
        );
    }

    #[tokio::test]
    async fn test_check_idle_triggers_sleeping() {
        let cfg = make_config("node-b", EnergyPolicy::OnDemand, 0);
        let mgr = EnergyManager::new(vec![cfg], None);

        // Set last_activity in the past so it exceeds the 0-minute timeout
        {
            let mut nodes = mgr.nodes.write().await;
            nodes.get_mut("node-b").unwrap().last_activity =
                Instant::now() - Duration::from_secs(1);
        }

        mgr.check_idle().await;

        // With proxmox: None and no Proxmox config, node goes directly to Asleep
        let status = mgr.status("node-b").await.unwrap();
        assert_eq!(
            status,
            NodePowerStatus::Asleep,
            "idle OnDemand node with 0-min timeout should transition to Asleep"
        );
    }
}
