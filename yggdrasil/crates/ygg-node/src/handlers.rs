use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use tracing::info;
use ygg_domain::mesh::{
    Heartbeat, MeshHello, MeshProxyRequest, MeshProxyResponse, NodeCapabilities, NodeState,
};
use ygg_energy::EnergyManager;
use ygg_mesh::proxy::MeshProxy;
use ygg_mesh::NodeRegistry;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<NodeRegistry>,
    pub proxy: Arc<MeshProxy>,
    pub local_capabilities: NodeCapabilities,
    pub energy: Option<Arc<EnergyManager>>,
}

/// Health check endpoint.
pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok", "service": "ygg-node" }))
}

/// Mesh hello handshake — register the peer and return our identity.
pub async fn mesh_hello(
    State(state): State<AppState>,
    Json(hello): Json<MeshHello>,
) -> impl IntoResponse {
    info!(peer = %hello.node.name, "received mesh hello");

    // Register the remote node
    state.registry.register(hello);

    // Respond with our hello
    let our_hello = state.registry.local_hello(state.local_capabilities.clone());
    Json(our_hello)
}

/// Heartbeat handler — update liveness for the sending node.
pub async fn mesh_heartbeat(
    State(state): State<AppState>,
    Json(hb): Json<Heartbeat>,
) -> impl IntoResponse {
    // Record energy activity for the heartbeating node
    if let Some(ref energy) = state.energy {
        energy.record_activity(&hb.node_name).await;
    }

    state.registry.heartbeat(hb);
    StatusCode::NO_CONTENT
}

/// List all known mesh nodes.
pub async fn list_nodes(State(state): State<AppState>) -> impl IntoResponse {
    let nodes: Vec<NodeState> = state.registry.all_nodes();
    Json(nodes)
}

/// Proxy a request through the mesh to a target service.
pub async fn mesh_proxy(
    State(state): State<AppState>,
    Json(req): Json<MeshProxyRequest>,
) -> impl IntoResponse {
    match state.proxy.proxy(req).await {
        Ok(resp) => (
            StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(resp),
        ),
        Err(e) => {
            let status = match &e {
                ygg_mesh::proxy::ProxyError::Denied { .. } => StatusCode::FORBIDDEN,
                ygg_mesh::proxy::ProxyError::ServiceNotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::BAD_GATEWAY,
            };
            (
                status,
                Json(MeshProxyResponse {
                    status: status.as_u16(),
                    body: e.to_string(),
                    headers: Default::default(),
                }),
            )
        }
    }
}

/// Energy status endpoint — returns power status of all managed nodes.
pub async fn energy_status(State(state): State<AppState>) -> impl IntoResponse {
    match &state.energy {
        Some(energy) => {
            let statuses = energy.status_all().await;
            let body: HashMap<String, String> = statuses
                .into_iter()
                .map(|(name, status)| (name, format!("{:?}", status).to_lowercase()))
                .collect();
            (StatusCode::OK, Json(serde_json::json!({
                "enabled": true,
                "nodes": body,
            })))
        }
        None => (StatusCode::OK, Json(serde_json::json!({
            "enabled": false,
            "nodes": {},
        }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::extract::State;
    use std::collections::HashMap;
    use ygg_domain::mesh::{
        ClusterConfig, GateConfig, NodeIdentity, ServiceEndpoint,
    };
    use ygg_mesh::gate::Gate;

    fn test_identity(name: &str) -> NodeIdentity {
        NodeIdentity {
            name: name.into(),
            role: "test".into(),
            services: vec!["test-svc".into()],
            advertise_addr: "127.0.0.1".into(),
            mesh_port: 9100,
        }
    }

    fn test_capabilities() -> NodeCapabilities {
        let mut services = HashMap::new();
        services.insert(
            "test-svc".to_string(),
            ServiceEndpoint {
                url: "http://127.0.0.1:8080".into(),
                health_path: "/health".into(),
            },
        );
        NodeCapabilities {
            services,
            has_gpu: false,
            energy_policy: None,
        }
    }

    /// Construct a minimal `AppState` for handler tests. The registry is built
    /// twice (once for `state.registry`, once handed off to the `MeshProxy`)
    /// because `NodeRegistry` is not `Clone`. Both share the same node config.
    fn test_state(name: &str) -> AppState {
        let cfg = ClusterConfig {
            node: test_identity(name),
            discovery: Default::default(),
            heartbeat: Default::default(),
            gate: GateConfig::default(),
        };
        let registry = Arc::new(NodeRegistry::new(cfg.clone()));
        let proxy_registry = NodeRegistry::new(cfg);
        let proxy =
            Arc::new(MeshProxy::new(proxy_registry, Gate::new(GateConfig::default())).unwrap());
        AppState {
            registry,
            proxy,
            local_capabilities: test_capabilities(),
            energy: None,
        }
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn health_returns_ok_status() {
        let resp = health().await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["status"], "ok");
        assert_eq!(v["service"], "ygg-node");
    }

    #[tokio::test]
    async fn list_nodes_starts_empty() {
        let state = test_state("self");
        let resp = list_nodes(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn mesh_hello_registers_peer_and_responds_with_local_identity() {
        let state = test_state("local-node");
        let peer = MeshHello {
            node: test_identity("peer-node"),
            capabilities: test_capabilities(),
            version: "test-0".into(),
        };
        let resp = mesh_hello(State(state.clone()), Json(peer))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["node"]["name"], "local-node");

        // Peer should now appear in registry.
        let nodes = state.registry.all_nodes();
        assert!(nodes.iter().any(|n| n.identity.name == "peer-node"));
    }

    #[tokio::test]
    async fn mesh_heartbeat_returns_204() {
        let state = test_state("local");
        let hb = Heartbeat {
            node_name: "peer".into(),
            timestamp: 0,
            load: None,
        };
        let resp = mesh_heartbeat(State(state), Json(hb)).await.into_response();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn energy_status_disabled_when_no_manager() {
        let state = test_state("local");
        let resp = energy_status(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["enabled"], false);
        assert!(v["nodes"].is_object());
    }
}
