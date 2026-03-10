# Sprint: 002 - Mimir MVP (Engram Memory Service)
## Status: PLANNING

## Objective

Build the Mimir Engram memory service as an Axum HTTP server that implements the Fergus `engram_client.rs` API contract. Mimir is the sole write path and primary query path for cause-effect engram memory. On every store request, it embeds the cause text via Ollama, deduplicates via SHA-256 hash, persists the engram to both PostgreSQL (pgvector) and Qdrant, and inserts into an in-memory LSH index for fast approximate pre-filtering. On every query request, it embeds the query text, searches Qdrant for top-N results, enriches those results with metadata from PostgreSQL, and returns them with similarity scores. The server must be wire-compatible with the existing Fergus `EngramClient` -- no client-side changes required.

## Scope

### In Scope
- Axum HTTP server on configurable port (default 9090)
- `GET /health` -- returns 200 OK with no body
- `POST /api/v1/store` -- accepts `{ "cause": "...", "effect": "..." }`, returns `{ "id": "uuid" }`
- `POST /api/v1/query` -- accepts `{ "text": "...", "limit": N }`, returns `Vec<{ id, cause, effect, similarity }>`
- `GET /api/v1/stats` -- returns `{ "core_count": N, "recall_count": N, "archival_count": N }`
- `POST /api/v1/promote` -- accepts `{ "id": "uuid", "tier": "core"|"recall"|"archival" }`, returns 200
- Startup: connect to Hades PostgreSQL, connect to Hades Qdrant, ensure `engrams` collection exists
- Startup: run migrations via `ygg_store::Store::migrate()`
- YAML config loading via `MimirConfig` from `ygg_domain::config`
- SHA-256 dedup on `cause + effect` concatenation before insert
- In-memory LSH index backed by `yggdrasil.lsh_buckets` table (loaded on startup, updated on store)
- Graceful error handling -- JSON error responses, never panic on bad input
- `tower-http` CORS layer for cross-origin requests from any frontend
- Structured tracing via `tracing` and `tracing-subscriber`

### Out of Scope
- Tier lifecycle automation (auto-promotion/archival based on access_count -- Sprint 003+)
- Summarization pipeline for archival tier (Sprint 003+)
- Authentication / authorization (no auth in MVP)
- TLS termination (handled by reverse proxy)
- Horizontal scaling / multi-instance coordination
- WebSocket streaming
- Prometheus metrics endpoint (Sprint 003+)
- Circuit breaker on the Mimir side (the Fergus client already implements this)
- Batch store endpoint

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** I/O-bound (network calls to Ollama for embedding, network calls to Qdrant/PostgreSQL for storage/retrieval). CPU-light except for LSH hashing.
- **Target Hardware:** Munin -- Intel Core Ultra 185H (6P+8E+2LP cores, 16 threads), 48GB DDR5, 2x 5Gb Ethernet, IP REDACTED_MUNIN_IP
- **Backend Services:**
  - Hades PostgreSQL: `postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres` (Intel N150, 32GB RAM, SATA SSD pool "Merlin")
  - Hades Qdrant: `http://REDACTED_HADES_IP:6334` (gRPC, same host as PostgreSQL)
  - Local Ollama: `http://localhost:11434` (qwen3-embedding model, runs on Munin ARC iGPU)
- **Utilization Plan:**
  - Tokio runtime with default multi-threaded scheduler. The 185H has 16 hardware threads; the async runtime will naturally spread I/O-bound work across available cores. No manual thread pinning needed for an I/O-bound service.
  - SQLx connection pool: 8 connections (half of hardware threads -- appropriate for an N150 backend CPU that will bottleneck before Munin does).
  - Qdrant client: single gRPC channel with default connection pooling (Qdrant client manages its own multiplexing).
  - In-memory LSH: `DashMap` for concurrent read/write. With 16 hash tables and 8 hash bits per table, the index is sharded across DashMap segments for lock-free reads.
  - Embedding: single-threaded per request (serialized through Ollama HTTP call). No parallelism needed -- Ollama handles its own GPU scheduling.
- **Fallback Strategy:** The service is I/O-bound and uses only async I/O; it will run identically on any machine with at least 2 cores and 1GB RAM. No SIMD, no GPU, no hardware-specific code paths.

## Performance Targets

| Metric | Target | Measurement Method |
|--------|--------|--------------------|
| Query P50 latency (excl. embedding) | < 15ms | `tracing` span timing on the query handler after embedding returns |
| Query P95 latency (excl. embedding) | < 50ms | Same span, P95 over 1-minute window |
| Store P50 latency (excl. embedding) | < 30ms | `tracing` span timing on the store handler after embedding returns |
| Store P95 latency (excl. embedding) | < 100ms | Same span, P95 over 1-minute window |
| Health check latency | < 2ms | Direct measurement, no DB call |
| Startup time (cold) | < 3s | Wall clock from process start to "listening on" log line (excl. LSH backfill) |
| LSH backfill time | < 5s for 10,000 engrams | Timed during startup from `lsh_buckets` table load |
| Memory ceiling (idle) | < 50MB RSS | Measured via `/proc/self/status` or `top` |
| Memory ceiling (10k engrams in LSH) | < 200MB RSS | Same |

## Data Schemas

### PostgreSQL Tables (Already Exist -- Migration 001)

**`yggdrasil.engrams`**
| Column | Type | Constraints |
|--------|------|-------------|
| `id` | `UUID` | PRIMARY KEY, DEFAULT gen_random_uuid() |
| `cause` | `TEXT` | NOT NULL |
| `effect` | `TEXT` | NOT NULL |
| `cause_embedding` | `vector(1024)` | pgvector, 1024-dim (qwen3-embedding output) |
| `content_hash` | `BYTEA` | NOT NULL, UNIQUE (SHA-256 of `cause || effect`) |
| `tier` | `TEXT` | NOT NULL, DEFAULT 'recall' |
| `tags` | `TEXT[]` | DEFAULT '{}' |
| `access_count` | `BIGINT` | DEFAULT 0 |
| `last_accessed` | `TIMESTAMPTZ` | DEFAULT NOW() |
| `created_at` | `TIMESTAMPTZ` | DEFAULT NOW() |
| `archived_by` | `UUID` | FK to engrams(id), nullable |
| `summary_of` | `UUID[]` | DEFAULT '{}' |

**`yggdrasil.lsh_buckets`**
| Column | Type | Constraints |
|--------|------|-------------|
| `table_idx` | `SMALLINT` | NOT NULL, part of composite PK |
| `bucket_hash` | `BIGINT` | NOT NULL, part of composite PK |
| `engram_id` | `UUID` | NOT NULL, FK to engrams(id) ON DELETE CASCADE, part of composite PK |

### Qdrant Collection

**Collection name:** `engrams`
| Field | Type | Notes |
|-------|------|-------|
| Vector | `Vec<f32>` | 1024 dimensions, Cosine distance |
| Point ID | UUID string | Matches `engrams.id` in PostgreSQL |
| Payload | None stored | Mimir retrieves metadata from PostgreSQL after Qdrant returns IDs |

### Request/Response Schemas (JSON over HTTP)

**POST /api/v1/store -- Request**
```json
{
  "cause": "string (required)",
  "effect": "string (required)",
  "tags": ["string"] // optional, defaults to []
}
```
Maps directly to `ygg_domain::engram::NewEngram`.

**POST /api/v1/store -- Response (201 Created)**
```json
{
  "id": "uuid-string"
}
```
Maps directly to `ygg_domain::engram::StoreResponse`.

**POST /api/v1/store -- Response (409 Conflict, duplicate)**
```json
{
  "error": "engram with identical content already exists"
}
```

**POST /api/v1/query -- Request**
```json
{
  "text": "string (required)",
  "limit": 5 // optional, defaults to 5
}
```
Maps directly to `ygg_domain::engram::EngramQuery`.

**POST /api/v1/query -- Response (200 OK)**
```json
[
  {
    "id": "uuid-string",
    "cause": "string",
    "effect": "string",
    "similarity": 0.95
  }
]
```
Note: Returns a flat array (not wrapped in an object). The Fergus `EngramClient::query_inner()` deserializes as `Vec<Engram>` where `Engram` has fields `{ id, cause, effect, similarity }`. The full `ygg_domain::engram::Engram` struct has additional fields (`tier`, `tags`, `created_at`, `access_count`, `last_accessed`) which are serialized but ignored by the Fergus client via `#[serde(default)]` / missing-field tolerance. Return the full struct; the client will take what it needs.

**GET /api/v1/stats -- Response (200 OK)**
```json
{
  "core_count": 0,
  "recall_count": 42,
  "archival_count": 3
}
```
Maps directly to `ygg_domain::engram::MemoryStats`.

**POST /api/v1/promote -- Request**
```json
{
  "id": "uuid-string (required)",
  "tier": "core" | "recall" | "archival" // required
}
```

**POST /api/v1/promote -- Response (200 OK)**
Empty body, status 200.

**POST /api/v1/promote -- Response (404 Not Found)**
```json
{
  "error": "engram {id} not found"
}
```

**GET /health -- Response (200 OK)**
Empty body, status 200. No database check. This must be fast for load balancer probes.

**Error Response Format (all endpoints)**
```json
{
  "error": "human-readable error message"
}
```
Status codes: 400 (bad request / validation), 404 (not found), 409 (duplicate), 500 (internal).

## API Contracts

### HTTP Routes (Axum Router)

```
GET  /health           -> handlers::health()
POST /api/v1/store     -> handlers::store_engram(State, Json<NewEngram>)
POST /api/v1/query     -> handlers::query_engrams(State, Json<EngramQuery>)
GET  /api/v1/stats     -> handlers::get_stats(State)
POST /api/v1/promote   -> handlers::promote_engram(State, Json<PromoteRequest>)
```

### Internal Module Interfaces

**`state.rs` -- `AppState`**
```rust
pub struct AppState {
    pub store: ygg_store::Store,                    // PostgreSQL pool
    pub vectors: ygg_store::qdrant::VectorStore,    // Qdrant client
    pub embedder: ygg_embed::EmbedClient,           // Ollama embedding client
    pub lsh: LshIndex,                              // In-memory LSH index
    pub config: ygg_domain::config::MimirConfig,    // Loaded YAML config
}
```

**`lsh.rs` -- `LshIndex`**
```rust
pub struct LshIndex {
    tables: Vec<DashMap<u64, Vec<Uuid>>>,  // num_tables hash tables
    hyperplanes: Vec<Vec<Vec<f32>>>,       // [table_idx][bit_idx][dim] random hyperplanes
    num_tables: usize,
    hash_bits: usize,
}

impl LshIndex {
    pub fn new(num_tables: usize, hash_bits: usize, dim: usize) -> Self;
    pub fn insert(&self, id: Uuid, embedding: &[f32]);
    pub fn query(&self, embedding: &[f32], threshold: usize) -> Vec<Uuid>;
    pub fn remove(&self, id: Uuid, embedding: &[f32]);
    pub fn len(&self) -> usize;
}
```
The LSH index uses random hyperplane hashing (SimHash). Each table independently hashes a vector into a `hash_bits`-bit bucket by computing the sign of dot products with random hyperplanes. `query()` returns UUIDs that appear in at least `threshold` of the `num_tables` tables for the given query bucket.

**`error.rs` -- `MimirError`**
```rust
pub enum MimirError {
    Store(ygg_store::error::StoreError),
    Embed(ygg_embed::EmbedError),
    Config(String),
    Validation(String),
}
```
Implements `IntoResponse` for Axum, mapping each variant to the appropriate HTTP status code and JSON error body.

## Interface Boundaries

| Module | Owns | Exposes | Depends On |
|--------|------|---------|------------|
| `mimir::main` | Process lifecycle, CLI parsing, config loading, server startup/shutdown | Nothing (binary entrypoint) | `state`, `handlers`, `error`, `ygg_domain::config` |
| `mimir::state` | `AppState` construction, connection initialization, collection bootstrapping | `AppState` struct, `AppState::new()` async constructor | `ygg_store::Store`, `ygg_store::qdrant::VectorStore`, `ygg_embed::EmbedClient`, `mimir::lsh::LshIndex` |
| `mimir::handlers` | HTTP request/response mapping, business logic orchestration | Five handler functions (see API Contracts) | `mimir::state::AppState`, `mimir::error::MimirError`, `ygg_domain::engram::*`, `ygg_store::postgres::engrams::*` |
| `mimir::lsh` | In-memory LSH index data structure, hyperplane generation, bucket hashing | `LshIndex` struct and its methods | `dashmap`, `uuid` |
| `mimir::error` | Error type unification, HTTP status code mapping | `MimirError` enum, `IntoResponse` impl | `ygg_store::error::StoreError`, `ygg_embed::EmbedError`, `axum::response` |
| `ygg_store` (external) | PostgreSQL connection pool, all SQL execution | `Store`, `postgres::engrams::*`, `qdrant::VectorStore` | `sqlx`, `qdrant-client`, `ygg_domain` |
| `ygg_embed` (external) | Ollama HTTP communication, embedding serialization | `EmbedClient`, `embed_single()`, `embed_batch()` | `reqwest` |
| `ygg_domain` (external) | All type definitions, config structs, domain errors | `engram::*`, `config::MimirConfig` | None (leaf crate) |

**Ownership rules:**
- Only `mimir::handlers` may call `ygg_store::postgres::engrams::*` functions. No other mimir module touches SQL.
- Only `mimir::handlers` may call `ygg_embed::EmbedClient`. No other mimir module triggers embedding.
- Only `mimir::lsh` may read/write the `DashMap` internals. Handlers interact via the `LshIndex` public API.
- `mimir::state` owns construction but does not own business logic. It is a data holder, not a service.

## File-Level Implementation Plan

### `crates/mimir/src/main.rs`
1. Parse CLI args (already done -- keep existing `Cli` struct).
2. Load `MimirConfig` from YAML file at `cli.config` path.
3. Override `database_url` from CLI if provided via `--database-url` or `MIMIR_DATABASE_URL` env.
4. Construct `AppState::new(config).await` -- this connects to PostgreSQL, Qdrant, ensures collection, loads LSH from DB.
5. Build Axum router with all routes and `tower_http::cors::CorsLayer::permissive()`.
6. Bind to `config.listen_addr` (default `0.0.0.0:9090`).
7. Log "mimir listening on {addr}" at INFO level.
8. `axum::serve()` with graceful shutdown on SIGTERM/SIGINT via `tokio::signal`.

### `crates/mimir/src/state.rs` (NEW)
1. Define `AppState` struct (see Interface Boundaries).
2. Implement `AppState::new(config: MimirConfig) -> Result<Self, MimirError>`:
   - `Store::connect(&config.database_url).await`
   - `Store::migrate("./migrations").await`
   - `VectorStore::connect(&config.qdrant_url).await`
   - `VectorStore::ensure_collection("engrams").await`
   - `EmbedClient::new(&config.embed.ollama_url, &config.embed.model)`
   - `LshIndex::new(config.lsh.num_tables, config.lsh.hash_bits, 1024)`
   - Load existing LSH buckets from `yggdrasil.lsh_buckets` and backfill the in-memory index.
3. The backfill query: `SELECT table_idx, bucket_hash, engram_id FROM yggdrasil.lsh_buckets` -- iterate rows and insert into `LshIndex.tables[table_idx][bucket_hash].push(engram_id)`.

### `crates/mimir/src/handlers.rs` (NEW)
1. **`health()`**: Return `StatusCode::OK` with empty body. No DB call.
2. **`store_engram(State(state), Json(body))`**:
   - Validate `cause` and `effect` are non-empty. Return 400 if empty.
   - Compute SHA-256 of `format!("{}{}", body.cause, body.effect)`.
   - Call `state.embedder.embed_single(&body.cause).await` to get the embedding vector.
   - Call `ygg_store::postgres::engrams::insert_engram(pool, &body.cause, &body.effect, &embedding, &hash, MemoryTier::Recall, &body.tags).await`.
   - On duplicate error (from UNIQUE constraint on content_hash): return 409 with error message.
   - Call `state.vectors.upsert("engrams", id, embedding.clone(), payload).await` where payload is an empty HashMap (metadata lives in PostgreSQL).
   - Insert into `state.lsh` index: `state.lsh.insert(id, &embedding)`.
   - Persist LSH buckets to `yggdrasil.lsh_buckets` for the new engram (fire-and-forget spawn, not in critical path).
   - Return 201 with `StoreResponse { id }`.
3. **`query_engrams(State(state), Json(body))`**:
   - Validate `text` is non-empty. Return 400 if empty.
   - Call `state.embedder.embed_single(&body.text).await` to get query embedding.
   - Call `state.vectors.search("engrams", embedding, body.limit as u64).await` to get `Vec<(Uuid, f32)>`.
   - For each returned UUID, call `ygg_store::postgres::engrams::get_engram(pool, id).await` and set `similarity` from the Qdrant score.
   - Bump access counts (already handled inside `query_by_similarity`, but since we are using Qdrant for the search and then individual gets, we need to explicitly bump access counts with a single UPDATE for all returned IDs).
   - Return 200 with `Vec<Engram>` (the full domain Engram, serialized as JSON).
4. **`get_stats(State(state))`**:
   - Call `ygg_store::postgres::engrams::get_stats(pool).await`.
   - Return 200 with `MemoryStats`.
5. **`promote_engram(State(state), Json(body))`**:
   - Parse `body.id` as UUID, parse `body.tier` as `MemoryTier`. Return 400 on invalid values.
   - Call `ygg_store::postgres::engrams::set_tier(pool, id, tier).await`.
   - On not-found: return 404.
   - Return 200 with empty body.

### `crates/mimir/src/lsh.rs` (NEW)
1. Define `LshIndex` struct with `Vec<DashMap<u64, Vec<Uuid>>>` for tables and `Vec<Vec<Vec<f32>>>` for hyperplanes.
2. `new()`: Generate random hyperplanes using a seeded RNG (use `rand` crate or manually generate from a deterministic seed for reproducibility). Each table has `hash_bits` hyperplanes, each hyperplane is a `Vec<f32>` of dimension 1024.
3. `hash()` (private): For a given table index and embedding, compute the `hash_bits`-bit hash by taking the sign of each dot product with the table's hyperplanes. Pack into a `u64`.
4. `insert()`: For each table, compute the hash, then push the UUID into `tables[table_idx].entry(hash).or_default().push(id)`.
5. `query()`: For each table, compute the hash, then collect all UUIDs from that bucket. Return UUIDs that appear in >= `threshold` tables.
6. `remove()`: For each table, compute the hash, then remove the UUID from the bucket vector.
7. `backfill_from_rows()`: Accept pre-computed `(table_idx, bucket_hash, engram_id)` rows and populate the DashMaps directly (no re-hashing needed -- the DB stores the computed hashes).
8. `export_buckets()`: For a given `(id, embedding)`, return the `Vec<(table_idx, bucket_hash, engram_id)>` tuples for DB persistence.
9. `len()`: Return total unique UUIDs across all tables (approximate, for stats/logging).

### `crates/mimir/src/error.rs` (NEW)
1. Define `MimirError` enum with variants: `Store(StoreError)`, `Embed(EmbedError)`, `Config(String)`, `Validation(String)`.
2. Implement `From<StoreError>` and `From<EmbedError>`.
3. Implement `IntoResponse` for Axum:
   - `Store(StoreError::Duplicate(_))` -> 409
   - `Store(StoreError::NotFound(_))` -> 404
   - `Store(_)` -> 500
   - `Embed(_)` -> 502 (bad gateway -- upstream Ollama failed)
   - `Config(_)` -> 500
   - `Validation(_)` -> 400
4. Response body: `{ "error": "{self}" }` using the `Display` impl from `thiserror`.

### `configs/mimir/config.yaml` (NEW)
```yaml
listen_addr: "0.0.0.0:9090"
database_url: "postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres"
qdrant_url: "http://REDACTED_HADES_IP:6334"
embed:
  ollama_url: "http://localhost:11434"
  model: "qwen3-embedding"
lsh:
  num_tables: 16
  hash_bits: 8
tiers:
  recall_capacity: 1000
  summarization_batch_size: 100
```

### `crates/mimir/Cargo.toml` (MODIFY)
Add missing dependencies:
- `dashmap = { workspace = true }` -- for LSH concurrent hash maps
- `rand` (not in workspace yet) -- for hyperplane generation. If avoiding a new workspace dep, use a deterministic PRNG seeded from a fixed value, implemented manually with xorshift64 or similar. The `core-executor` agent should decide the simplest approach.

### No New Migrations Required
The `yggdrasil.engrams` and `yggdrasil.lsh_buckets` tables already exist from migration 001. No schema changes needed for this sprint.

## Acceptance Criteria

- [ ] `cargo build --release -p mimir` compiles with zero warnings
- [ ] `mimir` starts and logs "mimir listening on 0.0.0.0:9090" within 3 seconds
- [ ] `mimir` connects to Hades PostgreSQL on startup without error
- [ ] `mimir` connects to Hades Qdrant on startup and creates `engrams` collection if missing
- [ ] `mimir` runs migrations from `./migrations/` on startup
- [ ] `GET /health` returns 200 with latency < 2ms
- [ ] `POST /api/v1/store` with valid `{ "cause": "...", "effect": "..." }` returns 201 with `{ "id": "uuid" }`
- [ ] Stored engram exists in both `yggdrasil.engrams` (PostgreSQL) and `engrams` collection (Qdrant)
- [ ] Storing the same cause+effect twice returns 409 Conflict (SHA-256 dedup)
- [ ] `POST /api/v1/query` with `{ "text": "...", "limit": 5 }` returns array of engrams with `similarity` scores
- [ ] Query results are ordered by similarity descending
- [ ] `GET /api/v1/stats` returns correct tier counts matching database state
- [ ] `POST /api/v1/promote` with `{ "id": "valid-uuid", "tier": "core" }` returns 200 and updates the tier in PostgreSQL
- [ ] `POST /api/v1/promote` with non-existent UUID returns 404
- [ ] Empty or missing `cause`/`effect`/`text` fields return 400 with JSON error body
- [ ] Malformed JSON body returns 400, not 500
- [ ] LSH index is loaded from `lsh_buckets` table on startup
- [ ] LSH index is updated on every successful store operation
- [ ] Query P95 latency < 50ms (excluding embedding generation time)
- [ ] Store P95 latency < 100ms (excluding embedding generation time)
- [ ] Process RSS < 50MB idle, < 200MB with 10k engrams in LSH
- [ ] Config loads from `configs/mimir/config.yaml` by default, overridable via `--config` flag
- [ ] `database_url` is overridable via `--database-url` CLI arg or `MIMIR_DATABASE_URL` env var
- [ ] CORS headers present on all responses (permissive policy)
- [ ] Graceful shutdown on SIGTERM -- in-flight requests complete before exit

## Dependencies

| Dependency | Type | Status | Blocking? |
|------------|------|--------|-----------|
| Sprint 001 (Foundation) | Sprint | DONE | No -- all 9 crates compile clean |
| Migration 001 (engram schema) | Database | DONE | No -- tables exist on Hades |
| Migration 002 (index metadata) | Database | DONE | No |
| Hades PostgreSQL | Infrastructure | Running | Yes -- required at runtime |
| Hades Qdrant | Infrastructure | Running | Yes -- required at runtime |
| Munin Ollama (qwen3-embedding) | Infrastructure | Running | Yes -- required for store/query |
| `ygg_domain` crate | Code | Complete | No |
| `ygg_store` crate | Code | Complete | No |
| `ygg_embed` crate | Code | Complete | No |
| `dashmap` workspace dep | Code | Available | No -- already in `Cargo.toml` workspace deps |
| `rand` or equivalent PRNG | Code | Not in workspace | No -- `core-executor` adds to mimir's `Cargo.toml` |

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Qdrant on Hades not yet configured | Mimir cannot start | Medium (NetworkHardware.md says "no idea if it's configured yet") | `infra-devops` agent must verify Qdrant is running on `REDACTED_HADES_IP:6334` before `core-executor` deploys. If not running, `infra-devops` starts the container and verifies gRPC connectivity. |
| Ollama qwen3-embedding model not pulled on Munin | Store/query fail with 404 from Ollama | Low | Startup should log a warning if `embed_single("test")` fails. `infra-devops` ensures model is pulled: `ollama pull qwen3-embedding` on Munin. |
| LSH backfill on large datasets blocks startup | Slow startup, potential timeouts | Low (MVP starts with empty DB) | Backfill runs after the HTTP server is bound. The server starts accepting requests immediately; LSH queries return empty results until backfill completes. Log progress. |
| pgvector IVFFlat index on empty table | IVFFlat requires data to be effective; queries on near-empty tables may not use the index | Low (correct but slow) | Acceptable for MVP. The index will become effective as data accumulates. HNSW migration can be considered in Sprint 003 if IVFFlat performance degrades. |
| `rand` crate not in workspace dependencies | Compilation failure if `core-executor` adds it without updating workspace `Cargo.toml` | Low | Sprint doc explicitly calls out this dependency. `core-executor` must add `rand = "0.8"` to `[workspace.dependencies]` and to `crates/mimir/Cargo.toml`. Alternatively, implement a simple xorshift64 PRNG to avoid the dep entirely. |
| SHA-256 hash collision | Two different engrams rejected as duplicates | Negligible (2^-256 probability) | Not mitigated. Accept the theoretical risk. |
| Fergus client expects flat array from /api/v1/query | If response is wrapped in an object, deserialization fails silently | High if wrong | Sprint doc specifies flat `Vec<Engram>` serialization. Acceptance criteria explicitly tests this. |

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | Use Qdrant as primary vector search, PostgreSQL pgvector as backup/audit store | Qdrant is purpose-built for ANN search with better throughput than pgvector IVFFlat. PostgreSQL remains the CRUD source of truth for metadata. Dual-write ensures no data loss if Qdrant is rebuilt. |
| 2026-03-09 | LSH index is in-memory with DB-backed persistence (not purely in-DB) | In-memory DashMap gives sub-microsecond bucket lookups. DB backing ensures index survives restarts without re-embedding all engrams. |
| 2026-03-09 | No authentication in MVP | Mimir runs on a private LAN (10.0.x.x). Auth adds complexity with no security benefit on a trusted network. Revisit if exposed externally. |
| 2026-03-09 | Return full `Engram` struct from /api/v1/query, not a stripped-down DTO | The Fergus client uses `#[serde(default)]` and ignores unknown fields. Returning the full struct means Mimir does not need a separate DTO, and future clients get richer data for free. |
| 2026-03-09 | Embed `cause` only (not `cause + effect`) for vector similarity | The cause is the "what happened" -- it is the natural query target. The effect is the "what resulted" -- it is the answer. Embedding cause aligns with how the Fergus client queries: it sends the user's message as query text, which maps to a cause. |
| 2026-03-09 | SHA-256 dedup on `cause + effect` concatenation | Dedup must consider both fields. Two engrams with the same cause but different effects are distinct memories. |
| 2026-03-09 | Use SimHash (random hyperplane) for LSH, not MinHash | SimHash works directly on dense float vectors (cosine similarity). MinHash is designed for set similarity (Jaccard) and would require converting embeddings to binary sketches, adding unnecessary complexity. |
| 2026-03-09 | Start HTTP server before LSH backfill completes | Prevents slow startup from blocking health checks and readiness probes. The LSH is an optimization layer, not a correctness requirement -- Qdrant handles the actual ANN search. |

---

**Next agent:** `core-executor` -- implement all files listed in the File-Level Implementation Plan. Begin with `error.rs` and `lsh.rs` (no external dependencies), then `state.rs`, then `handlers.rs`, and finally update `main.rs`. Create `configs/mimir/config.yaml`. Verify compilation with `cargo build --release -p mimir`.

**Blocker check for `infra-devops`:** Before `core-executor` can integration-test, `infra-devops` must verify:
1. Qdrant is running and reachable at `http://REDACTED_HADES_IP:6334`
2. `qwen3-embedding` model is pulled on Munin (`ollama list` on REDACTED_MUNIN_IP)
