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
