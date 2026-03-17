use serde::Deserialize;
use ygg_energy::proxmox::{ProxmoxClient, ProxmoxError};

/// Summary of a QEMU VM from the Proxmox API.
#[derive(Debug, Deserialize)]
pub struct VmInfo {
    pub vmid: u32,
    pub name: Option<String>,
    pub status: String,
}

/// Check if the Proxmox node is reachable and online.
pub async fn node_online(client: &ProxmoxClient, node: &str) -> Result<bool, ProxmoxError> {
    let url = format!("{}/api2/json/nodes/{}/status", client.base_url(), node);
    let resp = client
        .http_client()
        .get(&url)
        .header("Authorization", format!("PVEAPIToken={}", client.token()))
        .send()
        .await;

    match resp {
        Ok(r) => Ok(r.status().is_success()),
        Err(_) => Ok(false),
    }
}

/// Get the full configuration of a QEMU VM.
pub async fn get_vm_config(
    client: &ProxmoxClient,
    node: &str,
    vmid: u32,
) -> Result<serde_json::Value, ProxmoxError> {
    let url = format!(
        "{}/api2/json/nodes/{}/qemu/{}/config",
        client.base_url(),
        node,
        vmid
    );

    let resp = client
        .http_client()
        .get(&url)
        .header("Authorization", format!("PVEAPIToken={}", client.token()))
        .send()
        .await
        .map_err(|e| ProxmoxError::Network(e.to_string()))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
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

    Ok(body["data"].clone())
}

/// Set VM configuration parameters (e.g., hostpci0=0000:43:00,pcie=1).
pub async fn set_vm_config(
    client: &ProxmoxClient,
    node: &str,
    vmid: u32,
    params: &[(&str, &str)],
) -> Result<(), ProxmoxError> {
    let url = format!(
        "{}/api2/json/nodes/{}/qemu/{}/config",
        client.base_url(),
        node,
        vmid
    );

    let resp = client
        .http_client()
        .put(&url)
        .header("Authorization", format!("PVEAPIToken={}", client.token()))
        .form(params)
        .send()
        .await
        .map_err(|e| ProxmoxError::Network(e.to_string()))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(ProxmoxError::Api {
            message: body,
            status,
        });
    }

    Ok(())
}

/// Delete VM configuration keys (e.g., remove hostpci0 after shutdown).
pub async fn delete_vm_config_keys(
    client: &ProxmoxClient,
    node: &str,
    vmid: u32,
    keys: &[&str],
) -> Result<(), ProxmoxError> {
    let delete_val = keys.join(",");
    set_vm_config(client, node, vmid, &[("delete", &delete_val)]).await
}

/// List all QEMU VMs on a Proxmox node.
pub async fn list_vms(
    client: &ProxmoxClient,
    node: &str,
) -> Result<Vec<VmInfo>, ProxmoxError> {
    let url = format!("{}/api2/json/nodes/{}/qemu", client.base_url(), node);

    let resp = client
        .http_client()
        .get(&url)
        .header("Authorization", format!("PVEAPIToken={}", client.token()))
        .send()
        .await
        .map_err(|e| ProxmoxError::Network(e.to_string()))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
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

    let vms: Vec<VmInfo> = body["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    Ok(vms)
}
