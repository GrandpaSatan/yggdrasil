use std::collections::HashMap;

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ygg_domain::chunk::{SearchQuery, SearchResponse};
use ygg_store::postgres::chunks::{
    count_chunks, count_chunks_by_language, count_indexed_files, find_references, lookup_symbols,
};

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

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/symbols  (AST symbol lookup)
// ─────────────────────────────────────────────────────────────────

/// Request body for symbol lookup.
#[derive(Debug, Deserialize)]
pub struct SymbolLookupRequest {
    /// Exact symbol name to find (e.g. "AppState", "health_handler").
    pub name: Option<String>,
    /// Chunk type filter: "function", "struct", "enum", "impl", "trait", "module".
    pub chunk_type: Option<String>,
    /// Language filter: "rust", "go", "python", "typescript", etc.
    pub language: Option<String>,
    /// File path filter (exact match).
    pub file_path: Option<String>,
    /// Max results (default 20, clamped to 100).
    pub limit: Option<u32>,
}

/// A single symbol result.
#[derive(Debug, Serialize)]
pub struct SymbolResult {
    pub id: String,
    pub file_path: String,
    pub name: String,
    pub chunk_type: String,
    pub parent_context: String,
    pub language: String,
    pub start_line: i32,
    pub end_line: i32,
}

/// Response body for symbol lookup.
#[derive(Debug, Serialize)]
pub struct SymbolLookupResponse {
    pub symbols: Vec<SymbolResult>,
}

/// `POST /api/v1/symbols`
///
/// Lookup code symbols by name, type, language, and/or file path.
/// At least one filter must be provided.
pub async fn symbol_lookup_handler(
    State(state): State<AppState>,
    Json(req): Json<SymbolLookupRequest>,
) -> Result<Json<SymbolLookupResponse>, MuninnError> {
    if req.name.is_none()
        && req.chunk_type.is_none()
        && req.language.is_none()
        && req.file_path.is_none()
    {
        return Err(MuninnError::BadRequest(
            "at least one filter (name, chunk_type, language, file_path) is required".to_string(),
        ));
    }

    let limit = req.limit.unwrap_or(20).min(100);
    let rows = lookup_symbols(
        &state.pool,
        req.name.as_deref(),
        req.chunk_type.as_deref(),
        req.language.as_deref(),
        req.file_path.as_deref(),
        limit,
    )
    .await?;

    let symbols: Vec<SymbolResult> = rows
        .into_iter()
        .map(|(id, fp, name, ct, pc, lang, sl, el)| SymbolResult {
            id: id.to_string(),
            file_path: fp,
            name,
            chunk_type: ct,
            parent_context: pc,
            language: lang,
            start_line: sl,
            end_line: el,
        })
        .collect();

    Ok(Json(SymbolLookupResponse { symbols }))
}

// ─────────────────────────────────────────────────────────────────
// POST /api/v1/references  (find references to a symbol)
// ─────────────────────────────────────────────────────────────────

/// Request body for find-references.
#[derive(Debug, Deserialize)]
pub struct FindReferencesRequest {
    /// Symbol name to search for in code content.
    pub symbol: String,
    /// Optional language filter.
    pub language: Option<String>,
    /// Optional: exclude this chunk ID from results (the definition itself).
    pub exclude_id: Option<String>,
    /// Max results (default 20, clamped to 50).
    pub limit: Option<u32>,
}

/// A single reference result.
#[derive(Debug, Serialize)]
pub struct ReferenceResult {
    pub id: String,
    pub file_path: String,
    pub name: String,
    pub chunk_type: String,
    pub parent_context: String,
    pub start_line: i32,
    pub end_line: i32,
    pub relevance: f64,
}

/// Response body for find-references.
#[derive(Debug, Serialize)]
pub struct FindReferencesResponse {
    pub references: Vec<ReferenceResult>,
}

/// `POST /api/v1/references`
///
/// Find code chunks that reference a given symbol name using BM25 text search.
pub async fn find_references_handler(
    State(state): State<AppState>,
    Json(req): Json<FindReferencesRequest>,
) -> Result<Json<FindReferencesResponse>, MuninnError> {
    if req.symbol.trim().is_empty() {
        return Err(MuninnError::BadRequest("symbol must not be empty".to_string()));
    }

    let exclude_id = req
        .exclude_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok());

    let limit = req.limit.unwrap_or(20).min(50);
    let rows = find_references(
        &state.pool,
        &req.symbol,
        req.language.as_deref(),
        exclude_id,
        limit,
    )
    .await?;

    let references: Vec<ReferenceResult> = rows
        .into_iter()
        .map(|(id, fp, name, ct, pc, sl, el, rank)| ReferenceResult {
            id: id.to_string(),
            file_path: fp,
            name,
            chunk_type: ct,
            parent_context: pc,
            start_line: sl,
            end_line: el,
            relevance: rank,
        })
        .collect();

    Ok(Json(FindReferencesResponse { references }))
}
