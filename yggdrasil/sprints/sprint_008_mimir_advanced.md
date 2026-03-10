# Sprint: 008 - Mimir Advanced Memory Management
## Status: IMPLEMENTED

## Objective

Implement Mimir's advanced three-tier memory lifecycle: hierarchical summarization of aging Recall engrams into Archival tier summaries, manual and automatic promotion of important engrams to the Core tier (always included in query results), and a sliding-window eviction policy that prevents unbounded Recall tier growth. A background Tokio task within Mimir periodically evaluates Recall tier occupancy against the configured `recall_capacity` threshold and, when exceeded, batches the oldest/lowest-access engrams, calls Odin's coding model to produce a consolidated summary, stores the summary as a new Archival tier engram linking back to its source engrams, and archives the originals. Core tier engrams bypass vector similarity search entirely -- they are always prepended to query results. Two new HTTP endpoints (`POST /api/v1/promote`, `GET /api/v1/core`) and a new `GET /api/v1/stats` field complete the API surface.

## Scope

### In Scope
- Background summarization task in Mimir: periodically checks Recall tier count against `recall_capacity`
- When Recall tier exceeds capacity: select oldest `summarization_batch_size` engrams (by `created_at ASC` then `access_count ASC`), call Odin for summarization, store summary as Archival engram, mark originals as archived
- New `SummarizationService` module in `crates/mimir/src/summarization.rs`
- Odin HTTP call from Mimir for summarization (POST `/v1/chat/completions` with non-streaming, coding model)
- New `SummarizationConfig` fields in `MimirConfig` and `TierConfig` for tuning
- Core tier: engrams marked `tier = 'core'` are always included in query results regardless of vector similarity
- Modify `query_engrams` handler to prepend Core tier engrams to every query result
- Verify existing `POST /api/v1/promote` endpoint (already exists in Mimir main.rs and handlers.rs)
- New `GET /api/v1/core` endpoint: list all Core tier engrams
- Update `GET /api/v1/stats` to include `oldest_recall_created_at` and `recall_capacity` fields
- New store functions: `get_oldest_recall_engrams()`, `get_core_engrams()`, `archive_engrams()`, `get_engrams_batch()`
- Update `MemoryStats` in `ygg-domain` with additional fields
- Archival engram links to source engrams via `summary_of UUID[]` column (already exists in migration 001)
- Archived originals set `archived_by` to the new summary engram's UUID (column already exists in migration 001)
- Remove archived engrams from Qdrant `engrams` collection and LSH index
- Configuration: summarization check interval, minimum age before eligible, Odin URL for summarization calls

### Out of Scope
- Automatic Core tier promotion (manual only via `POST /api/v1/promote` in this sprint)
- Summarization of Archival engrams (single-level summarization only; recursive summarization is future work)
- Custom summarization models (uses the default coding model via Odin; model selection is Odin's responsibility)
- Streaming summarization responses (non-streaming only for simplicity)
- Exposing summarization status or progress via API
- Modifying the Fergus client contract (`POST /api/v1/query` response shape unchanged)
- Any changes to Huginn, Muninn, or MCP server
- Database migration changes (all required columns already exist in migration 001: `archived_by`, `summary_of`)

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** Mixed (I/O-bound for database queries and HTTP calls, GPU-bound for LLM summarization via Odin).
- **Target Hardware:** Munin (REDACTED_MUNIN_IP) -- Intel Core Ultra 185H (6P+8E+2LP cores, 16 threads), 48GB DDR5, Intel ARC iGPU.
- **Utilization Plan:**
  - Background summarization runs on a single Tokio task. It is not CPU-intensive -- it spends most time waiting for the Odin summarization HTTP call to return. The task runs every `check_interval_secs` (default 300s / 5 minutes) and processes one batch per cycle.
  - Each summarization batch fetches `summarization_batch_size` (default 100) engrams from PostgreSQL, concatenates their cause+effect text (~50KB for 100 engrams), sends a single LLM call to Odin, and stores the summary. Database I/O is minimal (2 SELECT + 1 INSERT + 1 batch UPDATE + batch Qdrant delete + LSH remove).
  - The LLM summarization call goes through Odin, which routes it to the coding model on Munin (qwen3-coder-30b-a3b). This competes for the backend semaphore (max_concurrent: 2) with user chat requests. Summarization uses a single slot and can be deprioritized (see fallback strategy).
  - Core tier query injection adds a single SQL query (`SELECT * FROM engrams WHERE tier = 'core'`) to every `/api/v1/query` call. With < 50 Core engrams expected, this is < 5ms additional latency.
  - Memory impact: the summarization task holds one batch of engrams in memory (~50KB for 100 engrams) plus the summarization prompt (~60KB with system prompt). Peak additional RSS < 2MB.
- **Fallback Strategy:**
  - If Odin is unreachable, the summarization task logs a warning and retries on the next cycle. No engrams are archived without a successful summary.
  - If the Odin backend semaphore is full (all slots occupied by user requests), the summarization HTTP call blocks waiting for a slot. This is acceptable because summarization runs in the background and is latency-insensitive. If this becomes a problem, Odin can add a low-priority queue in a future sprint.
  - If PostgreSQL is slow, the batch size can be reduced via config. Processing 10 engrams per batch is still effective at steady state.

## Performance Targets

| Metric | Target | Measurement Method |
|--------|--------|--------------------|
| `query_engrams` P95 with Core injection | < 60ms (was < 50ms without Core) | `tracing` span at handler level |
| Core tier SELECT P95 | < 10ms for <= 50 Core engrams | `tracing` span on SQL query |
| `GET /api/v1/core` P95 | < 20ms | `tracing` span at handler level |
| Summarization batch: DB operations | < 500ms for 100-engram batch | `tracing` span on summarization cycle |
| Summarization batch: LLM call | 10-60s (model-dependent) | `tracing` span on Odin HTTP call |
| Summarization batch: total cycle | < 90s for 100-engram batch | `tracing` span on full cycle |
| Memory overhead of summarization task | < 2MB additional RSS | `/proc/self/status` VmRSS comparison |
| Core engram count (expected) | < 50 | `GET /api/v1/stats` |
| Recall tier never exceeds | `recall_capacity` + `summarization_batch_size` | `GET /api/v1/stats` |

## Data Schemas

### Updated `MemoryStats` (in `ygg-domain/src/engram.rs`)

```rust
/// Memory system statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub core_count: i64,
    pub recall_count: i64,
    pub archival_count: i64,
    /// Configured maximum Recall tier capacity.
    pub recall_capacity: i64,
    /// Timestamp of the oldest engram in Recall tier (None if empty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_recall_at: Option<DateTime<Utc>>,
}
```

### New `SummarizationConfig` fields (in `ygg-domain/src/config.rs`)

Extend existing `TierConfig`:

```rust
/// Memory tier capacity configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierConfig {
    /// Maximum number of Recall tier engrams before summarization triggers.
    #[serde(default = "default_recall_capacity")]
    pub recall_capacity: usize,
    /// Number of engrams to batch for each summarization cycle.
    #[serde(default = "default_summarization_batch")]
    pub summarization_batch_size: usize,
    /// How often to check Recall tier capacity (seconds).
    #[serde(default = "default_check_interval")]
    pub check_interval_secs: u64,
    /// Minimum age (seconds) before a Recall engram is eligible for summarization.
    /// Prevents summarizing very recent engrams that may still be actively accessed.
    #[serde(default = "default_min_age_secs")]
    pub min_age_secs: u64,
    /// Odin URL for summarization LLM calls.
    #[serde(default = "default_odin_url")]
    pub odin_url: String,
}

fn default_check_interval() -> u64 {
    300 // 5 minutes
}

fn default_min_age_secs() -> u64 {
    86400 // 24 hours
}

fn default_odin_url() -> String {
    "http://localhost:8080".to_string()
}
```

### Summarization prompt

System prompt sent to Odin for summarization:

```
You are a memory consolidation system. You receive a batch of cause-effect memory pairs and must produce a single consolidated summary that preserves all important information.

## Rules
- Preserve key facts, decisions, and lessons learned
- Merge duplicate or overlapping information
- Maintain cause-effect relationships where meaningful
- Output a single cause-effect pair where:
  - "cause" is a concise description of the topics and contexts covered
  - "effect" is the consolidated knowledge and outcomes
- Keep the total output under 2000 characters
- Do not add information not present in the source memories
- Output ONLY valid JSON: {"cause": "...", "effect": "..."}
```

User prompt:

```
Consolidate these {count} memories into a single summary:

{formatted_engrams}
```

Where `formatted_engrams` is:

```
1. Cause: {cause}
   Effect: {effect}
   Created: {created_at}

2. Cause: {cause}
   Effect: {effect}
   Created: {created_at}

...
```

### Summarization LLM request (to Odin)

```json
{
  "model": null,
  "messages": [
    { "role": "system", "content": "<system prompt above>" },
    { "role": "user", "content": "<user prompt above>" }
  ],
  "stream": false,
  "max_tokens": 2048,
  "temperature": 0.3
}
```

Response parsing: extract JSON `{"cause": "...", "effect": "..."}` from `choices[0].message.content`. If the model wraps it in a code fence, strip the fence. If parsing fails, use the entire response content as the `effect` and set `cause` to `"Consolidated summary of {count} memories from {start_date} to {end_date}"`.

### Archival engram storage

When a batch of Recall engrams is summarized:

1. Store the summary as a new engram:
   - `tier`: `archival`
   - `cause`: from LLM response
   - `effect`: from LLM response
   - `tags`: `["auto-summary", "batch-{batch_id}"]`
   - `summary_of`: `[uuid1, uuid2, ..., uuidN]` (IDs of the original engrams)
   - `content_hash`: SHA-256 of `cause + "\n" + effect` (same as normal store)
   - `cause_embedding`: embed the cause text (same as normal store)
   - Upsert into Qdrant `engrams` collection
   - Insert into LSH index

2. Update the original engrams:
   - `archived_by`: set to the new summary engram's UUID
   - `tier`: change from `recall` to `archival`

3. Remove original engrams from Qdrant (their vectors are no longer needed; the summary has its own vector):
   - Delete points by ID from `engrams` collection

4. Remove original engrams from LSH index:
   - Call `lsh.remove(id, embedding)` for each (requires fetching embeddings or re-computing hashes)
   - Optimization: since we are deleting from Qdrant anyway, we can skip LSH removal and let stale LSH entries exist harmlessly (they point to archived engrams that will be filtered by tier). Decision: skip LSH removal to avoid needing to re-embed or store embeddings in memory. Stale entries waste lookup time but are negligible with < 100 evictions per cycle.

### Updated config file `configs/mimir/config.yaml`

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
  check_interval_secs: 300
  min_age_secs: 86400
  odin_url: "http://localhost:8080"
```

## API Contracts

### Existing: `POST /api/v1/promote` (no changes, already implemented)

Request:
```json
{
  "id": "uuid",
  "tier": "core"
}
```

Response: `200 OK` (no body)

Error: `404` if engram not found.

### New: `GET /api/v1/core`

Returns all Core tier engrams. No pagination (Core tier is expected to be small, < 50 engrams).

Response `200`:
```json
[
  {
    "id": "uuid",
    "cause": "Rust async patterns",
    "effect": "Use tokio::spawn for CPU-bound work...",
    "similarity": 0.0,
    "tier": "core",
    "tags": ["rust", "async"],
    "created_at": "2026-03-09T10:30:00Z",
    "access_count": 42,
    "last_accessed": "2026-03-09T14:00:00Z"
  }
]
```

### Modified: `POST /api/v1/query` (behavior change, no schema change)

Behavior change: Core tier engrams are always prepended to query results.

Flow:
1. Embed query text (unchanged)
2. Search Qdrant for top-N nearest vectors (unchanged)
3. Fetch engram metadata from PostgreSQL (unchanged)
4. **NEW**: Fetch all Core tier engrams from PostgreSQL
5. **NEW**: Deduplicate -- if a Core engram was already returned by vector search, keep its similarity score from Qdrant and do not duplicate it
6. Prepend Core engrams (with `similarity: 1.0` marker) to the results
7. Bump access counts for all returned engrams (unchanged)
8. Return results (Core engrams first, then vector-similar engrams)

Note: The `limit` parameter applies only to the vector search portion. Core engrams are always included in addition to the limit. If there are 5 Core engrams and `limit=5`, the response will contain up to 10 engrams. This is intentional -- Core engrams are permanent context.

### Modified: `GET /api/v1/stats`

Response `200`:
```json
{
  "core_count": 3,
  "recall_count": 847,
  "archival_count": 215,
  "recall_capacity": 1000,
  "oldest_recall_at": "2026-02-15T08:00:00Z"
}
```

### Internal: Odin summarization call (Mimir -> Odin)

| Target | Endpoint | Method | Purpose |
|--------|----------|--------|---------|
| Odin (localhost:8080) | `POST /v1/chat/completions` | POST | Non-streaming summarization of engram batch |

Request matches the standard OpenAI chat completion format. Model is `null` (let Odin use default routing, which selects the coding model on Munin).

## Interface Boundaries

| Module | Owns | Exposes | Depends On |
|--------|------|---------|------------|
| `mimir::summarization` | Background summarization task lifecycle, batch selection logic, Odin HTTP call for summarization, summary parsing, archival workflow | `SummarizationService::new()`, `SummarizationService::start()` (spawns background task) | `ygg_store::postgres::engrams`, `ygg_store::qdrant::VectorStore`, `ygg_embed::EmbedClient`, `mimir::lsh::LshIndex`, `reqwest` (for Odin calls) |
| `mimir::handlers` (extended) | Core tier injection in query, new `/api/v1/core` handler, updated `/api/v1/stats` handler | `query_engrams()` (updated), `get_core_engrams()` (new), `get_stats()` (updated) | `ygg_store::postgres::engrams`, `mimir::state::AppState` |
| `mimir::state` (extended) | Stores shutdown signal for summarization task | `AppState` (unchanged struct, new field for shutdown handle) | `tokio::sync::watch` |
| `ygg_store::postgres::engrams` (extended) | New query functions for tier-based retrieval and batch archival | `get_core_engrams()`, `get_oldest_recall_engrams()`, `archive_engrams()`, `get_stats()` (updated) | `sqlx`, `ygg_domain` |
| `ygg_domain::engram` | Updated `MemoryStats` struct | `MemoryStats` (updated) | `serde`, `chrono` |
| `ygg_domain::config` | Updated `TierConfig` with summarization fields | `TierConfig` (updated) | `serde` |

**Ownership rules:**
- Only `mimir::summarization` calls Odin for LLM summarization. Handlers never trigger summarization directly.
- Only `mimir::summarization` modifies the `archived_by` and `summary_of` columns. Handlers read these columns but do not write them.
- Only `mimir::handlers::query_engrams` performs Core tier injection into query results. The store layer returns raw results without Core injection.
- The `SummarizationService` is spawned as a background Tokio task in `main.rs`. It is given a `watch::Receiver` for graceful shutdown.

## File-Level Implementation Plan

### `crates/ygg-domain/src/engram.rs` (MODIFY)

Update `MemoryStats` to add `recall_capacity` and `oldest_recall_at` fields:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub core_count: i64,
    pub recall_count: i64,
    pub archival_count: i64,
    pub recall_capacity: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_recall_at: Option<DateTime<Utc>>,
}
```

### `crates/ygg-domain/src/config.rs` (MODIFY)

Add three new fields to `TierConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierConfig {
    #[serde(default = "default_recall_capacity")]
    pub recall_capacity: usize,
    #[serde(default = "default_summarization_batch")]
    pub summarization_batch_size: usize,
    #[serde(default = "default_check_interval")]
    pub check_interval_secs: u64,
    #[serde(default = "default_min_age_secs")]
    pub min_age_secs: u64,
    #[serde(default = "default_odin_url")]
    pub odin_url: String,
}

fn default_check_interval() -> u64 {
    300
}

fn default_min_age_secs() -> u64 {
    86400
}

fn default_odin_url() -> String {
    "http://localhost:8080".to_string()
}
```

### `crates/ygg-store/src/postgres/engrams.rs` (MODIFY)

Add four new functions:

**`get_core_engrams(pool) -> Result<Vec<Engram>, StoreError>`:**
```sql
SELECT id, cause, effect, tier, tags, created_at, access_count, last_accessed
FROM yggdrasil.engrams
WHERE tier = 'core'
ORDER BY created_at ASC
```

Returns all Core tier engrams. Sets `similarity = 1.0` for each (marker value indicating Core).

**`get_oldest_recall_engrams(pool, limit, min_age_secs) -> Result<Vec<Engram>, StoreError>`:**
```sql
SELECT id, cause, effect, tier, tags, created_at, access_count, last_accessed
FROM yggdrasil.engrams
WHERE tier = 'recall'
  AND archived_by IS NULL
  AND created_at < NOW() - INTERVAL '1 second' * $2
ORDER BY access_count ASC, created_at ASC
LIMIT $1
```

Returns the oldest, least-accessed Recall engrams eligible for summarization.

**`archive_engrams(pool, source_ids, summary_id) -> Result<(), StoreError>`:**
```sql
UPDATE yggdrasil.engrams
SET tier = 'archival',
    archived_by = $2
WHERE id = ANY($1)
  AND tier = 'recall'
```

Marks a batch of Recall engrams as archived, linking them to the summary engram.

**`insert_archival_engram(pool, cause, effect, embedding, content_hash, tags, summary_of_ids) -> Result<Uuid, StoreError>`:**
```sql
INSERT INTO yggdrasil.engrams
    (id, cause, effect, cause_embedding, content_hash, tier, tags, summary_of)
VALUES ($1, $2, $3, $4::vector, $5, 'archival', $6, $7)
```

Inserts a new Archival engram with the `summary_of` array populated.

**Update `get_stats(pool) -> Result<MemoryStats, StoreError>`:**
```sql
SELECT
    COUNT(*) FILTER (WHERE tier = 'core') AS core_count,
    COUNT(*) FILTER (WHERE tier = 'recall') AS recall_count,
    COUNT(*) FILTER (WHERE tier = 'archival') AS archival_count,
    MIN(created_at) FILTER (WHERE tier = 'recall') AS oldest_recall_at
FROM yggdrasil.engrams
```

### `crates/mimir/src/summarization.rs` (NEW)

```rust
/// Background service that periodically checks Recall tier capacity and
/// summarizes old engrams into Archival tier summaries via Odin LLM calls.
pub struct SummarizationService {
    store: Store,
    vectors: VectorStore,
    embedder: EmbedClient,
    http: reqwest::Client,
    config: TierConfig,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
}
```

**`SummarizationService::new(store, vectors, embedder, config, shutdown_rx) -> Self`:**
- Store references and config.
- Create `reqwest::Client` for Odin calls.

**`SummarizationService::start(self) -> tokio::task::JoinHandle<()>`:**
- Spawns a Tokio task that loops:
  1. `tokio::select!` between `tokio::time::sleep(check_interval)` and `shutdown_rx.changed()`
  2. On timer: call `self.check_and_summarize().await`
  3. On shutdown: break loop, log "summarization service stopped"

**`SummarizationService::check_and_summarize(&self) -> Result<(), MimirError>`:**
1. `get_stats(pool)` to get current Recall count
2. If `recall_count <= recall_capacity`: return early (nothing to do)
3. `get_oldest_recall_engrams(pool, summarization_batch_size, min_age_secs)`
4. If batch is empty: return early (no eligible engrams yet)
5. Build summarization prompt from batch
6. Call Odin `POST /v1/chat/completions` (non-streaming)
7. Parse summary from response (extract JSON `{cause, effect}`)
8. Compute content hash and embedding for the summary
9. `insert_archival_engram(pool, cause, effect, embedding, hash, tags, source_ids)`
10. Upsert summary into Qdrant
11. `archive_engrams(pool, source_ids, summary_id)`
12. Delete source engram points from Qdrant (batch delete by IDs)
13. Log: "summarized {count} engrams into archival engram {summary_id}"

**`SummarizationService::call_odin_summarize(&self, prompt: &str, system: &str) -> Result<(String, String), MimirError>`:**
- POST to `{odin_url}/v1/chat/completions`
- Parse response, extract cause+effect JSON
- On HTTP error or parse failure: return `MimirError::Summarization(...)`
- Timeout: 120s (summarization can be slow with large batches)

### `crates/mimir/src/error.rs` (MODIFY)

Add variant:
```rust
/// Summarization-related failures.
#[error("summarization error: {0}")]
Summarization(String),
```

### `crates/mimir/src/handlers.rs` (MODIFY)

**Modify `query_engrams`:**
After fetching vector-similar engrams, add:
```rust
// --- Core tier injection ---
let core_engrams = engrams::get_core_engrams(pool).await?;
let existing_ids: std::collections::HashSet<Uuid> =
    engrams_out.iter().map(|e| e.id).collect();

let mut result = Vec::with_capacity(core_engrams.len() + engrams_out.len());
for mut core in core_engrams {
    if let Some(pos) = engrams_out.iter().position(|e| e.id == core.id) {
        // Core engram was also found by vector search -- use the Qdrant similarity score
        core.similarity = engrams_out[pos].similarity;
        engrams_out.remove(pos);
    } else {
        core.similarity = 1.0; // marker for "always included"
    }
    result.push(core);
}
result.extend(engrams_out);
```

**Add `get_core_engrams` handler:**
```rust
pub async fn get_core_engrams_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, MimirError> {
    let core = engrams::get_core_engrams(state.store.pool()).await?;
    Ok((StatusCode::OK, Json(core)))
}
```

**Modify `get_stats`:**
Pass `recall_capacity` from config into the `MemoryStats` response:
```rust
pub async fn get_stats(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, MimirError> {
    let mut stats = engrams::get_stats(state.store.pool()).await?;
    stats.recall_capacity = state.config.tiers.recall_capacity as i64;
    Ok((StatusCode::OK, Json(stats)))
}
```

### `crates/mimir/src/state.rs` (MODIFY)

Add a shutdown watch channel to `AppState` for coordinating summarization task shutdown:

```rust
pub struct AppState {
    pub store: Store,
    pub vectors: VectorStore,
    pub embedder: EmbedClient,
    pub lsh: LshIndex,
    pub config: MimirConfig,
    /// Sender to signal background tasks to shut down.
    pub shutdown_tx: tokio::sync::watch::Sender<bool>,
}
```

In `AppState::new()`: create `let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);` and store `shutdown_tx`.

### `crates/mimir/src/main.rs` (MODIFY)

1. After constructing `AppState`, clone `shutdown_tx` to create a receiver for the summarization service:
```rust
let summarization_rx = shared_state.shutdown_tx.subscribe();
```

2. Start the summarization service:
```rust
let summarization = SummarizationService::new(
    shared_state.store.clone(),
    shared_state.vectors.clone(),
    shared_state.embedder.clone(),
    shared_state.config.tiers.clone(),
    summarization_rx,
);
let _summarization_handle = summarization.start();
```

3. Add the new route:
```rust
.route("/api/v1/core", get(get_core_engrams_handler))
```

4. On shutdown, signal the summarization task:
```rust
// In shutdown_signal or after axum::serve returns:
let _ = shared_state.shutdown_tx.send(true);
```

### `crates/mimir/src/lib.rs` (MODIFY)

Add: `pub mod summarization;`

### `crates/mimir/Cargo.toml` (MODIFY)

Add `reqwest` dependency (for Odin HTTP calls):
```toml
reqwest = { workspace = true }
```

### `configs/mimir/config.yaml` (MODIFY)

Add summarization fields to the `tiers` section:
```yaml
tiers:
  recall_capacity: 1000
  summarization_batch_size: 100
  check_interval_secs: 300
  min_age_secs: 86400
  odin_url: "http://localhost:8080"
```

## Acceptance Criteria

- [ ] Background summarization task starts with Mimir and logs "summarization service started"
- [ ] When Recall tier count exceeds `recall_capacity`, the summarization task selects the oldest/least-accessed batch and logs "starting summarization of N engrams"
- [ ] Summarization calls Odin `POST /v1/chat/completions` with a well-formed prompt containing the source engrams
- [ ] The LLM response is parsed into a `{cause, effect}` pair and stored as a new Archival tier engram
- [ ] The new Archival engram's `summary_of` column contains the UUIDs of all source engrams
- [ ] Source engrams are updated: `tier = 'archival'`, `archived_by = <summary_uuid>`
- [ ] Source engram vectors are deleted from Qdrant `engrams` collection
- [ ] `POST /api/v1/query` always includes Core tier engrams at the beginning of results, regardless of query text
- [ ] Core engrams that also match vector search are not duplicated in results
- [ ] Core engrams in query results have `similarity: 1.0` when they were not part of the vector search results
- [ ] `GET /api/v1/core` returns all Core tier engrams as a JSON array
- [ ] `GET /api/v1/core` returns empty array `[]` when no Core engrams exist
- [ ] `GET /api/v1/stats` includes `recall_capacity` and `oldest_recall_at` fields
- [ ] Summarization task respects `min_age_secs` -- engrams newer than this threshold are not selected
- [ ] Summarization task skips cycles when Recall count is within capacity (no unnecessary Odin calls)
- [ ] Summarization task handles Odin unreachable gracefully (logs warning, retries next cycle)
- [ ] Summarization task handles malformed LLM response gracefully (uses entire response as effect, logs warning)
- [ ] Summarization task stops cleanly on SIGTERM/SIGINT (logs "summarization service stopped")
- [ ] `POST /api/v1/promote` with `tier: "core"` still works correctly (already implemented, verify no regressions)
- [ ] Recall tier count stabilizes at or below `recall_capacity` after sufficient summarization cycles
- [ ] No panics in the summarization task on empty batches, duplicate content hashes, or database errors

## Dependencies

| Dependency | Type | Status |
|------------|------|--------|
| Sprint 002 (Mimir MVP) | Must be implemented | Mimir base service with store, query, promote endpoints |
| Sprint 005 (Odin) | Must be running | Odin serves the `/v1/chat/completions` endpoint for summarization |
| Migration 001 | Already applied | `archived_by` and `summary_of` columns already exist in `yggdrasil.engrams` |
| Ollama on Munin | Must have coding model | The summarization prompt is processed by whatever model Odin routes to (default: qwen3-coder-30b-a3b) |
| `reqwest` crate | Workspace dependency | Already in workspace Cargo.toml, needs to be added to mimir's Cargo.toml |

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Summarization LLM call competes with user chat requests for Odin backend semaphore | Summarization runs in background, is latency-insensitive, and processes one batch every 5 minutes. Even if it blocks waiting for a slot, user requests get priority via first-come-first-served. If contention becomes an issue, add a priority queue to Odin's backend semaphore in a future sprint. |
| LLM produces poor summaries that lose critical information | Summarization prompt instructs the model to preserve all key facts. Source engrams remain in PostgreSQL (tier changed to 'archival', not deleted) so no data is permanently lost. Manual review is possible via `summary_of` links. |
| Summarization of dedup-colliding content (new summary has same hash as existing engram) | The `insert_archival_engram` function uses the same dedup check as regular store. On collision (unlikely), log a warning and skip the batch -- the source engrams remain in Recall tier. |
| Core tier grows unbounded, degrading query performance | Core tier is manual-only (via `POST /api/v1/promote`). Log a warning if Core count exceeds 50. Future: add a hard cap or require demotion before promotion. |
| Summarization fails mid-batch (e.g., Qdrant delete succeeds but PostgreSQL update fails) | Use a transaction for the PostgreSQL updates (archive + insert). Qdrant operations are outside the transaction but are idempotent -- re-running the same cycle will re-attempt Qdrant deletes. Stale Qdrant points are harmless (they point to archived engrams that will not match tier filters). |
| `min_age_secs` too short causes summarization of engrams still in active use | Default is 24 hours. Configurable per deployment. The `access_count ASC` ordering in the candidate query deprioritizes frequently-accessed engrams even if they are old. |

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | Summarization calls Odin, not Ollama directly | Reuses Odin's model routing and backend semaphore. Mimir does not need to know about Ollama endpoints or manage concurrency itself. |
| 2026-03-09 | Core engrams prepended to query results with `similarity: 1.0` marker | Core engrams are permanent context -- they must always be present. The `1.0` marker distinguishes them from vector-matched results. Prepending ensures they appear first. |
| 2026-03-09 | Source engrams not deleted from PostgreSQL, only re-tiered to Archival | Preserves data lineage. The `summary_of` and `archived_by` columns provide bidirectional traceability. If a summary is found inadequate, the originals can be inspected. |
| 2026-03-09 | Skip LSH removal for archived engrams | Re-embedding or caching embeddings for LSH removal adds complexity for minimal benefit. Stale LSH entries point to archived engrams that are filtered by tier in the handlers. The LSH index is rebuilt on restart from `lsh_buckets` which still contains the stale entries, but the query path (Qdrant + PostgreSQL) is authoritative. |
| 2026-03-09 | `min_age_secs` default 24 hours | Prevents summarizing engrams from the current work session. Most valuable recall engrams are recent ones that are being actively referenced. 24 hours provides a conservative buffer. |
| 2026-03-09 | `limit` on query does not include Core engrams | Core engrams are always included regardless of limit. This prevents a low limit from excluding permanent context. The total result count can exceed `limit` by the Core engram count. |
| 2026-03-09 | No database migration needed | Migration 001 already includes `archived_by UUID` and `summary_of UUID[]` columns on `yggdrasil.engrams`. These columns were designed for this sprint. |
| 2026-03-09 | Odin model selection is null (use default routing) | The summarization prompt is a text consolidation task, not coding or reasoning. The default model (coding model on Munin) is adequate. If a dedicated summarization model is needed later, it can be specified in `TierConfig`. |
