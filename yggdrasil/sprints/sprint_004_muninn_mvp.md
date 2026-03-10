# Sprint: 004 - Muninn MVP (Retrieval Engine)
## Status: IMPLEMENTED

## Objective

Build the Muninn retrieval engine as an Axum HTTP server that provides hybrid search over the code chunks indexed by Huginn (Sprint 003). Muninn receives a natural-language query, embeds it via Ollama, runs vector search against Qdrant and BM25 full-text search against PostgreSQL in parallel, fuses the results via Reciprocal Rank Fusion (RRF), fetches full chunk metadata from PostgreSQL, and assembles the top-N results into a context string suitable for LLM consumption. The context string is ordered by file path then line number, with file path headers, and respects a configurable token budget. Muninn exposes three HTTP endpoints: search, health, and stats.

## Scope

### In Scope
- Axum HTTP server on configurable address (default `0.0.0.0:9100`)
- `POST /api/v1/search` -- accepts `SearchQuery`, returns `SearchResponse` with fused results and assembled context string
- `GET /health` -- returns 200 OK with empty JSON object
- `GET /api/v1/stats` -- returns total chunk count, total file count, and language distribution from PostgreSQL
- Startup: connect to Hades PostgreSQL, connect to Hades Qdrant, ensure `code_chunks` collection exists, initialize `EmbedClient`
- YAML config loading via existing `MuninnConfig` from `ygg_domain::config`
- Vector search: embed query text via `EmbedClient::embed_single()`, search Qdrant `code_chunks` collection via `VectorStore::search()`
- BM25 search: use existing `ygg_store::postgres::chunks::search_bm25()` against PostgreSQL tsvector
- Reciprocal Rank Fusion: merge vector and BM25 results using `score = sum(1/(k + rank_i))` with configurable k (default 60)
- Context window assembly: fetch full `CodeChunk` from PostgreSQL, sort by file path then start_line, format with file path headers, enforce token budget (default 80% of 32K = 25,600 tokens, estimated at 4 chars per token = 102,400 characters)
- Parallel execution: vector search and BM25 search run concurrently via `tokio::join!`
- `tower-http` CORS layer for cross-origin requests
- Structured tracing via `tracing` and `tracing-subscriber`
- Graceful error handling -- JSON error responses with appropriate HTTP status codes, never panic on bad input

### Out of Scope
- Authentication / authorization (private LAN, no auth)
- TLS termination (handled by reverse proxy if needed)
- Query rewriting or expansion (raw query passed to both search backends)
- Re-ranking models (RRF is the sole fusion strategy for MVP)
- Caching layer (no query result caching in MVP)
- Streaming responses
- Prometheus metrics endpoint
- WebSocket interface
- Semantic search over engrams (that is Mimir's domain)
- Indexing or writing to any store (Muninn is read-only)
- Pagination (results capped by `limit` parameter, max 50)

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** I/O-bound (network calls to Ollama for query embedding, network calls to Qdrant for vector search, network calls to PostgreSQL for BM25 search and chunk fetching). CPU is idle except for RRF score computation and context string assembly.
- **Target Hardware:** Hugin (REDACTED_HUGIN_IP) -- AMD Ryzen 7 255 (Zen 5, 8C/16T), 64GB DDR5. Collocated with Huginn on the same host.
- **Backend Services:**
  - Hades PostgreSQL: `postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres` (Intel N150, 32GB RAM, SATA SSD pool "Merlin")
  - Hades Qdrant: `http://REDACTED_HADES_IP:6334` (gRPC, same host as PostgreSQL)
  - Hugin local Ollama: `http://localhost:11434` (qwen3-embedding model for query embedding)
- **Utilization Plan:**
  - Tokio runtime with default multi-threaded scheduler. The Ryzen 7 255 has 16 hardware threads. The service is I/O-bound so even 2-4 threads would suffice; the default scheduler provides headroom for concurrent requests.
  - SQLx connection pool: 8 connections. This matches the pool size used by Mimir and Huginn on the same backend. The Hades N150 CPU is the bottleneck, not the connection count.
  - Qdrant client: single gRPC channel with default connection pooling. Qdrant manages its own multiplexing.
  - Parallel search: for each query, vector search (Qdrant) and BM25 search (PostgreSQL) are launched concurrently via `tokio::join!`. This halves the combined latency since the two backends are independent.
  - Batch chunk fetch: after RRF produces the final ranked ID list, fetch all chunks in a single SQL query using `WHERE id = ANY($1)` with a UUID array. This avoids N sequential roundtrips to Hades.
  - Context assembly: purely CPU-bound string concatenation. On a Zen 5 core at ~5 GHz, assembling 50 chunks into a context string completes in microseconds. No parallelism needed.
  - Memory: Muninn holds no long-lived in-memory data structures (no LSH index, no caches). Steady-state RSS is dominated by the Tokio runtime, SQLx pool, and reqwest/Qdrant clients. Expected < 50MB.
  - Co-location with Huginn: both services run on Hugin. Huginn is primarily active during file saves (bursty), while Muninn is active during search queries (bursty). The two workloads are complementary and unlikely to contend on an 8C/16T CPU with 64GB RAM. If contention arises under heavy load, Huginn's indexing semaphore (8 permits) can be reduced to 4.
- **Fallback Strategy:** All operations are standard async I/O. On a lesser machine (e.g., 2-core), Tokio naturally limits concurrency. No hardware-specific optimizations are used. Performance degrades linearly with core count but correctness is unaffected.

## Performance Targets

| Metric | Target | Measurement Method |
|--------|--------|--------------------|
| Search latency P50 (excluding embedding) | < 40ms | `tracing` span on `hybrid_search()` from after embedding completes to fused results returned |
| Search latency P95 (excluding embedding) | < 100ms | Same tracing span, P95 histogram bucket |
| End-to-end search latency P50 (including embedding) | < 200ms | `tracing` span on full `/api/v1/search` handler |
| End-to-end search latency P95 (including embedding) | < 400ms | Same tracing span, P95 histogram bucket |
| Context assembly P95 | < 20ms | `tracing` span on `assemble_context()` |
| RRF fusion P95 | < 1ms | `tracing` span on `reciprocal_rank_fusion()` |
| Batch chunk fetch P95 | < 50ms for 50 chunks | `tracing` span on `get_chunks_by_ids()` |
| Stats endpoint P95 | < 50ms | `tracing` span on `/api/v1/stats` handler |
| Memory ceiling (steady state) | < 50MB RSS | `/proc/self/status` VmRSS |
| Startup time | < 2s (connect to PG + Qdrant + Ollama health check) | Wall clock from process start to "muninn ready" log line |
| Concurrent requests | Handle 10 concurrent search requests without degradation | Load test with `wrk` or `hey`, verify P95 stays under 500ms |

## Data Schemas

### Request: `POST /api/v1/search`

Uses existing `ygg_domain::chunk::SearchQuery`:
```json
{
  "query": "string (required) -- natural language search query",
  "limit": "integer (optional, default 10, max 50) -- number of results to return",
  "languages": ["string (optional) -- filter by language, e.g. [\"rust\", \"python\"]"]
}
```

Field mapping to Rust types:
| Field | Rust Type | Serialization | Validation |
|-------|-----------|---------------|------------|
| `query` | `String` | Required, non-empty | Return 400 if empty or whitespace-only |
| `limit` | `usize` | Default 10 via `default_search_limit()` | Clamp to range [1, 50] |
| `languages` | `Option<Vec<Language>>` | Optional, snake_case enum values | Ignore unknown language values, proceed with valid ones; if all invalid, search all languages |

### Response: `POST /api/v1/search`

Uses existing `ygg_domain::chunk::SearchResponse`:
```json
{
  "results": [
    {
      "chunk": {
        "id": "uuid",
        "file_path": "/absolute/path/to/file.rs",
        "repo_root": "/absolute/path/to/repo",
        "language": "rust",
        "chunk_type": "function",
        "name": "handle_request",
        "parent_context": "impl Server",
        "content": "pub fn handle_request(...) { ... }",
        "start_line": 42,
        "end_line": 67,
        "content_hash": [/* bytes */],
        "indexed_at": "2026-03-09T12:00:00Z"
      },
      "score": 0.0312,
      "source": "fused"
    }
  ],
  "context": "// File: /path/to/file.rs\n// Lines 42-67\npub fn handle_request(...) { ... }\n\n---\n\n// File: ..."
}
```

### Response: `GET /api/v1/stats`

New type (defined in `muninn::handlers` or `ygg_domain::chunk`):
```json
{
  "total_chunks": 1542,
  "total_files": 87,
  "languages": {
    "rust": 1200,
    "python": 200,
    "go": 100,
    "typescript": 42
  }
}
```

Rust struct:
```rust
pub struct StatsResponse {
    pub total_chunks: i64,
    pub total_files: i64,
    pub languages: HashMap<String, i64>,
}
```

### Response: `GET /health`

```json
{}
```

HTTP 200 with empty JSON object body. No type needed -- use `axum::Json(serde_json::json!({}))`.

### Internal: RRF Candidate

Not serialized. Used internally during fusion:
```rust
struct RrfCandidate {
    id: Uuid,
    rrf_score: f64,
    vector_rank: Option<usize>,
    bm25_rank: Option<usize>,
}
```

## API Contracts

### HTTP Endpoints

| Method | Path | Request Body | Response Body | Status Codes |
|--------|------|-------------|---------------|--------------|
| `POST` | `/api/v1/search` | `SearchQuery` (JSON) | `SearchResponse` (JSON) | 200 OK, 400 Bad Request (empty query), 500 Internal Server Error |
| `GET` | `/health` | None | `{}` | 200 OK |
| `GET` | `/api/v1/stats` | None | `StatsResponse` (JSON) | 200 OK, 500 Internal Server Error |

### Error Response Format

All error responses use a consistent JSON envelope:
```json
{
  "error": "human-readable error description"
}
```

HTTP status codes:
- 400: client error (empty query, malformed JSON)
- 500: server error (database connection failure, Qdrant unreachable, embedding service down)

### New Store Functions Required in `ygg_store`

**`crates/ygg-store/src/postgres/chunks.rs` -- add:**

```rust
/// Fetch multiple chunks by their IDs in a single query.
/// Returns chunks in arbitrary order (caller must sort if needed).
/// Missing IDs are silently skipped (no error for IDs not found).
pub async fn get_chunks_by_ids(
    pool: &PgPool,
    ids: &[Uuid],
) -> Result<Vec<CodeChunk>, StoreError>;
```

Implementation: `SELECT ... FROM yggdrasil.code_chunks WHERE id = ANY($1)` binding `ids` as a UUID array. This is the critical optimization that avoids N sequential `get_chunk()` calls.

**`crates/ygg-store/src/postgres/chunks.rs` -- add:**

```rust
/// Get total chunk count.
pub async fn count_chunks(pool: &PgPool) -> Result<i64, StoreError>;

/// Get total indexed file count.
pub async fn count_indexed_files(pool: &PgPool) -> Result<i64, StoreError>;

/// Get chunk count grouped by language.
pub async fn count_chunks_by_language(pool: &PgPool) -> Result<Vec<(String, i64)>, StoreError>;
```

Implementation:
- `count_chunks`: `SELECT COUNT(*) FROM yggdrasil.code_chunks`
- `count_indexed_files`: `SELECT COUNT(*) FROM yggdrasil.indexed_files`
- `count_chunks_by_language`: `SELECT language, COUNT(*) FROM yggdrasil.code_chunks GROUP BY language`

### Existing Store Functions Used (No Changes Needed)

| Function | Module | Used By |
|----------|--------|---------|
| `search_bm25(pool, query, limit, languages)` | `ygg_store::postgres::chunks` | `muninn::search` -- BM25 leg of hybrid search |
| `get_chunk(pool, id)` | `ygg_store::postgres::chunks` | Not used (replaced by batch `get_chunks_by_ids`) |
| `VectorStore::connect(url)` | `ygg_store::qdrant` | `muninn::main` -- startup connection |
| `VectorStore::ensure_collection(name)` | `ygg_store::qdrant` | `muninn::main` -- verify `code_chunks` exists |
| `VectorStore::search(collection, embedding, limit)` | `ygg_store::qdrant` | `muninn::search` -- vector leg of hybrid search |
| `EmbedClient::new(url, model)` | `ygg_embed` | `muninn::main` -- startup init |
| `EmbedClient::embed_single(text)` | `ygg_embed` | `muninn::search` -- embed query text |

## Interface Boundaries

| Module | Owns | Exposes | Depends On |
|--------|------|---------|------------|
| `muninn::main` | Process lifecycle, CLI parsing, config loading, Axum router setup, graceful shutdown | Nothing (binary entrypoint) | `muninn::handlers`, `muninn::state`, `muninn::error`, `ygg_domain::config::MuninnConfig` |
| `muninn::state` | Shared application state (PgPool, VectorStore, EmbedClient, SearchConfig) | `AppState` struct | `ygg_store::Store`, `ygg_store::qdrant::VectorStore`, `ygg_embed::EmbedClient`, `ygg_domain::config::SearchConfig` |
| `muninn::handlers` | HTTP request/response translation, input validation, response serialization | `search_handler()`, `health_handler()`, `stats_handler()` | `muninn::state::AppState`, `muninn::search::hybrid_search()`, `muninn::assembler::assemble_context()`, `muninn::error::MuninnError`, `ygg_domain::chunk::SearchQuery/SearchResponse/SearchResult` |
| `muninn::search` | Hybrid search orchestration: query embedding, parallel vector+BM25 dispatch, RRF fusion | `hybrid_search(state, query, limit, languages) -> Vec<SearchResult>` | `muninn::state::AppState`, `muninn::fusion::reciprocal_rank_fusion()`, `ygg_store::postgres::chunks::search_bm25()`, `ygg_store::postgres::chunks::get_chunks_by_ids()`, `ygg_store::qdrant::VectorStore::search()`, `ygg_embed::EmbedClient::embed_single()` |
| `muninn::fusion` | RRF score computation, result merging, deduplication | `reciprocal_rank_fusion(vector_results, bm25_results, k) -> Vec<(Uuid, f64)>` | None (pure computation, no external dependencies) |
| `muninn::assembler` | Context window assembly: chunk sorting, file-path headers, token budget enforcement | `assemble_context(chunks, token_budget) -> String` | `ygg_domain::chunk::CodeChunk` |
| `muninn::error` | Error type unification, Axum `IntoResponse` implementation | `MuninnError` enum | `ygg_store::error::StoreError`, `ygg_embed::EmbedError` |
| `ygg_store` (external) | PostgreSQL connection pool, chunk CRUD/search, Qdrant vector search | `Store`, `postgres::chunks::*`, `qdrant::VectorStore` | `sqlx`, `qdrant-client`, `ygg_domain` |
| `ygg_embed` (external) | Ollama HTTP communication | `EmbedClient`, `embed_single()` | `reqwest` |
| `ygg_domain` (external) | Type definitions, config structs | `chunk::*`, `config::MuninnConfig`, `config::SearchConfig` | None (leaf crate) |

**Ownership rules:**
- Only `muninn::search` may call `ygg_store::postgres::chunks::*` query functions and `VectorStore::search()`. No other Muninn module touches the database or vector store directly.
- Only `muninn::search` may call `ygg_embed::EmbedClient::embed_single()`. No other Muninn module triggers embedding.
- Only `muninn::handlers` may deserialize HTTP requests or serialize HTTP responses. The search module works with domain types, not HTTP types.
- `muninn::fusion` is a pure function module with no I/O. It receives ranked lists and returns fused scores.
- `muninn::assembler` is a pure function module with no I/O. It receives chunks and returns a string.
- Muninn is strictly read-only. It never writes to PostgreSQL or Qdrant. All writes are Huginn's responsibility.

## File-Level Implementation Plan

### `crates/ygg-store/src/postgres/chunks.rs` (MODIFY)

Add four new functions after the existing `get_chunk()`:

1. `get_chunks_by_ids(pool, ids: &[Uuid]) -> Result<Vec<CodeChunk>, StoreError>` -- batch fetch using `WHERE id = ANY($1)`. Reuse the same column mapping as `get_chunk()`. Missing IDs are silently skipped (fetch_all, not fetch_one).

2. `count_chunks(pool) -> Result<i64, StoreError>` -- `SELECT COUNT(*) FROM yggdrasil.code_chunks`. Return the count as `i64`.

3. `count_indexed_files(pool) -> Result<i64, StoreError>` -- `SELECT COUNT(*) FROM yggdrasil.indexed_files`. Return the count as `i64`.

4. `count_chunks_by_language(pool) -> Result<Vec<(String, i64)>, StoreError>` -- `SELECT language, COUNT(*) as cnt FROM yggdrasil.code_chunks GROUP BY language`. Return tuples of `(language_string, count)`.

### `crates/muninn/src/error.rs` (NEW)

Define `MuninnError` enum:
```rust
pub enum MuninnError {
    Store(ygg_store::error::StoreError),
    Embed(ygg_embed::EmbedError),
    Config(String),
    BadRequest(String),
}
```

Implement:
- `thiserror::Error` derive for `Display`
- `From<StoreError>` for `MuninnError`
- `From<EmbedError>` for `MuninnError`
- `axum::response::IntoResponse` for `MuninnError`:
  - `BadRequest` -> 400 with JSON error body
  - `Store` -> 500 with JSON error body
  - `Embed` -> 500 with JSON error body
  - `Config` -> 500 with JSON error body
  - All error responses use format: `{ "error": "<message>" }`

### `crates/muninn/src/state.rs` (NEW)

Define shared application state:
```rust
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub vectors: VectorStore,
    pub embedder: EmbedClient,
    pub search_config: SearchConfig,
}
```

`AppState` is `Clone` because `PgPool`, `VectorStore`, `EmbedClient`, and `SearchConfig` are all `Clone`. Axum extracts it via `State<AppState>`.

No methods on `AppState` -- it is a data holder. Construction happens in `main.rs`.

### `crates/muninn/src/fusion.rs` (NEW)

Define Reciprocal Rank Fusion:

```rust
/// Merge vector search and BM25 search results using Reciprocal Rank Fusion.
///
/// Each input is a ranked list of (id, original_score) pairs, ordered by rank
/// (index 0 = rank 1). The output is a deduplicated list of (id, rrf_score)
/// sorted descending by rrf_score.
///
/// RRF formula: rrf_score(d) = sum over all rankings R_i of: 1 / (k + rank_i(d))
/// where rank_i(d) is the 1-based rank of document d in ranking R_i.
/// If d does not appear in ranking R_i, that term is 0.
pub fn reciprocal_rank_fusion(
    vector_results: &[(Uuid, f32)],
    bm25_results: &[(Uuid, f64)],
    k: f64,
) -> Vec<(Uuid, f64)>;
```

Implementation:
1. Create a `HashMap<Uuid, f64>` for accumulating RRF scores.
2. For each `(id, _score)` in `vector_results` at index `i`: add `1.0 / (k + (i + 1) as f64)` to the map entry for `id`.
3. For each `(id, _score)` in `bm25_results` at index `i`: add `1.0 / (k + (i + 1) as f64)` to the map entry for `id`.
4. Collect the map into a `Vec<(Uuid, f64)>`.
5. Sort descending by `rrf_score`. Ties broken by UUID (deterministic).
6. Return the sorted vec.

Note: the original scores from Qdrant (cosine similarity) and PostgreSQL (ts_rank) are not used in the RRF computation. Only ordinal rank matters. The original scores are discarded at this stage.

### `crates/muninn/src/assembler.rs` (NEW)

Define context window assembly:

```rust
/// Assemble a context string from ranked search results.
///
/// Chunks are grouped by file_path and sorted by start_line within each group.
/// File groups are ordered by the highest-scoring chunk in each group (so the
/// most relevant file comes first).
///
/// Format:
/// ```
/// // File: /path/to/file.rs
/// // Lines 42-67 (function: handle_request)
/// pub fn handle_request(...) { ... }
///
/// // Lines 100-120 (struct: Config)
/// pub struct Config { ... }
///
/// ---
///
/// // File: /path/to/other.py
/// // Lines 1-30 (function: main)
/// def main(): ...
/// ```
///
/// Enforces a token budget. Tokens are estimated at 4 characters per token.
/// Chunks are added in rank order until the budget is exhausted. A partially
/// fitting chunk is excluded (no mid-chunk truncation).
pub fn assemble_context(
    results: &[SearchResult],
    token_budget: usize,
) -> String;
```

Implementation:
1. Compute `char_budget = token_budget * 4`.
2. Build a `BTreeMap<String, Vec<&SearchResult>>` grouping results by `chunk.file_path`.
3. For each file group, sort chunks by `start_line` ascending.
4. Order file groups by the maximum `score` of any chunk in the group (descending).
5. Iterate through file groups in order. For each group:
   a. Append file header: `// File: {file_path}\n`
   b. For each chunk in the group (by start_line):
      - Format the chunk block: `// Lines {start_line}-{end_line} ({chunk_type}: {name})\n{content}\n\n`
      - Check if adding this block would exceed `char_budget`. If yes, stop adding chunks (but continue to the next file group in case a smaller chunk from another file fits -- however, for simplicity in MVP, stop entirely once budget is exceeded).
      - Append the chunk block.
   c. Append separator: `\n---\n\n`
6. Trim trailing separator.
7. Return the assembled string.

### `crates/muninn/src/search.rs` (NEW)

Define hybrid search orchestration:

```rust
/// Execute hybrid search: embed query, run vector + BM25 in parallel,
/// fuse with RRF, fetch full chunks, return ranked results.
pub async fn hybrid_search(
    state: &AppState,
    query: &str,
    limit: usize,
    languages: Option<&[String]>,
) -> Result<Vec<SearchResult>, MuninnError>;
```

Implementation:
1. **Embed query:** `state.embedder.embed_single(query).await?` to get `Vec<f32>`.
2. **Parallel search:** Use `tokio::join!` to run both concurrently:
   a. Vector search: `state.vectors.search("code_chunks", query_embedding.clone(), (limit * 3) as u64).await?` -- fetch 3x limit to give RRF enough candidates.
   b. BM25 search: `ygg_store::postgres::chunks::search_bm25(&state.pool, query, limit * 3, languages).await?` -- same 3x limit.
3. **Fuse:** `fusion::reciprocal_rank_fusion(&vector_results, &bm25_results, state.search_config.rrf_k)` -- returns `Vec<(Uuid, f64)>` sorted by fused score.
4. **Truncate:** Take the top `limit` entries from the fused list.
5. **Batch fetch:** Collect the UUIDs into a `Vec<Uuid>`. Call `ygg_store::postgres::chunks::get_chunks_by_ids(&state.pool, &ids).await?`.
6. **Assemble results:** For each `(id, rrf_score)` in the truncated fused list, find the matching `CodeChunk` from the batch fetch. Build a `SearchResult { chunk, score: rrf_score, source: SearchSource::Fused }`. Preserve the fused rank order.
7. **Handle missing chunks:** If a UUID from Qdrant is not found in PostgreSQL (stale index), log a warning and skip it. Do not error. This can happen if Huginn re-indexed and deleted a chunk after Qdrant returned it.
8. **Return** the `Vec<SearchResult>`.

### `crates/muninn/src/handlers.rs` (NEW)

Define Axum route handlers:

**`search_handler`**
```rust
pub async fn search_handler(
    State(state): State<AppState>,
    Json(mut query): Json<SearchQuery>,
) -> Result<Json<SearchResponse>, MuninnError>;
```
1. Validate: if `query.query.trim().is_empty()`, return `MuninnError::BadRequest("query must not be empty")`.
2. Clamp `query.limit` to `[1, 50]`.
3. Convert `query.languages` from `Option<Vec<Language>>` to `Option<Vec<String>>` using `Language::as_str().to_string()`. If the vec is empty after conversion, set to `None`.
4. Call `search::hybrid_search(&state, &query.query, query.limit, languages.as_deref()).await?`.
5. Call `assembler::assemble_context(&results, effective_token_budget)` where `effective_token_budget = (state.search_config.context_token_budget as f64 * state.search_config.context_fill_ratio) as usize`.
6. Return `Json(SearchResponse { results, context })`.

**`health_handler`**
```rust
pub async fn health_handler() -> Json<serde_json::Value>;
```
Return `Json(serde_json::json!({}))` with implicit 200 status.

**`stats_handler`**
```rust
pub async fn stats_handler(
    State(state): State<AppState>,
) -> Result<Json<StatsResponse>, MuninnError>;
```
1. Call `count_chunks(&state.pool).await?`.
2. Call `count_indexed_files(&state.pool).await?`.
3. Call `count_chunks_by_language(&state.pool).await?` and collect into `HashMap<String, i64>`.
4. Return `Json(StatsResponse { total_chunks, total_files, languages })`.

### `crates/muninn/src/main.rs` (MODIFY -- replace skeleton)

1. Parse CLI args (keep existing `Cli` struct with `--config` and `--listen_addr`).
2. Load `MuninnConfig` from YAML file: `serde_yaml::from_reader(std::fs::File::open(&cli.config)?)`.
3. Determine listen address: `cli.listen_addr.unwrap_or(config.listen_addr.clone())`.
4. Connect to PostgreSQL: `Store::connect(&config.database_url).await?`.
5. Run migrations: `store.migrate("./migrations").await?`.
6. Connect to Qdrant: `VectorStore::connect(&config.qdrant_url).await?`.
7. Ensure collection: `vectors.ensure_collection("code_chunks").await?`.
8. Initialize embedder: `EmbedClient::new(&config.embed.ollama_url, &config.embed.model)`.
9. Build `AppState { pool: store.pool().clone(), vectors, embedder, search_config: config.search }`.
10. Build Axum router:
    ```rust
    let app = Router::new()
        .route("/health", get(handlers::health_handler))
        .route("/api/v1/search", post(handlers::search_handler))
        .route("/api/v1/stats", get(handlers::stats_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);
    ```
11. Bind and serve: `axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?`.
12. Define `async fn shutdown_signal()` using `tokio::signal::ctrl_c()`.
13. Log "muninn ready on {listen_addr}" at INFO level after binding.

### `configs/muninn/config.yaml` (NEW)

```yaml
listen_addr: "0.0.0.0:9100"
database_url: "postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres"
qdrant_url: "http://REDACTED_HADES_IP:6334"
embed:
  ollama_url: "http://localhost:11434"
  model: "qwen3-embedding"
search:
  rrf_k: 60.0
  context_token_budget: 32000
  context_fill_ratio: 0.8
```

## Acceptance Criteria

- [ ] `cargo build --release -p muninn` compiles with zero errors and zero warnings
- [ ] `muninn --config configs/muninn/config.yaml` starts and logs "muninn ready on 0.0.0.0:9100"
- [ ] `GET /health` returns HTTP 200 with body `{}`
- [ ] `POST /api/v1/search` with `{"query": "handle request"}` returns a `SearchResponse` with non-empty `results` and non-empty `context` (assumes Huginn has indexed at least the yggdrasil workspace)
- [ ] Search results have `source: "fused"` indicating RRF was applied
- [ ] Search results are ordered by descending RRF score
- [ ] `POST /api/v1/search` with `{"query": ""}` returns HTTP 400 with `{"error": "query must not be empty"}`
- [ ] `POST /api/v1/search` with `{"query": "struct", "limit": 5}` returns at most 5 results
- [ ] `POST /api/v1/search` with `{"query": "fn main", "limit": 100}` clamps limit to 50 and returns at most 50 results
- [ ] `POST /api/v1/search` with `{"query": "function", "languages": ["rust"]}` returns only Rust chunks
- [ ] `GET /api/v1/stats` returns `StatsResponse` with correct `total_chunks`, `total_files`, and `languages` matching PostgreSQL data
- [ ] Context string in search response groups chunks by file path
- [ ] Context string orders chunks within a file by start_line ascending
- [ ] Context string includes file path headers (`// File: /path/to/file.rs`)
- [ ] Context string includes line range and chunk identity (`// Lines 42-67 (function: handle_request)`)
- [ ] Context string respects token budget (does not exceed 25,600 tokens estimated at 4 chars/token = 102,400 characters)
- [ ] Vector search and BM25 search execute concurrently (verified by tracing spans overlapping in time)
- [ ] Chunks are fetched in a single batch query (one `SELECT ... WHERE id = ANY(...)`, not N individual SELECTs)
- [ ] Stale Qdrant IDs (pointing to deleted chunks) produce a warning log, not an error
- [ ] Search latency P95 < 100ms excluding embedding time (measured over 50 sequential requests)
- [ ] Context assembly P95 < 20ms (measured over 50 sequential requests)
- [ ] Memory stays below 50MB RSS at steady state
- [ ] Muninn is strictly read-only: no INSERT, UPDATE, or DELETE queries are ever executed
- [ ] CORS headers are present on all responses
- [ ] Graceful shutdown on SIGTERM/SIGINT completes in-flight requests before exiting
- [ ] Config loads from `configs/muninn/config.yaml` by default, overridable via `--config` flag
- [ ] Listen address can be overridden via `--listen-addr` CLI flag or `MUNINN_LISTEN_ADDR` env var

## Dependencies

| Dependency | Type | Status | Blocking? |
|------------|------|--------|-----------|
| Sprint 001 (Foundation) | Sprint | DONE | No |
| Sprint 002 (Mimir MVP) | Sprint | DONE | No |
| Sprint 003 (Huginn MVP) | Sprint | PLANNING (must be DONE to have indexed data to search) | Yes -- Muninn has no data without Huginn |
| Migration 002 (index metadata) | Database | DONE -- tables exist on Hades | No |
| Hades PostgreSQL | Infrastructure | Running | Yes -- required at runtime |
| Hades Qdrant | Infrastructure | Status uncertain (NetworkHardware.md: "no idea if it's configured yet") | Yes -- must be verified by `infra-devops` |
| Hugin Ollama (qwen3-embedding) | Infrastructure | Must be verified | Yes -- `infra-devops` must ensure model is pulled on Hugin |
| `ygg_domain` crate | Code | Complete (SearchQuery, SearchResponse, SearchResult, MuninnConfig all exist) | No |
| `ygg_store` crate | Code | Needs 4 new functions: `get_chunks_by_ids`, `count_chunks`, `count_indexed_files`, `count_chunks_by_language` | No -- `core-executor` adds them |
| `ygg_embed` crate | Code | Complete | No |
| Muninn Cargo.toml | Code | Already has all needed deps (axum, tower, tower-http, tokio, serde, sqlx via ygg-store, etc.) | No |

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Qdrant on Hades not yet configured | Muninn cannot perform vector search | Medium (NetworkHardware.md says "no idea if it's configured yet") | `infra-devops` must verify Qdrant is running and reachable at `http://REDACTED_HADES_IP:6334` from Hugin before integration testing. Muninn should log a clear error on startup if Qdrant is unreachable and exit with code 1. |
| Huginn has not indexed any data | Muninn search returns empty results | High (Sprint 003 is still PLANNING) | Muninn compiles and passes unit tests (fusion, assembler) independently of indexed data. Integration testing requires Sprint 003 to be DONE. `core-executor` should test fusion and assembler with synthetic data in unit tests. |
| Stale vector index after Huginn re-indexes | Qdrant returns a UUID that no longer exists in PostgreSQL | Low (race window during re-indexing) | `hybrid_search()` silently skips missing chunks with a warning log. The result count may be less than `limit` in this case. Acceptable for MVP. |
| Ollama qwen3-embedding model not pulled on Hugin | Query embedding fails with HTTP error | Medium | `infra-devops` must verify. Muninn's `EmbedClient` already returns `EmbedError::Http` which becomes a 500 to the caller. Error message will indicate the cause. |
| BM25 returns no results for code-specific queries | Queries like "impl VectorStore" may not match tsvector well | Medium (tsvector uses English stemmer which may mangle code tokens) | RRF handles this gracefully: if BM25 returns nothing, fused results come entirely from vector search. The hybrid approach is specifically designed for this -- vector search compensates for BM25 weakness on non-natural-language queries. |
| Large result set causes slow batch fetch | Fetching 50 chunks with their full `content` could be slow over network to Hades | Low (each chunk is typically < 5KB, so 50 chunks is < 250KB) | Batch fetch uses a single SQL roundtrip. P95 target of < 50ms is conservative. If exceeded, reduce the max limit from 50 to 30. |
| Co-location with Huginn on Hugin causes resource contention | Huginn indexing during Muninn search increases latency | Low (Huginn is bursty during file saves; Muninn is bursty during queries) | Monitor with tracing spans. If contention is observed, reduce Huginn's parse semaphore from 8 to 4. Hugin's 64GB RAM and 8C/16T make contention unlikely. |
| Context assembly produces too much text for LLM context window | LLM cannot process the assembled context | Low (token budget is explicitly enforced) | The `context_fill_ratio` of 0.8 leaves 20% headroom for the system prompt and user query. Adjustable via config. |
| Language filter on SearchQuery uses enum but BM25 expects strings | Type mismatch between `Language` enum and `&[String]` in `search_bm25()` | None (design accounts for this) | Handler converts `Vec<Language>` to `Vec<String>` via `Language::as_str().to_string()` before passing to search. |

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | Use Reciprocal Rank Fusion (RRF) with k=60, not weighted score combination | RRF is rank-based, not score-based, so it does not require normalizing the incompatible score ranges of Qdrant cosine similarity (0.0-1.0) and PostgreSQL ts_rank (unbounded). k=60 is the standard value from the original RRF paper (Cormack, Clarke, Buettcher 2009) and works well without tuning. |
| 2026-03-09 | Fetch 3x limit candidates from each search backend before fusion | RRF works best when both rankings have sufficient overlap. If we only fetch `limit` from each, the fused list may have fewer than `limit` results (since some IDs appear in only one ranking). 3x provides ample candidates while keeping the query cost bounded. |
| 2026-03-09 | Batch fetch chunks via `WHERE id = ANY($1)` instead of individual `get_chunk()` calls | Reduces PostgreSQL roundtrips from N to 1. Over the network to Hades (REDACTED_HADES_IP), each roundtrip adds ~1ms latency. For 50 chunks, that is 50ms of pure network overhead avoided. |
| 2026-03-09 | Context assembly uses 4 chars/token estimate, not a tokenizer | Running a real tokenizer (tiktoken, sentencepiece) adds a dependency and CPU cost for minimal accuracy gain. The 4 chars/token heuristic is conservative for code (which tends to have shorter tokens than English prose). The 80% fill ratio provides additional safety margin. |
| 2026-03-09 | Context groups chunks by file path, ordered by line number | Presenting chunks from the same file together with line continuity gives the LLM spatial awareness of the codebase. Interleaving chunks from different files would lose this context. |
| 2026-03-09 | File groups ordered by highest-scoring chunk in group | Ensures the most relevant file appears first in the context, maximizing the chance that the LLM attends to it (attention bias toward beginning of context in transformer models). |
| 2026-03-09 | Muninn is strictly read-only | Clean separation of concerns: Huginn writes, Muninn reads. This simplifies Muninn's error handling (no write conflicts), makes it safe to restart without data loss risk, and allows multiple Muninn instances in the future without coordination. |
| 2026-03-09 | Port 9100 for Muninn (Mimir uses 9090) | Avoids port collision with Mimir on the same subnet. Mimir runs on Munin (REDACTED_MUNIN_IP), but using distinct ports across services prevents confusion and allows co-location if needed. |
| 2026-03-09 | Define StatsResponse locally in Muninn, not in ygg_domain | Stats are Muninn-specific (total_chunks, total_files, languages). Other services have different stats needs. Putting it in ygg_domain would pollute the shared domain with retrieval-specific types. If Odin needs stats from Muninn, it will deserialize the JSON directly. |
| 2026-03-09 | No query caching in MVP | Caching adds complexity (invalidation on re-index, memory management). Muninn's search latency target of < 100ms P95 is fast enough for interactive use. Caching can be added in a future sprint if profiling shows repeated identical queries. |
| 2026-03-09 | Silently skip stale Qdrant IDs instead of erroring | During Huginn re-indexing, there is a brief window where Qdrant has a point ID that PostgreSQL has deleted. This is a benign race condition. Erroring would cause search failures during normal indexing operations. Logging a warning provides visibility without impacting the user. |
| 2026-03-09 | Stop context assembly on first chunk that exceeds budget (no knapsack optimization) | Knapsack packing (finding the optimal subset of chunks that fits the budget) is NP-hard and adds complexity. Since chunks are already sorted by relevance, the greedy approach (add in order, stop when full) produces a good-enough result. The most relevant chunks are included first, which is the desired behavior. |

---

**Next agent:** `core-executor` -- implement all files listed in the File-Level Implementation Plan. Execution order:

1. Add `get_chunks_by_ids()`, `count_chunks()`, `count_indexed_files()`, `count_chunks_by_language()` to `crates/ygg-store/src/postgres/chunks.rs`.
2. Create `crates/muninn/src/error.rs`.
3. Create `crates/muninn/src/state.rs`.
4. Create `crates/muninn/src/fusion.rs`.
5. Create `crates/muninn/src/assembler.rs`.
6. Create `crates/muninn/src/search.rs`.
7. Create `crates/muninn/src/handlers.rs`.
8. Replace `crates/muninn/src/main.rs` (remove CLI skeleton, build full Axum server).
9. Create `configs/muninn/config.yaml`.
10. Verify compilation: `cargo build --release -p muninn`.

**Dependency on Sprint 003:** Muninn compiles and its pure-function modules (fusion, assembler) can be unit-tested without indexed data. However, integration testing of the `/api/v1/search` endpoint requires Huginn to have indexed at least one repository. The `core-executor` should write unit tests for `fusion.rs` and `assembler.rs` with synthetic data, and defer integration tests until Sprint 003 is DONE.

**Blocker check for `infra-devops`:** Before `core-executor` can integration-test, `infra-devops` must verify:
1. Qdrant is running and reachable at `http://REDACTED_HADES_IP:6334` from Hugin (REDACTED_HUGIN_IP)
2. `qwen3-embedding` model is pulled on Hugin: `ollama list | grep qwen3-embedding`
3. PostgreSQL on Hades accepts connections from Hugin's IP (REDACTED_HUGIN_IP)
