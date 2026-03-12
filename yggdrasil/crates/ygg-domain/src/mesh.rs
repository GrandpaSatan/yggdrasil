use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Cluster-level configuration for node mesh networking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// This node's identity within the mesh.
    pub node: NodeIdentity,
    /// How nodes discover each other.
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    /// Heartbeat and timeout settings.
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
    /// Gate policy rules for inter-node tool access.
    #[serde(default)]
    pub gate: GateConfig,
}

/// This node's identity and capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeIdentity {
    /// Unique node name (e.g. "munin", "hugin", "thor").
    pub name: String,
    /// Human-readable role description.
    #[serde(default)]
    pub role: String,
    /// Services hosted on this node (e.g. ["odin", "mimir"]).
    #[serde(default)]
    pub services: Vec<String>,
    /// Address this node advertises to the mesh (e.g. "REDACTED_MUNIN_IP").
    pub advertise_addr: String,
    /// Port for mesh API (default 9100).
    #[serde(default = "default_mesh_port")]
    pub mesh_port: u16,
}

fn default_mesh_port() -> u16 {
    9100
}

/// Discovery mechanism configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    /// Discovery mode: "mdns" or "static".
    #[serde(default = "default_discovery_mode")]
    pub mode: DiscoveryMode,
    /// Static seed nodes (used when mode = "static" or as bootstrap for mDNS).
    #[serde(default)]
    pub seeds: Vec<SeedNode>,
    /// mDNS service name (default: "_yggdrasil._tcp.local").
    #[serde(default = "default_mdns_service")]
    pub mdns_service: String,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            mode: DiscoveryMode::Mdns,
            seeds: Vec::new(),
            mdns_service: default_mdns_service(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiscoveryMode {
    Mdns,
    Static,
}

fn default_discovery_mode() -> DiscoveryMode {
    DiscoveryMode::Mdns
}

fn default_mdns_service() -> String {
    "_yggdrasil._tcp.local".to_string()
}

/// A statically configured seed node for bootstrap or static-only mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedNode {
    pub name: String,
    pub addr: String,
    pub port: u16,
}

/// Heartbeat configuration for mesh liveness detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatConfig {
    /// Heartbeat interval in seconds (default 30).
    #[serde(default = "default_heartbeat_interval")]
    pub interval_secs: u64,
    /// Number of missed heartbeats before a node is considered offline (default 3).
    #[serde(default = "default_missed_threshold")]
    pub missed_threshold: u32,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_heartbeat_interval(),
            missed_threshold: default_missed_threshold(),
        }
    }
}

fn default_heartbeat_interval() -> u64 {
    30
}

fn default_missed_threshold() -> u32 {
    3
}

/// Gate policy engine for controlling inter-node access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateConfig {
    /// Default policy when no rule matches (default: allow).
    #[serde(default = "default_gate_policy")]
    pub default_policy: GatePolicy,
    /// Deny rules evaluated in order. First match wins.
    #[serde(default)]
    pub rules: Vec<GateRule>,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            default_policy: GatePolicy::Allow,
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GatePolicy {
    Allow,
    Deny,
}

fn default_gate_policy() -> GatePolicy {
    GatePolicy::Allow
}

/// A gate rule matching source/target node and tool name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateRule {
    /// Source node name pattern (glob, e.g. "*" or "thor").
    pub source: String,
    /// Target tool name pattern (glob, e.g. "ha_*" or "*").
    pub tool: String,
    /// Policy to apply when matched.
    pub policy: GatePolicy,
}

/// Runtime state of a node in the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeState {
    /// Node identity.
    pub identity: NodeIdentity,
    /// Current status.
    pub status: NodeStatus,
    /// Capabilities advertised by this node.
    pub capabilities: NodeCapabilities,
    /// Last successful heartbeat timestamp (Unix epoch seconds).
    pub last_heartbeat: u64,
    /// Mesh protocol version.
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeStatus {
    Online,
    Offline,
    Waking,
    Sleeping,
}

/// Capabilities a node advertises to the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapabilities {
    /// Services available on this node with their addresses.
    pub services: HashMap<String, ServiceEndpoint>,
    /// Whether this node has GPU compute available.
    #[serde(default)]
    pub has_gpu: bool,
    /// Energy policy for this node.
    #[serde(default)]
    pub energy_policy: Option<String>,
}

/// A service endpoint within a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceEndpoint {
    /// HTTP address (e.g. "http://REDACTED_MUNIN_IP:8080").
    pub url: String,
    /// Health check path (default: "/health").
    #[serde(default = "default_health_path")]
    pub health_path: String,
}

fn default_health_path() -> String {
    "/health".to_string()
}

/// Mesh handshake request/response — exchanged on first contact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshHello {
    pub node: NodeIdentity,
    pub capabilities: NodeCapabilities,
    pub version: String,
}

/// Mesh proxy request — routes a request through the mesh to a target node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshProxyRequest {
    /// Source node initiating the request.
    pub source_node: String,
    /// Target service name on the receiving node.
    pub service: String,
    /// HTTP method (GET, POST, etc.).
    pub method: String,
    /// Path on the target service (e.g. "/api/v1/search").
    pub path: String,
    /// Optional request body (JSON string).
    #[serde(default)]
    pub body: Option<String>,
    /// Optional headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// Mesh proxy response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshProxyResponse {
    pub status: u16,
    pub body: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// Heartbeat payload sent periodically between nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub node_name: String,
    pub timestamp: u64,
    /// Current load (optional, for future routing decisions).
    #[serde(default)]
    pub load: Option<f64>,
}
