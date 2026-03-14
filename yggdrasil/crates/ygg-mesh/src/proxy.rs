use std::time::Duration;

use tracing::debug;
use ygg_domain::mesh::{MeshProxyRequest, MeshProxyResponse, ServiceEndpoint};

use crate::gate::Gate;
use crate::registry::NodeRegistry;

/// Mesh proxy: routes requests through the mesh to target services on remote nodes.
pub struct MeshProxy {
    registry: NodeRegistry,
    gate: Gate,
    client: reqwest::Client,
}

impl MeshProxy {
    pub fn new(registry: NodeRegistry, gate: Gate) -> Result<Self, ProxyError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ProxyError::ClientBuild(e.to_string()))?;

        Ok(Self {
            registry,
            gate,
            client,
        })
    }

    /// Route a proxy request to the appropriate node and service.
    /// The gate is evaluated on the receiving side, but we also check locally
    /// to fail fast when we know the request will be denied.
    pub async fn proxy(&self, req: MeshProxyRequest) -> Result<MeshProxyResponse, ProxyError> {
        // Gate check: is this request allowed?
        if !self.gate.check(&req.source_node, &req.service) {
            return Err(ProxyError::Denied {
                origin: req.source_node,
                tool: req.service,
            });
        }

        // Find the target service in local node capabilities first,
        // otherwise search the mesh registry.
        let (target_node, endpoint) = self.find_service(&req.service)?;

        let url = format!("{}{}", endpoint.url, req.path);
        debug!(
            source = %req.source_node,
            service = %req.service,
            url = %url,
            "proxying mesh request"
        );

        let mut http_req = match req.method.to_uppercase().as_str() {
            "GET" => self.client.get(&url),
            "POST" => self.client.post(&url),
            "PUT" => self.client.put(&url),
            "DELETE" => self.client.delete(&url),
            "PATCH" => self.client.patch(&url),
            other => return Err(ProxyError::UnsupportedMethod(other.to_string())),
        };

        // Add mesh headers
        http_req = http_req
            .header("X-Ygg-Source-Node", &req.source_node)
            .header("X-Ygg-Target-Node", &target_node)
            .header("X-Ygg-Mesh-Proxy", "true");

        // Add custom headers (with injection protection)
        for (k, v) in &req.headers {
            if k.bytes().any(|b| b == b'\r' || b == b'\n')
                || v.bytes().any(|b| b == b'\r' || b == b'\n')
            {
                return Err(ProxyError::InvalidHeader(format!(
                    "header contains invalid characters: {}",
                    k
                )));
            }
            http_req = http_req.header(k, v);
        }

        // Add body if present
        if let Some(body) = &req.body {
            http_req = http_req
                .header("content-type", "application/json")
                .body(body.clone());
        }

        let resp = http_req
            .send()
            .await
            .map_err(|e| ProxyError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| ProxyError::Network(e.to_string()))?;

        Ok(MeshProxyResponse {
            status,
            body,
            headers: Default::default(),
        })
    }

    /// Find a service endpoint across all known nodes.
    /// Returns the node name and the service endpoint.
    fn find_service(&self, service: &str) -> Result<(String, ServiceEndpoint), ProxyError> {
        // Check local node first (implicit, not in registry)
        // Then check online remote nodes
        for node in self.registry.online_nodes() {
            if let Some(ep) = node.capabilities.services.get(service) {
                return Ok((node.identity.name.clone(), ep.clone()));
            }
        }

        Err(ProxyError::ServiceNotFound(service.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("gate denied: {origin} → {tool}")]
    Denied { origin: String, tool: String },

    #[error("service not found in mesh: {0}")]
    ServiceNotFound(String),

    #[error("unsupported HTTP method: {0}")]
    UnsupportedMethod(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("failed to build HTTP client: {0}")]
    ClientBuild(String),

    #[error("invalid header: {0}")]
    InvalidHeader(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use ygg_domain::mesh::{
        ClusterConfig, GateConfig, GatePolicy, HeartbeatConfig, NodeCapabilities,
        NodeIdentity, ServiceEndpoint,
    };

    fn test_registry_with_service(service_name: &str, url: &str) -> NodeRegistry {
        let config = ClusterConfig {
            node: NodeIdentity {
                name: "local".to_string(),
                role: "test".to_string(),
                services: vec![],
                advertise_addr: "127.0.0.1".to_string(),
                mesh_port: 9100,
            },
            discovery: Default::default(),
            heartbeat: HeartbeatConfig {
                interval_secs: 30,
                missed_threshold: 3,
            },
            gate: Default::default(),
        };

        let registry = NodeRegistry::new(config);

        let mut services = HashMap::new();
        services.insert(
            service_name.to_string(),
            ServiceEndpoint {
                url: url.to_string(),
                health_path: "/health".to_string(),
            },
        );

        let hello = ygg_domain::mesh::MeshHello {
            node: NodeIdentity {
                name: "remote-node".to_string(),
                role: "worker".to_string(),
                services: vec![service_name.to_string()],
                advertise_addr: "127.0.0.2".to_string(),
                mesh_port: 9100,
            },
            capabilities: NodeCapabilities {
                services,
                has_gpu: false,
                energy_policy: None,
            },
            version: "0.1.0".to_string(),
        };

        registry.register(hello);
        registry
    }

    fn empty_registry() -> NodeRegistry {
        let config = ClusterConfig {
            node: NodeIdentity {
                name: "local".to_string(),
                role: "test".to_string(),
                services: vec![],
                advertise_addr: "127.0.0.1".to_string(),
                mesh_port: 9100,
            },
            discovery: Default::default(),
            heartbeat: HeartbeatConfig {
                interval_secs: 30,
                missed_threshold: 3,
            },
            gate: Default::default(),
        };

        NodeRegistry::new(config)
    }

    #[tokio::test]
    async fn test_proxy_denied_by_gate() {
        let registry = test_registry_with_service("mimir", "http://127.0.0.1:9090");
        let gate = Gate::new(GateConfig {
            default_policy: GatePolicy::Deny,
            rules: vec![],
        });
        let proxy = MeshProxy::new(registry, gate).unwrap();

        let req = MeshProxyRequest {
            source_node: "thor".to_string(),
            service: "mimir".to_string(),
            method: "GET".to_string(),
            path: "/health".to_string(),
            body: None,
            headers: HashMap::new(),
        };

        let result = proxy.proxy(req).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::Denied { origin, tool } => {
                assert_eq!(origin, "thor");
                assert_eq!(tool, "mimir");
            }
            other => panic!("expected Denied, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_proxy_service_not_found() {
        let registry = empty_registry();
        let gate = Gate::new(GateConfig {
            default_policy: GatePolicy::Allow,
            rules: vec![],
        });
        let proxy = MeshProxy::new(registry, gate).unwrap();

        let req = MeshProxyRequest {
            source_node: "munin".to_string(),
            service: "nonexistent_service".to_string(),
            method: "GET".to_string(),
            path: "/health".to_string(),
            body: None,
            headers: HashMap::new(),
        };

        let result = proxy.proxy(req).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::ServiceNotFound(svc) => {
                assert_eq!(svc, "nonexistent_service");
            }
            other => panic!("expected ServiceNotFound, got: {other:?}"),
        }
    }
}
