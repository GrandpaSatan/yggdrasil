//! REST API handlers for config sync and version checking.
//!
//! These are plain HTTP endpoints (not MCP tools) so the local client can
//! call them before the MCP handshake during startup.

use std::sync::Arc;

use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use ygg_store::Store;
use ygg_store::postgres::config_files;

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct VersionResponse {
    pub server_version: String,
    pub client_latest: String,
    pub config_version: String,
}

#[derive(Deserialize)]
pub struct ConfigQuery {
    pub project_id: Option<String>,
}

#[derive(Serialize)]
pub struct ConfigResponse {
    pub file_type: String,
    pub content: String,
    pub content_hash: String,
    pub updated_at: String,
    pub updated_by: String,
}

#[derive(Deserialize)]
pub struct PushConfigRequest {
    pub project_id: Option<String>,
    pub content: String,
    pub workstation_id: String,
}

#[derive(Serialize)]
pub struct PushConfigResponse {
    pub status: String,
    pub config_version: String,
}

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------

fn internal_error(msg: impl std::fmt::Display) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg.to_string()).into_response()
}

// ---------------------------------------------------------------------------
// GET /api/v1/version
// ---------------------------------------------------------------------------

pub async fn get_version(State(store): State<Arc<Store>>) -> Response {
    let pool = store.pool();

    let server_v = config_files::get_version(pool, "server")
        .await
        .ok()
        .flatten()
        .map(|r| r.version)
        .unwrap_or_else(|| "1.0.0".to_string());

    let client_v = config_files::get_version(pool, "client")
        .await
        .ok()
        .flatten()
        .map(|r| r.version)
        .unwrap_or_else(|| "1.0.0".to_string());

    let config_v = config_files::get_version(pool, "config")
        .await
        .ok()
        .flatten()
        .map(|r| r.version)
        .unwrap_or_else(|| "1.0.0".to_string());

    Json(VersionResponse {
        server_version: server_v,
        client_latest: client_v,
        config_version: config_v,
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/v1/config/:file_type?project_id=xxx
// ---------------------------------------------------------------------------

pub async fn get_config(
    State(store): State<Arc<Store>>,
    Path(file_type): Path<String>,
    Query(params): Query<ConfigQuery>,
) -> Response {
    let pool = store.pool();
    let project_id = params.project_id.as_deref();

    match config_files::get_config(pool, &file_type, project_id).await {
        Ok(Some(row)) => Json(ConfigResponse {
            file_type: row.file_type,
            content: row.content,
            content_hash: row.content_hash,
            updated_at: row.updated_at.to_rfc3339(),
            updated_by: row.updated_by,
        })
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => internal_error(e),
    }
}

// ---------------------------------------------------------------------------
// POST /api/v1/config/:file_type
// ---------------------------------------------------------------------------

pub async fn push_config(
    State(store): State<Arc<Store>>,
    Path(file_type): Path<String>,
    Json(body): Json<PushConfigRequest>,
) -> Response {
    let pool = store.pool();
    let project_id = body.project_id.as_deref();

    // Compute SHA-256 of content
    let content_hash = format!("{:x}", Sha256::digest(body.content.as_bytes()));

    // Check if content actually changed
    let changed = match config_files::get_config(pool, &file_type, project_id).await {
        Ok(Some(existing)) => existing.content_hash != content_hash,
        Ok(None) => true,
        Err(e) => return internal_error(e),
    };

    if !changed {
        let config_v = config_files::get_version(pool, "config")
            .await
            .ok()
            .flatten()
            .map(|r| r.version)
            .unwrap_or_else(|| "1.0.0".to_string());

        return Json(PushConfigResponse {
            status: "unchanged".to_string(),
            config_version: config_v,
        })
        .into_response();
    }

    // Upsert the config
    if let Err(e) = config_files::upsert_config(
        pool,
        &file_type,
        project_id,
        &body.content,
        &content_hash,
        &body.workstation_id,
    )
    .await
    {
        return internal_error(e);
    }

    // Bump config version
    let new_version = match config_files::bump_version(pool, "config", "patch").await {
        Ok(v) => v,
        Err(e) => return internal_error(e),
    };

    Json(PushConfigResponse {
        status: "updated".to_string(),
        config_version: new_version,
    })
    .into_response()
}
