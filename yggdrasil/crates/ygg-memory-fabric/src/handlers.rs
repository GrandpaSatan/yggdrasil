//! HTTP handlers for fabric endpoints.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use prometheus::Encoder;

use crate::embed::cosine;
use crate::metrics::get as m_get;
use crate::types::*;
use crate::AppState;

pub async fn health() -> &'static str { "ok" }

pub async fn metrics() -> Response {
    let m = m_get();
    let encoder = prometheus::TextEncoder::new();
    let mut buf = Vec::new();
    encoder.encode(&m.registry.gather(), &mut buf).ok();
    (
        StatusCode::OK,
        [("Content-Type", encoder.format_type())],
        buf,
    ).into_response()
}

pub async fn publish(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PublishRequest>,
) -> Result<Json<PublishResponse>, ApiError> {
    let _timer = m_get().publish_latency
        .with_label_values(&[&req.model])
        .start_timer();

    let embedding = match req.embedding {
        Some(v) if v.len() == state.embedder.dim() => v,
        Some(_) => return Err(ApiError::bad_request("embedding dim mismatch")),
        None => state.embedder.embed_one(&req.text).await
            .map_err(|e| ApiError::internal(format!("embed: {e}")))?,
    };

    let record = FabricRecord {
        flow_id: req.flow_id.clone(),
        step_n: req.step_n,
        model: req.model.clone(),
        text: req.text,
        embedding,
        ts: Utc::now().timestamp(),
    };

    state.store
        .append(record, state.cfg.max_records_per_flow, state.cfg.flow_ttl_secs)
        .await
        .map_err(|e| ApiError::internal(format!("store.append: {e}")))?;

    m_get().publish_total.with_label_values(&[&req.model]).inc();

    Ok(Json(PublishResponse {
        flow_id: req.flow_id,
        step_n: req.step_n,
        stored: true,
        embedding_dim: state.embedder.dim(),
    }))
}

pub async fn query(
    State(state): State<Arc<AppState>>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    let records = state.store.list(&req.flow_id).await
        .map_err(|e| ApiError::internal(format!("store.list: {e}")))?;

    let timer_label = if records.is_empty() { "false" } else { "true" };
    let _timer = m_get().query_latency
        .with_label_values(&[timer_label])
        .start_timer();

    m_get().query_total
        .with_label_values(&[bucket_flow_id(&req.flow_id)])
        .inc();

    if records.is_empty() {
        return Ok(Json(QueryResponse { flow_id: req.flow_id, hits: Vec::new() }));
    }

    // Resolve query embedding.
    let q_emb = match (req.embedding, req.query_text) {
        (Some(v), _) if v.len() == state.embedder.dim() => v,
        (Some(_), _) => return Err(ApiError::bad_request("query embedding dim mismatch")),
        (None, Some(text)) if !text.is_empty() => state.embedder
            .embed_one(&text).await
            .map_err(|e| ApiError::internal(format!("embed: {e}")))?,
        _ => return Err(ApiError::bad_request("query_text or embedding required")),
    };

    let mut scored: Vec<(f32, &FabricRecord)> = records.iter()
        .map(|r| (cosine(&q_emb, &r.embedding), r))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let top_k = req.top_k.max(1).min(records.len());
    let hits: Vec<QueryHit> = scored.into_iter().take(top_k).map(|(sim, r)| QueryHit {
        step_n: r.step_n,
        model: r.model.clone(),
        text: r.text.clone(),
        similarity: sim,
        ts: r.ts,
    }).collect();

    // Record L3 hit per (last consumer model → each hit's producer model).
    // Best-effort — pair labeling is coarse. The extension can refine
    // this later by passing the caller model as a header.
    if !hits.is_empty() {
        for h in &hits {
            m_get().l3_hits_total
                .with_label_values(&[&format!("any_to_{}", h.model)])
                .inc();
        }
    }

    Ok(Json(QueryResponse { flow_id: req.flow_id, hits }))
}

pub async fn done(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DoneRequest>,
) -> Result<Json<DoneResponse>, ApiError> {
    let evicted = state.store.evict(&req.flow_id).await
        .map_err(|e| ApiError::internal(format!("store.evict: {e}")))?;
    m_get().evictions_total.with_label_values(&["explicit"]).inc();
    Ok(Json(DoneResponse { flow_id: req.flow_id, evicted }))
}

pub async fn history(
    State(state): State<Arc<AppState>>,
    Path(flow_id): Path<String>,
) -> Result<Json<HistoryResponse>, ApiError> {
    let records = state.store.list(&flow_id).await
        .map_err(|e| ApiError::internal(format!("store.list: {e}")))?;
    Ok(Json(HistoryResponse { flow_id, records }))
}

// ───────── Helpers ─────────

/// Bucket a flow_id down to a low-cardinality label so Prometheus
/// doesn't explode. We use the leading 2 hex chars (0–255 buckets).
fn bucket_flow_id(flow_id: &str) -> &str {
    // Avoid allocation: return a prefix of the str. Since labels need
    // &str with 'static lifetime for register-then-reuse, we hash down
    // to a fixed set via the first character's hex code.
    match flow_id.chars().next() {
        Some('0'..='9') => "digit",
        Some('a'..='f') | Some('A'..='F') => "hex",
        _ => "other",
    }
}

// ───────── Error type ─────────

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: msg.into() }
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::INTERNAL_SERVER_ERROR, message: msg.into() }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}
