# System Architect Memory -- Yggdrasil Project

## Project Location
- Workspace root: `/home/jesus/Documents/HardwareSetup/yggdrasil/`
- Hardware reference (canonical): `/home/jesus/Documents/HardwareSetup/NetworkHardware.md`
- Fergus client (API contract source): `/home/jesus/Documents/Rust/Fergus_Agent/fergus-rs/crates/fergus-server/src/engram_client.rs`

## Key Architecture Decisions
- Embedding dimension: 4096 (qwen3-embedding actual output, NOT 1024 as originally planned)
- Qdrant uses UUID strings as point IDs, stores no payload (metadata lives in PostgreSQL)
- Dual-write pattern: data goes to both PostgreSQL and Qdrant
- SHA-256 dedup on content concatenation
- LSH uses SimHash (random hyperplanes), in-memory DashMap with DB persistence in `lsh_buckets`
- Fergus client expects flat `Vec<Engram>` from /api/v1/query (not wrapped in object)
- No auth on private LAN services (10.0.x.x)
- tree_sitter::Parser is !Send + !Sync -- must construct per blocking task
- MCP server is standalone stdio binary (ygg-mcp-server), NOT embedded in Odin
- HA config is Option in both OdinConfig and McpServerConfig -- graceful degradation when absent

## Hardware Targets
- Odin + Mimir + ygg-mcp-server deploy to **Munin** (REDACTED_MUNIN_IP): Intel Core Ultra 185H, 48GB DDR5, ARC iGPU
- Huginn + Muninn deploy to **Hugin** (REDACTED_HUGIN_IP): AMD Ryzen 7 255 (Zen 5, 8C/16T), 64GB DDR5
- PostgreSQL on **Munin** (localhost:5432): pgvector Docker container (Hades PG lacked pgvector)
- Qdrant on **Hades** (REDACTED_HADES_IP:6334): Intel N150, 32GB, SATA SSD pool "Merlin"
- Home Assistant on **chirp** (REDACTED_CHIRP_IP): 2 cores, 4GB RAM, on Plume (REDACTED_PLUME_IP)
- Heavy compute on **Thor** (REDACTED_THOR_IP): Threadripper 2990WX, 128GB, multi-GPU (on-demand only)

## Workspace Structure
- 10 crates: ygg-domain, ygg-store, ygg-embed, ygg-mcp, ygg-mcp-server, ygg-ha, odin, mimir, huginn, muninn
- Edition 2024, Rust workspace with shared deps
- Migrations in `/migrations/` (runtime path, not sqlx compile-time)
- Configs in `/configs/<service>/config.yaml` (odin uses `node.yaml`)

## Sprint Status
- Sprint 000 (Infrastructure): exists in sprints/
- Sprint 001 (Foundation): DONE -- all 9 crates compile clean
- Sprint 002 (Mimir MVP): DONE
- Sprint 003 (Huginn MVP): DONE -- code implemented and deployed
- Sprint 004 (Muninn MVP): DONE -- code implemented and deployed
- Sprint 005 (Odin Orchestrator): DONE -- fully implemented, deployed to Munin, includes HA context, metrics, dual backend type
- Sprint 006 (MCP Integration): DONE -- 9 tools (5 core + 4 HA), 2 resources, rmcp 1.1, schemars 1
- Sprint 007 (HA Integration): SUPERSEDED -- HA tools merged into Sprint 006
- Sprint 008 (Mimir Advanced): PLANNING -- doc written 2026-03-09, depends on 002+005
- Sprint 009 (Hardware Optimization): PLANNING -- doc written 2026-03-09, optimization track
- Sprint 010 (Production Hardening): DONE -- systemd units (WatchdogSec=30), metrics, backup, deploy scripts, bug fixes (qwq->qwen3, HA_TOKEN expansion, PG host fix)
- Sprint 011 (Hardening): DONE -- systemd units, deploy scripts
- Sprint 013 (Hugin MoE Swap): DONE -- replaced QwQ-32B-AWQ/vLLM with qwen3:30b-a3b on Hugin (19x speedup)
- Sprint 014 (Munin IPEX-LLM Ollama): DONE -- IPEX-LLM Docker container deployed, 15.9 tok/s

## Service Ports (CANONICAL)
- Odin: 8080 (Munin)
- Mimir: 9090 (Munin)
- Muninn: 9091 (Hugin)
- Huginn: 9092 (Hugin) -- health/metrics only, added Sprint 010
- ygg-mcp-server: N/A (stdio)
- Home Assistant: 8123 (chirp, REDACTED_CHIRP_IP)

## Ollama Models (current deployed state)
- Munin (localhost:11434, IPEX-LLM container): qwen3-coder:30b-a3b-q4_K_M (coding, 15.9 tok/s), qwen3-embedding (4096-dim)
- Hugin (REDACTED_HUGIN_IP:11434, native Ollama): qwen3:30b-a3b (reasoning, 26.48 tok/s), qwen3-embedding (4096-dim)
- vLLM on Hugin: STOPPED (Sprint 013), container remains for rollback
- NOTE: NetworkHardware.md says Munin runs "qwen3 14b" -- outdated, actual is qwen3-coder:30b-a3b-q4_K_M

## Patterns Established
- Error types: thiserror enums per crate (MimirError, HuginnError, StoreError, OdinError, HaError)
- Config: serde YAML deserialization into typed structs in ygg_domain::config
- Store pattern: `Store` struct wraps `PgPool`, free functions for queries in submodules
- VectorStore: wraps Qdrant client, `ensure_collection()` on startup, no payload stored
- EmbedClient: stateless HTTP client, `embed_single()` / `embed_batch()`
- Sprint docs follow strict template with all sections filled (no TBD/TODO)
- Embedding text for code chunks: "{lang} {type}: {name}\nParent: {parent}\n{content}"
- Odin uses HTTP-only interfaces (no ygg-store, no ygg-embed in Sprint 005)
- Transparent proxy: forward body as axum::body::Bytes, preserve status code and content-type

## Store Layer Conventions
- `ygg_store::postgres::<entity>::<operation>()` -- free functions, not methods on Store
- Existing modules: `engrams`, `chunks`
- Each service owns which store functions it calls
- Batch fetch pattern: `WHERE id = ANY($1)` with UUID array

## Odin-Specific Patterns
- SemanticRouter: keyword-based v1, hardcoded keyword sets per intent
- RAG: parallel fetch from Muninn + Mimir via tokio::join!, 3s timeout, best-effort
- Streaming: Ollama newline-delimited JSON -> OpenAI SSE + data: [DONE]
- Per-backend semaphore with try_acquire() returning 503
- Engram store-on-completion: fire-and-forget tokio::spawn
- Mimir proxy: transparent passthrough for /api/v1/query and /api/v1/store
- HA intent: skip Muninn code context, keep Mimir engrams, inject HA domain summary (cached 60s)

## MCP Integration Patterns (Sprint 006 -- DONE)
- rmcp 1.1 official Rust MCP SDK with features = ["server", "transport-io"]
- schemars 1.x (NOT 0.8) -- must match rmcp's re-exported version
- Uses `#[tool_router]` + `#[tool]` macros, `ServerHandler` impl, stdio transport
- Tracing to stderr (stdout is JSON-RPC channel)
- 5 core tools + 4 HA tools = 9 total (HA tools gated by config.ha presence)
- 2 resources: yggdrasil://models, yggdrasil://memory/stats
- search_code calls Muninn directly (Odin has no /api/v1/search proxy)
- McpServerConfig: odin_url, muninn_url (optional), timeout_secs, ha (optional)
- Config at configs/mcp-server/config.yaml (timeout: 300s for slow HA automation calls)
- ygg-mcp depends on ygg-ha (HA tools live in ygg-mcp::tools, not separate crate)
- Input validation: 100KB max search/memory fields, 1MB max prompts
- ha_call_service: 19-domain allowlist, `lock` excluded for safety
- AutomationGenerator model: FIXED -- now uses "qwen3:30b-a3b" (was hardcoded "qwq-32b", fixed Sprint 010)

## HA Integration Patterns (merged into Sprint 006)
- HA REST API: /api/states, /api/services, /api/services/{domain}/{service}
- Bearer token auth (long-lived access token from HA UI)
- AutomationGenerator: fetches entities+services for context, calls LLM via Odin
- Odin semantic router: "home_automation" intent -> qwen3:30b-a3b on Hugin
- HA context cached 60s in Odin via RwLock<Option<(Instant, String)>>
- ${HA_TOKEN} env var expansion needed in config loading (serde_yaml does NOT expand env vars)

## Retrieval Patterns (Muninn)
- Hybrid search: vector (Qdrant) + BM25 (PostgreSQL tsvector) fused via RRF (k=60)
- Fetch 3x limit candidates from each backend
- Context assembly: group by file_path, sort by start_line, ordered by best score
- Token budget: 4 chars/token heuristic, 80% fill ratio of 32K window
- Muninn is strictly read-only

## Config Structs (ygg_domain::config)
- OdinConfig: node_name, listen_addr, backends, routing, mimir, muninn, ha (Option)
- HuginnConfig: watch_paths, database_url, qdrant_url, embed, debounce_ms, listen_addr (Sprint 010)
- MimirConfig: listen_addr, database_url, qdrant_url, embed, lsh, tiers
- MuninnConfig: listen_addr, database_url, qdrant_url, embed, search
- McpServerConfig: odin_url, muninn_url (Option), timeout_secs, ha (Option)
- HaConfig: url, token, timeout_secs
- TierConfig: recall_capacity, summarization_batch_size, check_interval_secs, min_age_secs, odin_url (Sprint 008)
- EmbedConfig: ollama_url, model, backend (default "ollama"), model_path (Option, Sprint 009)

## Database Schema
- Schema namespace: `yggdrasil`
- Migration 001: engrams (includes archived_by, summary_of columns), lsh_buckets
- Migration 002: indexed_files, code_chunks (with generated tsvector search_vec)
- Qdrant collections: `engrams` (Mimir), `code_chunks` (Huginn)
- ON DELETE CASCADE from indexed_files to code_chunks
- Sprint 008 uses existing archived_by + summary_of columns (no new migration needed)

## Mimir Advanced Patterns (Sprint 008)
- Background SummarizationService: tokio task with watch channel for shutdown
- Calls Odin /v1/chat/completions for LLM summarization (not Ollama directly)
- Core engrams always prepended to query results (similarity: 1.0 marker)
- Source engrams re-tiered to archival (not deleted), Qdrant vectors removed
- Stale LSH entries left in place (filtered by tier in query handlers)
- TierConfig: recall_capacity (1000), summarization_batch_size (100), check_interval_secs (300), min_age_secs (86400)

## Production Hardening Patterns (Sprint 010 -- DONE)
- systemd Type=notify with sd-notify crate for all HTTP services, WatchdogSec=30 on all 4 daemon units
- Watchdog: tokio task sends WATCHDOG=1 every 15s (half of WatchdogSec=30)
- MCP server uses Type=simple (stdout is JSON-RPC channel, no watchdog)
- metrics + metrics-exporter-prometheus for /metrics endpoints
- Huginn gets health listener on port 9092 (health + metrics)
- Backup: pg_dump from Munin localhost:5432/yggdrasil + Qdrant snapshots on Hades, stored in RAVEN pool
- Deploy scripts: install.sh, update.sh, rollback.sh (shell, not Ansible)
- yggdrasil system user, /opt/yggdrasil/bin/, /etc/yggdrasil/ config paths
- HA_TOKEN env var expansion: performed at startup in ygg-mcp-server main.rs (serde_yaml cannot expand)
- Remaining deploy tasks: backup cron not yet installed on Munin, NetworkHardware.md stale (infra-devops)

## IPEX-LLM Patterns (Sprint 014)
- Docker image: `intelanalytics/ipex-llm-inference-cpp-xpu:latest` (archived Jan 2026, images still on Docker Hub)
- iGPU env vars: BIGDL_LLM_XMX_DISABLED=1, SYCL_CACHE_PERSISTENT=1, DEVICE=iGPU, ONEAPI_DEVICE_SELECTOR=level_zero:0
- Startup: `source ipex-llm-init --gpu --device iGPU && bash start-ollama.sh`
- Context cap: OLLAMA_CONTEXT_LENGTH=8192 (prevents 32K KV cache from exhausting 48GB RAM)
- Deploy artifacts: `deploy/munin/docker-compose.ipex-ollama.yml`, `deploy/munin/yggdrasil-ollama-ipex.service`
- Munin native Ollama: stopped+disabled, binary left for rollback
- Sprint 014 supersedes Sprint 009 iGPU workstream (containerized approach replaces host oneAPI install)

## Known Discrepancies
- NetworkHardware.md says Munin runs "qwen3 14b" -- outdated, actual is qwen3-coder:30b-a3b-q4_K_M via IPEX-LLM container
- ARCHITECTURE.md embedding dims were corrected from 1024 to 4096 on 2026-03-09
- ARCHITECTURE.md PostgreSQL location corrected from Hades to Munin pgvector Docker on 2026-03-09
- AutomationGenerator model discrepancy: RESOLVED in Sprint 010 -- now uses qwen3:30b-a3b
- MCP config ${HA_TOKEN}: RESOLVED in Sprint 010 -- ygg-mcp-server main.rs now performs env var expansion at startup

## Odin Implementation Details (Sprint 005 -- DONE)
- 10 source files: main.rs, lib.rs, openai.rs, error.rs, router.rs, state.rs, proxy.rs, rag.rs, handlers.rs, metrics.rs
- Supports both BackendType::Ollama and BackendType::Openai (dual backend)
- OpenAI backends: forward request directly to /v1/chat/completions, SSE pass-through
- Streaming: 10MB line buffer cap to prevent OOM
- HA: HaClient from ygg-ha, domain summary cache with RwLock double-check pattern
- Metrics: 5 Prometheus metrics via metrics + metrics-exporter-prometheus
- systemd: Type=notify, ExecStartPre waits for Mimir health, HA_TOKEN env var
- Global concurrency: tower::limit::ConcurrencyLimitLayer::new(64)
- Body limit: DefaultBodyLimit::max(2MB)
- Config: configs/odin/node.yaml, CLI override via --listen-addr or ODIN_LISTEN_ADDR env
