use std::time::Duration;

use reqwest::Client;
use tracing::{debug, info};

/// Client for the Proxmox VE REST API.
#[derive(Debug, Clone)]
pub struct ProxmoxClient {
    client: Client,
    base_url: String,
    token: String,
}

impl ProxmoxClient {
    /// Create a new Proxmox client.
    /// - `base_url`: e.g. "https://REDACTED_THOR_IP:8006"
    /// - `token`: PVE API token in format "USER@REALM!TOKENID=SECRET"
    pub fn new(base_url: String, token: String) -> Self {
        let client = Client::builder()
            .danger_accept_invalid_certs(true) // Proxmox uses self-signed certs
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        Self {
            client,
            base_url,
            token,
        }
    }

    /// Start a QEMU VM on the specified Proxmox node.
    pub async fn start_vm(&self, node: &str, vmid: u32) -> Result<(), ProxmoxError> {
        let url = format!(
            "{}/api2/json/nodes/{}/qemu/{}/status/start",
            self.base_url, node, vmid
        );

        debug!(node = node, vmid = vmid, "starting Proxmox VM");

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("PVEAPIToken={}", self.token))
            .send()
            .await
            .map_err(|e| ProxmoxError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxmoxError::Api {
                message: body,
                status,
            });
        }

        info!(node = node, vmid = vmid, "VM start command accepted");
        Ok(())
    }

    /// Stop a QEMU VM gracefully (ACPI shutdown).
    pub async fn stop_vm(&self, node: &str, vmid: u32) -> Result<(), ProxmoxError> {
        let url = format!(
            "{}/api2/json/nodes/{}/qemu/{}/status/shutdown",
            self.base_url, node, vmid
        );

        debug!(node = node, vmid = vmid, "shutting down Proxmox VM");

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("PVEAPIToken={}", self.token))
            .send()
            .await
            .map_err(|e| ProxmoxError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxmoxError::Api {
                message: body,
                status,
            });
        }

        info!(node = node, vmid = vmid, "VM shutdown command accepted");
        Ok(())
    }

    /// Get the current status of a QEMU VM.
    pub async fn vm_status(&self, node: &str, vmid: u32) -> Result<VmStatus, ProxmoxError> {
        let url = format!(
            "{}/api2/json/nodes/{}/qemu/{}/status/current",
            self.base_url, node, vmid
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("PVEAPIToken={}", self.token))
            .send()
            .await
            .map_err(|e| ProxmoxError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxmoxError::Api {
                message: body,
                status,
            });
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProxmoxError::Network(e.to_string()))?;

        let vm_status = body["data"]["status"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();

        Ok(VmStatus {
            vmid,
            status: vm_status,
        })
    }
}

/// Status of a Proxmox VM.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VmStatus {
    pub vmid: u32,
    pub status: String, // "running", "stopped", etc.
}

#[derive(Debug, thiserror::Error)]
pub enum ProxmoxError {
    #[error("Proxmox API error (HTTP {status}): {message}")]
    Api { message: String, status: u16 },

    #[error("network error: {0}")]
    Network(String),
}
