use std::collections::HashMap;

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use ygg_domain::chunk::{SearchQuery, SearchResponse};
use ygg_store::postgres::chunks::{count_chunks, count_chunks_by_language, count_indexed_files};

use crate::{assembler::assemble_context, error::MuninnError, search::hybrid_search, state::AppState};

/// Response body for `GET /api/v1/stats`.
///
/// Defined locally in Muninn because these stats are retrieval-specific.
/// Other services should deserialize the JSON directly, not depend on this type.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatsResponse {
    pub total_chunks: i64,
    pub total_files: i64,
    pub languages: HashMap<String, i64>,
}

/// `POST /api/v1/search`
///
/// Accepts a `SearchQuery`, runs hybrid search, assembles context, and returns
/// a `SearchResponse`.
///
/// Validation:
/// - Returns 400 if `query` is empty or whitespace-only.
/// - Clamps `limit` to [1, 50].
/// - Converts `languages` from `Option<Vec<Language>>` to `Option<Vec<String>>`.
pub async fn search_handler(
    State(state): State<AppState>,
    Json(mut query): Json<SearchQuery>,
) -> Result<Json<SearchResponse>, MuninnError> {
    // --- Validate query ---
    if query.query.trim().is_empty() {
        return Err(MuninnError::BadRequest("query must not be empty".to_string()));
    }

    // --- Clamp limit to [1, 50] ---
    query.limit = query.limit.clamp(1, 50);

    // --- Convert language filter ---
    let languages_vec: Option<Vec<String>> = query.languages.as_ref().and_then(|langs| {
        if langs.is_empty() {
            None
        } else {
            let strings: Vec<String> = langs.iter().map(|l| l.as_str().to_string()).collect();
            if strings.is_empty() { None } else { Some(strings) }
        }
    });

    // --- Execute hybrid search ---
    let results = hybrid_search(
        &state,
        &query.query,
        query.limit,
        languages_vec.as_deref(),
    )
    .await?;

    // --- Assemble context string ---
    let effective_budget = (state.search_config.context_token_budget as f64
        * state.search_config.context_fill_ratio) as usize;
    let context = assemble_context(&results, effective_budget);

    Ok(Json(SearchResponse { results, context }))
}

/// `GET /health`
///
/// Returns HTTP 200 with an empty JSON object body.
pub async fn health_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({}))
}

/// `GET /api/v1/stats`
///
/// Returns total chunk count, total file count, and language distribution from PostgreSQL.
pub async fn stats_handler(
    State(state): State<AppState>,
) -> Result<Json<StatsResponse>, MuninnError> {
    let (chunks_result, files_result, langs_result) = tokio::join!(
        count_chunks(&state.pool),
        count_indexed_files(&state.pool),
        count_chunks_by_language(&state.pool),
    );

    let total_chunks = chunks_result?;
    let total_files = files_result?;
    let language_rows = langs_result?;

    let languages: HashMap<String, i64> = language_rows.into_iter().collect();

    Ok(Json(StatsResponse {
        total_chunks,
        total_files,
        languages,
    }))
}
