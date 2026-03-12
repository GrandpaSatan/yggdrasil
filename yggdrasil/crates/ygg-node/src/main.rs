//! Yggdrasil mesh node daemon.
//!
//! Runs the mesh networking layer: mDNS discovery, heartbeats, gate policy,
//! energy management, and inter-node HTTP proxy. Each physical/virtual node
//! runs one instance.
//!
//! Usage:
//!   ygg-node [--config <path>] [--energy-config <path>] [--bind <addr:port>]

mod handlers;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt};
use ygg_domain::mesh::{ClusterConfig, NodeCapabilities, ServiceEndpoint};
use ygg_energy::EnergyManager;
use ygg_mesh::discovery::{self, DiscoveryEvent};
use ygg_mesh::gate::Gate;
use ygg_mesh::proxy::MeshProxy;
use ygg_mesh::NodeRegistry;

/// Yggdrasil mesh node daemon.
#[derive(Debug, Parser)]
#[command(name = "ygg-node", version, about)]
struct Args {
    /// Path to the JSON cluster configuration file.
    #[arg(
        short,
        long,
        default_value = "configs/cluster/config.json",
        env = "YGG_CLUSTER_CONFIG"
    )]
    config: std::path::PathBuf,

    /// Path to the JSON energy policy configuration file.
    #[arg(
        long,
        default_value = "configs/energy/config.json",
        env = "YGG_ENERGY_CONFIG"
    )]
    energy_config: std::path::PathBuf,

    /// Override bind address for the mesh API.
    #[arg(short, long, env = "YGG_NODE_BIND")]
    bind: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    info!(config = %args.config.display(), "loading cluster configuration");

    let config: ClusterConfig = ygg_config::load_json(&args.config)
        .with_context(|| format!("failed to load config: {}", args.config.display()))?;

    // Load energy configuration (optional — warn and continue without if missing)
    let energy_manager: Option<Arc<EnergyManager>> =
        match ygg_config::load_json::<ygg_energy::EnergyConfig>(&args.energy_config) {
            Ok(energy_cfg) => {
                info!(
                    path = %args.energy_config.display(),
                    nodes = energy_cfg.nodes.len(),
                    "loaded energy configuration"
                );
                Some(Arc::new(EnergyManager::new(energy_cfg.nodes, None)))
            }
            Err(e) => {
                warn!(
                    path = %args.energy_config.display(),
                    error = %e,
                    "failed to load energy config — energy management disabled"
                );
                None
            }
        };

    let bind_addr = args.bind.unwrap_or_else(|| {
        format!("{}:{}", config.node.advertise_addr, config.node.mesh_port)
    });

    info!(
        node = %config.node.name,
        role = %config.node.role,
        services = ?config.node.services,
        bind = %bind_addr,
        energy = energy_manager.is_some(),
        "mesh node starting"
    );

    // Build local capabilities from config
    let capabilities = build_capabilities(&config, &energy_manager).await;

    // Initialize the node registry
    let registry = Arc::new(NodeRegistry::new(config.clone()));
    let gate = Gate::new(config.gate.clone());
    let proxy = Arc::new(MeshProxy::new((*registry).clone(), gate));

    // Start discovery
    let mut discovery_rx = discovery::start_discovery(&config).await?;

    // Spawn discovery event processor
    let registry_disc = Arc::clone(&registry);
    let local_hello = registry.local_hello(capabilities.clone());
    tokio::spawn(async move {
        while let Some(event) = discovery_rx.recv().await {
            match event {
                DiscoveryEvent::NodeFound { addr, port, name } => {
                    info!(peer = %name, addr = %addr, port = port, "attempting handshake");
                    match discovery::handshake(&addr, port, &local_hello).await {
                        Ok(remote_hello) => {
                            registry_disc.register(remote_hello);
                        }
                        Err(e) => {
                            tracing::warn!(peer = %name, error = %e, "handshake failed");
                        }
                    }
                }
                DiscoveryEvent::NodeLost { name } => {
                    info!(peer = %name, "node lost from discovery");
                    registry_disc.remove(&name);
                }
            }
        }
    });

    // Spawn heartbeat sender with energy management integration
    let registry_hb = Arc::clone(&registry);
    let energy_hb = energy_manager.clone();
    let hb_interval = config.heartbeat.interval_secs;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(hb_interval));
        let mut idle_check_counter: u64 = 0;
        loop {
            interval.tick().await;
            let hb = registry_hb.local_heartbeat();

            // Send heartbeat to all online peers
            let online_nodes = registry_hb.online_nodes();
            for node in &online_nodes {
                let url = format!(
                    "http://{}:{}/api/v1/mesh/heartbeat",
                    node.identity.advertise_addr, node.identity.mesh_port
                );
                let hb_clone = hb.clone();
                tokio::spawn(async move {
                    let client = reqwest::Client::new();
                    let _ = client.post(&url).json(&hb_clone).send().await;
                });
            }

            // Record activity for all online nodes in the energy manager
            if let Some(ref energy) = energy_hb {
                for node in &online_nodes {
                    energy.record_activity(&node.identity.name).await;
                }

                // Check idle nodes every 5 heartbeat cycles to avoid excessive checking
                idle_check_counter += 1;
                if idle_check_counter.is_multiple_of(5) {
                    energy.check_idle().await;
                }
            }

            // Check liveness of all known nodes
            registry_hb.check_liveness();
        }
    });

    // Build HTTP API
    let app_state = handlers::AppState {
        registry: Arc::clone(&registry),
        proxy: Arc::clone(&proxy),
        local_capabilities: capabilities,
        energy: energy_manager,
    };

    let app = Router::new()
        .route("/health", get(handlers::health))
        .route("/api/v1/mesh/hello", post(handlers::mesh_hello))
        .route("/api/v1/mesh/heartbeat", post(handlers::mesh_heartbeat))
        .route("/api/v1/mesh/nodes", get(handlers::list_nodes))
        .route("/api/v1/mesh/proxy", post(handlers::mesh_proxy))
        .route("/api/v1/mesh/energy", get(handlers::energy_status))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind to {}", bind_addr))?;

    info!(addr = %bind_addr, "mesh node ready");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("mesh node HTTP server error")?;

    info!("mesh node shut down cleanly");
    Ok(())
}

async fn build_capabilities(
    config: &ClusterConfig,
    energy_manager: &Option<Arc<EnergyManager>>,
) -> NodeCapabilities {
    let mut services = HashMap::new();
    for svc in &config.node.services {
        // Default service endpoint based on well-known ports
        let (port, health) = match svc.as_str() {
            "odin" => (8080, "/health"),
            "mimir" => (9090, "/health"),
            "muninn" => (9091, "/health"),
            "huginn" => (9092, "/health"),
            _ => (8080, "/health"),
        };
        services.insert(
            svc.clone(),
            ServiceEndpoint {
                url: format!("http://{}:{}", config.node.advertise_addr, port),
                health_path: health.to_string(),
            },
        );
    }

    // Resolve this node's energy policy from the energy manager
    let energy_policy = if let Some(energy) = energy_manager {
        energy
            .status(&config.node.name)
            .await
            .map(|s| format!("{:?}", s).to_lowercase())
    } else {
        None
    };

    NodeCapabilities {
        services,
        has_gpu: false,
        energy_policy,
    }
}

async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install CTRL+C handler");
        }
    };

    #[cfg(unix)]
    let sigterm = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { info!("received SIGINT, shutting down"); }
        _ = sigterm => { info!("received SIGTERM, shutting down"); }
    }
}
