use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;
use tracing::info;
use ygg_domain::mesh::{ClusterConfig, DiscoveryMode, MeshHello, SeedNode};

/// Events emitted by the discovery subsystem.
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// A new node was discovered.
    NodeFound { addr: String, port: u16, name: String },
    /// A node was removed from mDNS.
    NodeLost { name: String },
}

/// mDNS service type for Yggdrasil mesh.
const MDNS_SERVICE_TYPE: &str = "_yggdrasil._tcp.local.";

/// Starts discovery based on the configured mode.
/// Returns a channel that emits discovery events.
pub async fn start_discovery(
    config: &ClusterConfig,
) -> anyhow::Result<mpsc::Receiver<DiscoveryEvent>> {
    let (tx, rx) = mpsc::channel(64);

    // Clone tx for bootstrap seeds before moving into discovery functions
    let tx_bootstrap = tx.clone();

    match config.discovery.mode {
        DiscoveryMode::Mdns => {
            start_mdns_discovery(config, tx).await?;
        }
        DiscoveryMode::Static => {
            start_static_discovery(&config.discovery.seeds, tx).await?;
        }
    }

    // Always try static seeds as bootstrap, even in mDNS mode
    if config.discovery.mode == DiscoveryMode::Mdns && !config.discovery.seeds.is_empty() {
        let seeds = config.discovery.seeds.clone();
        tokio::spawn(async move {
            for seed in seeds {
                let _ = tx_bootstrap
                    .send(DiscoveryEvent::NodeFound {
                        addr: seed.addr,
                        port: seed.port,
                        name: seed.name,
                    })
                    .await;
            }
        });
    }

    Ok(rx)
}

/// Register this node as an mDNS service and browse for peers.
async fn start_mdns_discovery(
    config: &ClusterConfig,
    tx: mpsc::Sender<DiscoveryEvent>,
) -> anyhow::Result<()> {
    let mdns = ServiceDaemon::new()?;
    let node = &config.node;

    // Register ourselves
    let service_info = ServiceInfo::new(
        MDNS_SERVICE_TYPE,
        &node.name,
        &format!("{}.local.", node.name),
        &node.advertise_addr,
        node.mesh_port,
        [
            ("role", node.role.as_str()),
            ("services", &node.services.join(",")),
            ("version", env!("CARGO_PKG_VERSION")),
        ]
        .as_slice(),
    )?;

    mdns.register(service_info)?;
    info!(
        name = %node.name,
        addr = %node.advertise_addr,
        port = node.mesh_port,
        "registered mDNS service"
    );

    // Browse for peers
    let receiver = mdns.browse(MDNS_SERVICE_TYPE)?;
    let local_name = node.name.clone();

    tokio::spawn(async move {
        while let Ok(event) = receiver.recv_async().await {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let name = info.get_fullname().to_string();
                    // Skip ourselves
                    if name.contains(&local_name) {
                        continue;
                    }

                    let addrs: Vec<_> = info.get_addresses().iter().collect();
                    if let Some(addr) = addrs.first() {
                        let port = info.get_port();
                        let short_name = info
                            .get_property_val_str("name")
                            .unwrap_or(&name)
                            .to_string();

                        info!(
                            peer = %short_name,
                            addr = %addr,
                            port = port,
                            "mDNS: discovered peer"
                        );

                        let _ = tx
                            .send(DiscoveryEvent::NodeFound {
                                addr: addr.to_string(),
                                port,
                                name: short_name,
                            })
                            .await;
                    }
                }
                ServiceEvent::ServiceRemoved(_, name) => {
                    info!(name = %name, "mDNS: peer removed");
                    let _ = tx.send(DiscoveryEvent::NodeLost { name }).await;
                }
                _ => {}
            }
        }
    });

    Ok(())
}

/// Static discovery: emit all seeds as found nodes immediately.
async fn start_static_discovery(
    seeds: &[SeedNode],
    tx: mpsc::Sender<DiscoveryEvent>,
) -> anyhow::Result<()> {
    for seed in seeds {
        info!(
            name = %seed.name,
            addr = %seed.addr,
            port = seed.port,
            "static discovery: seed node"
        );
        let _ = tx
            .send(DiscoveryEvent::NodeFound {
                addr: seed.addr.clone(),
                port: seed.port,
                name: seed.name.clone(),
            })
            .await;
    }
    Ok(())
}

/// Perform a handshake with a discovered node.
/// Sends our MeshHello and receives theirs.
pub async fn handshake(
    addr: &str,
    port: u16,
    local_hello: &MeshHello,
) -> anyhow::Result<MeshHello> {
    let url = format!("http://{}:{}/api/v1/mesh/hello", addr, port);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let resp = client.post(&url).json(local_hello).send().await?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "handshake with {}:{} failed: HTTP {}",
            addr,
            port,
            resp.status()
        );
    }

    let remote_hello: MeshHello = resp.json().await?;
    info!(
        peer = %remote_hello.node.name,
        version = %remote_hello.version,
        "handshake complete"
    );

    Ok(remote_hello)
}
