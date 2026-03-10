# Core-Executor Agent Memory — Yggdrasil

## Project Structure
- Workspace root: `/home/jesus/Documents/HardwareSetup/yggdrasil`
- All crates under `crates/`: `ygg-domain`, `ygg-store`, `ygg-embed`, `mimir`, `odin`, `muninn`, `huginn`, `ygg-mcp`, `ygg-mcp-server`, `ygg-ha`
- Configs: `configs/<service>/config.yaml`
- Sprints: `sprints/sprint_NNN_<name>.md`

## Key File Locations
- Domain types (Engram, MemoryStats, TierConfig): `crates/ygg-domain/src/engram.rs`, `crates/ygg-domain/src/config.rs`
- PG engram queries: `crates/ygg-store/src/postgres/engrams.rs`
- Qdrant wrapper: `crates/ygg-store/src/qdrant.rs` — has `delete_many(&[Uuid])`, `upsert()`, `search()`
- Mimir handlers: `crates/mimir/src/handlers.rs`
- Mimir state: `crates/mimir/src/state.rs`
- Huginn health server: `crates/huginn/src/health.rs` — Arc<HealthState>, start_health_server()
- Deploy scripts: `deploy/` (install.sh, update.sh, rollback.sh, backup-hades.sh, wait-for-health.sh)
- Systemd units: `deploy/systemd/` (5 unit files)
- Ops runbook: `docs/OPERATIONS.md`

## Conventions
- Edition 2024 (watch for `gen` keyword and lifetime capture changes)
- `thiserror` for library error types, `anyhow` for application errors in `main()`
- Store layer returns `recall_capacity = 0` as placeholder; handlers inject configured value
- `get_core_engrams()` always returns similarity=1.0 as a marker for "always included"
- Default serde function names must be unique per file — use descriptive prefix when collision exists (e.g. `default_summarization_odin_url` vs `default_odin_url` for McpServerConfig)
- Fire-and-forget Tokio tasks for non-critical paths (LSH persist, access count bumps)
- `tokio::sync::watch` channel for graceful shutdown signaling across background tasks

## Sprint 008 Patterns (completed)
- Background summarization task: loop with `tokio::select!` on sleep + shutdown_rx.changed()
- Core tier injection: fetch core engrams after Qdrant search, deduplicate by ID, prepend
- `insert_archival_engram` uses `summary_of UUID[]` column (already in migration 001)
- `archive_engrams` uses `archived_by UUID` column (already in migration 001)
- reqwest client timeout: 120s for Odin summarization calls
- Fallback when LLM returns non-JSON: use full content as `effect`, construct generic `cause`
- LSH removal intentionally skipped for archived engrams (stale entries filtered by tier at query time)

## Sprint 009 Patterns (completed)
- `ygg-embed` dual-backend: `EmbedBackend` enum (Ollama | Candle) inside `EmbedClient`; candle variant behind `#[cfg(feature = "candle")]`
- candle optional deps live ONLY in `crates/ygg-embed/Cargo.toml` — NOT in workspace Cargo.toml
- `EmbedClient: Clone` requirement satisfied by `reqwest::Client` (Arc internally) for Ollama, `Arc<CandelEmbedModel>` for candle
- `EmbedConfig` in `ygg-domain/src/config.rs` now has `backend: String` (default "ollama") and `model_path: Option<String>`
- `docs/HARDWARE_OPTIMIZATION.md` is the canonical benchmark/procedure document for hardware-optimizer agent

## sd-notify API Break (v0.4.5)
- Old API: `watchdog_enabled(unset_env: bool) -> Option<Duration>`
- New API (0.4.5): `watchdog_enabled(unset_env: bool, usec: &mut u64) -> bool`
- Fix pattern: `let mut watchdog_usec = 0u64; if sd_notify::watchdog_enabled(false, &mut watchdog_usec) { let half = std::time::Duration::from_micros(watchdog_usec / 2); ... }`
- Affected crates: mimir, odin, muninn, huginn (all main.rs files)
- This break was triggered by adding optional candle deps to ygg-embed, which caused cargo to resolve a newer sd-notify version

## Sprint 010 Patterns (completed)
- systemd `Type=notify` + sd-notify in all 4 binary crates (odin, mimir, muninn, huginn)
- Huginn health server runs only in `Watch` subcommand (not `Index`); systemd unit must specify `watch` in ExecStart
- PrometheusHandle passed into `Arc<HealthState>` so `/metrics` handler can call `handle.render()`
- Axum route closure for metrics: `get(move || { let h = handle.clone(); async move { ([headers], h.render()) } })`
- Middleware layer ORDER: `.layer(middleware::from_fn(...)).layer(CorsLayer::permissive())` — CORS outermost
- Port assignments: Odin 8080 (Munin), Mimir 9090 (Munin), Muninn 9091 (Hugin), Huginn 9092 (Hugin)

## Workflow
- Always run `cargo check --workspace` after all changes to verify compilation
- No auto-commits — user handles all git operations
- Read sprint doc before writing any code
- Read all affected files before editing
- Adding optional deps to a crate can bump transitive deps across workspace — always re-run full workspace check
