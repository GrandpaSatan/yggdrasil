# core-executor agent memory
# Project: yggdrasil (HardwareSetup workspace)

## Project Structure
- Workspace root: `/home/jesus/Documents/HardwareSetup/yggdrasil/`
- Crates: `ygg-domain`, `ygg-store`, `ygg-embed`, `mimir`, `odin`, `huginn`, `muninn`, `ygg-mcp`, `ygg-ha`, `ygg-mcp-server`
- Sprint docs: `yggdrasil/sprints/`
- Configs: `yggdrasil/configs/<service>/config.yaml` (odin uses `node.yaml` not `config.yaml`)
- Migrations: `yggdrasil/migrations/`

## Edition 2024 Gotchas
- `gen` is a reserved keyword in Rust 2024. Use `rng.r#gen::<T>()` or `RngCore::next_u32()`.
- All crates use `edition = "2024"` via workspace.package.
- `impl Trait` return types in async fns capture ALL referenced lifetimes in Rust 2024.
  When a function returns `impl Stream + 'static` (required by `Sse::new`), all parameters
  must be owned (not borrowed). Pass `reqwest::Client` by value (it's `Clone + Arc`-based),
  pass URLs as `String` not `&str`. See `odin/proxy.rs::stream_chat` for the pattern.

## sqlx Usage
- `sqlx` is NOT re-exported by `ygg-store`. Add it explicitly to each crate's Cargo.toml.
- Type inference on `.map_err` closures needs: `map_err(|e: sqlx::Error| ...)`.
- `WHERE id = ANY($1)` with `.bind(&[Uuid])` works natively with sqlx 0.8 postgres driver.
- Always `use sqlx::Row as _;` at call sites to bring `.get()` into scope.

## Key Type Locations
- `ygg_domain::engram::{Engram, NewEngram, StoreResponse, EngramQuery, MemoryStats, MemoryTier}`
- `ygg_domain::config::OdinConfig` (+ BackendConfig, RoutingConfig, RoutingRule, MimirClientConfig, MuninnClientConfig)
- `ygg_domain::chunk::{SearchQuery, SearchResponse}` — Muninn's search types
- `SearchResponse.context: String` — pre-assembled code context from Muninn
- `ygg_store::Store` — wraps PgPool, has `.pool()`, `.connect()`, `.migrate()`
- `ygg_store::qdrant::VectorStore` — `.connect()`, `.ensure_collection()`, `.upsert()`, `.search()`
- `ygg_embed::EmbedClient` — `.new(url, model)`, `.embed_single(text)`, `.embed_batch(texts)`

## Axum Patterns (v0.8)
- `State<AppState>` when all fields are Clone (reqwest::Client, VectorStore, EmbedClient are Arc-based).
- `State<Arc<AppState>>` when AppState holds non-Clone fields (e.g. LshIndex in mimir).
- Handler: `async fn foo(State(state): State<AppState>, Json(body): Json<T>)`
- SSE: `axum::response::sse::Sse::new(stream)` requires stream to be `'static`.
- Transparent proxy response: `(StatusCode, HeaderMap, Bytes).into_response()`.
- `axum::body::Bytes` re-exports `bytes::Bytes` — do NOT add `bytes` as a direct dep.

## LSH Design (mimir) — SUPERSEDED by Sprint 015
- LSH index (`lsh.rs`, `LshIndex`) is still present in source but no longer wired into AppState.
- Sprint 015 replaced it with `SdrIndex` (brute-force Hamming scan, 256-bit packed SDRs).
- `LshConfig` in config.rs replaced by `SdrConfig { dim_bits: usize, model_dir: String }`.
- `MimirConfig` no longer has `embed` or `lsh` fields — only `sdr`, `qdrant_url`, `tiers`.

## SDR Design (mimir, Sprint 015 complete)
- `mimir::sdr` — pure functions: `binarize()`, `hamming_distance()`, `hamming_similarity()`,
  `to_bytes()`, `from_bytes()`, `to_f32_vec()`, `popcount()`. Type alias `Sdr = [u64; 4]`.
- `mimir::sdr_index::SdrIndex` — `RwLock<Vec<(Uuid, Sdr)>>`, brute-force top-K query.
  Methods: `insert()`, `remove()`, `query()`, `len()`, `load_from_rows()`.
- `AppState` in mimir holds `sdr_index: SdrIndex` + `embedder: OnnxEmbedder`.
- `OnnxEmbedder` wraps `Arc<Mutex<Session>>` (Session::run takes &mut self).
- `mimir::embedder::OnnxEmbedder` — `.load(Path)`, `.embed(&str) -> Result<Vec<f32>>` (sync).
  Always call via `tokio::task::spawn_blocking`. Session mutex is uncontended in practice.
- `ort` 2.0.0-rc.12 API: `Session::builder()?` → `.with_intra_threads(n).unwrap_or_else(|e| e.recover())`
  → `.commit_from_file(&path)?`. `Session::run` takes `&mut self`.
  `try_extract_tensor::<f32>()` returns `Result<(&Shape, &[f32])>` — Shape derefs to `&[i64]`.
  Use `TensorRef::from_array_view(&array)?` with ndarray 0.17 Array2 (NOT 0.16).
  The MutexGuard must be bound to a `let` variable (not a temporary) when outputs borrow from it.
- ndarray workspace dep bumped to "0.17" to match ort 2.0.0-rc.12's dependency.
- `VectorStore::ensure_collection_dim(name, dim, Distance)` added to ygg-store/src/qdrant.rs.
  `Distance` is re-exported as `pub use qdrant_client::qdrant::Distance` from `ygg_store::qdrant`.
- `engrams_sdr` Qdrant collection: 256-dim, Dot product (compatible with {0,1} float vectors).
- New ygg-store engrams functions: `insert_engram_sdr`, `fetch_engram_events`, `get_core_engram_events`.
  `trigger_type` and `trigger_label` columns are nullable — use `Option<String>` in `get()` calls.
- `/api/v1/recall` (POST, RecallQuery → RecallResponse): dual-system SDR recall endpoint.
  System 1 = in-memory Hamming (SdrIndex::query), System 2 = Qdrant dot-product search.
  Merge by UUID taking max similarity, then fetch metadata from PG, build EngramEvent list.
- Dev machine (Ubuntu 22.04, glibc 2.35) CANNOT link ort debug binaries (requires glibc 2.38+).
  Munin (glibc 2.42) can build+run fine. Use `cargo check --release` on dev machine for validation.

## Huginn Key Patterns (Sprint 003)
- `Language` does NOT impl `Hash` — use `Vec<(Language, T)>` with linear scan.
- `tree_sitter::QueryMatches` uses `streaming_iterator::StreamingIterator`, NOT `std::Iterator`.
- `tree_sitter::Parser` is `!Send + !Sync` — construct inside each `spawn_blocking` closure.
- Do NOT call sqlx directly from huginn — add helper functions to ygg-store.

## Muninn Key Patterns (Sprint 004)
- Muninn is read-only. Port: `0.0.0.0:9091` (was 9100 in Sprint 004 config — corrected Sprint 005).
- `hybrid_search()` owns the full pipeline: embed → parallel(vector+BM25) → RRF → batch fetch.
- `StatsResponse` is local to `muninn::handlers`, not in `ygg_domain`.

## Odin Key Patterns (Sprint 005)
- Odin uses HTTP only — no ygg-store, no ygg-embed deps needed in Sprint 005.
- `proxy::stream_chat` takes owned `reqwest::Client` and `String` (not refs) for `'static` stream.
- `proxy::generate_chat` and `proxy::list_models` take `&reqwest::Client` + `&str` (fine, non-streaming).
- Semantic router: keyword sets hardcoded per intent (coding/reasoning/home_assistant).
- RAG: Muninn + Mimir queried via `tokio::join!` with 3s `tokio::time::timeout` each (best-effort).
- Engram store: `tokio::spawn` fire-and-forget after non-streaming completion.
- Mimir proxy: forward `axum::body::Bytes` unchanged, mirror upstream status code + content-type.
- Health handler always returns HTTP 200 (degraded state expressed in body JSON, not HTTP status).

## rmcp 1.1 Patterns (Sprint 006)
- rmcp version in use: 1.1.0 (sprint doc said 0.16 — actual crates.io latest is 1.1.0).
- rmcp re-exports `schemars` 1.x as `rmcp::schemars`. Workspace `schemars` dep MUST be "1" (not "0.8").
- The `#[derive(JsonSchema)]` proc-macro resolves `schemars::` by crate name, so a direct `schemars = "1"`
  dep is required even if you also import `rmcp::schemars::JsonSchema`.
- Tool pattern: `#[tool_router]` on impl block + `#[tool_handler]` on ServerHandler impl.
  The `#[tool_router]` block must come BEFORE the `from_config` impl that calls `Self::tool_router()`.
- Struct must hold `tool_router: ToolRouter<Self>` field; init via `Self::tool_router()`.
- `ServerInfo` is a type alias for `InitializeResult`. Use `ServerInfo::new(capabilities).with_server_info(Implementation::new(name, version))`.
- `CallToolResult::success(vec![Content::text(s)])` and `CallToolResult::error(vec![Content::text(s)])`.
- `ListResourcesResult::with_all_items(vec![...])` — no struct literal (has `meta` field).
- `RawResource::new(uri, name).no_annotation()` to create `Resource` (no separate `Resource::new`).
- `AnnotateAble` trait method `no_annotation()` converts `RawResource` → `Resource`.
- `rmcp::transport::stdio()` returns `(tokio::io::Stdin, tokio::io::Stdout)`.
- Serve pattern: `server.serve((stdin, stdout)).await?.waiting().await?`.
- `#[tool_handler]` on `impl ServerHandler` — generates `list_tools`/`call_tool` dispatch.
- Tool methods return `String` (or any `IntoCallToolResult` type) — rmcp wraps them automatically.
- `ReadResourceResult::new(vec![ResourceContents::TextResourceContents { uri, mime_type, text, meta }])`.
- `ResourceContents::TextResourceContents` uses named fields (no constructor shorthand for uri/mime/text).

## HA Integration Patterns (Sprint 007)
- `HaConfig` now has `timeout_secs: u64` (default 10) — added `default_ha_timeout()` fn in config.rs.
- `OdinConfig` and `McpServerConfig` both have `pub ha: Option<HaConfig>` with `#[serde(default)]`.
- `HaClient::list_entities(domain: Option<&str>)` — calls `get_states()` then filters by `"{domain}."` prefix.
- `HaClient::get_services()` — GET `/api/services`, returns `Vec<DomainServices>`.
- `AutomationGenerator` in `ygg-ha::automation` — takes `odin_url` + `model`, calls Odin `/v1/chat/completions`.
- `extract_yaml()` searches for ` ```yaml ` fence, falls back to raw content if not found.
- `YggdrasilServer` holds `ha_client: Option<HaClient>` + `generator: Option<AutomationGenerator>`.
- All four HA tool functions take `Option<&HaClient>` — return "not configured" error when `None`.
- `AppState` has `ha_client: Option<HaClient>` and `ha_context_cache: Arc<RwLock<Option<(Instant, String)>>>`.
- `rag::fetch_context` takes `intent: &str` — skips Muninn for HA intents, populates `ha_context` field.
- Both `"home_assistant"` and `"home_automation"` intent names map to `ha_keywords()` in router.rs.
- `"gen"` is reserved in Edition 2024 — must rename local variable (used `automation_gen` here).
- HA context cache TTL is 60s; uses double-checked locking pattern under `RwLock`.

## Odin Zero-Injection Memory Architecture (Sprint 015)
- `odin::memory_router` — `apply_memory_events(&RecallResponse, &mut RoutingDecision)`.
  Called in `handlers::chat_handler` AFTER `router.classify()`, BEFORE semaphore acquire.
- `RoutingDecision` has NO `confidence` field — only `intent`, `model`, `backend_url`,
  `backend_name`, `backend_type`. Don't add confidence; use intent override pattern only.
- `RagContext.engram_context: Option<String>` is GONE. Replaced by `memory_events: Option<RecallResponse>`.
- Mimir is now called at `POST /api/v1/recall` (not `/api/v1/query`). Returns `RecallResponse`.
- `rag::fetch_memory_events()` is the public function replacing the old private `fetch_engram_context`.
- `build_system_prompt()` NO LONGER injects any engram/memory text. Code + HA context only.
- `odin/Cargo.toml` has `[dev-dependencies] chrono = { workspace = true }` for test helpers.
- Handlers.rs calls `rag::fetch_memory_events()` separately (step 4) then `rag::fetch_context()`
  (step 6) — the latter also calls fetch_memory_events internally for RagContext.memory_events.

## ort Version Pin
- `ort = "2"` in workspace Cargo.toml causes resolution failure (no stable 2.x on crates.io).
- Fixed: `ort = "=2.0.0-rc.12"` — must use exact version pin for RC packages.
- mimir/embedder.rs has pre-existing ort RC API incompatibilities (E0308, E0277, E0599).
  These are NOT caused by our changes — they predate Sprint 015 step 2.

## Sprint Status
- Sprint 001 (Foundation): DONE
- Sprint 002 (Mimir MVP): IMPLEMENTED — `cargo check --release -p mimir` clean
- Sprint 003 (Huginn MVP): IMPLEMENTED — `cargo build --release -p huginn` clean
- Sprint 004 (Muninn MVP): IMPLEMENTED — 10/10 unit tests pass
- Sprint 005 (Odin MVP): IMPLEMENTED — `cargo check --release -p odin` clean, workspace clean
- Sprint 006 (MCP Integration): IMPLEMENTED — `cargo check --workspace` clean
- Sprint 007 (HA Integration): IMPLEMENTED — `cargo check --workspace --release` clean, 3/3 unit tests pass
- Sprint 015 (SDR types + index, step 1): IMPLEMENTED — workspace clean, 11/11 unit tests pass
- Sprint 015 (Odin zero-injection memory, step 2): IMPLEMENTED — odin check clean, 6/6 unit tests pass
- Sprint 015 (ONNX SDR pipeline wiring, step 3): IMPLEMENTED — `cargo check --release --workspace` clean
  (dev machine glibc 2.35 blocks `cargo test` link; build on Munin glibc 2.42 to run tests)
