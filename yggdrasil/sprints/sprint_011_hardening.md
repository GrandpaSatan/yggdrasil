# Sprint 011 — Reliability & Hardening

**Objective:** Fix crash paths, resource leaks, and missing guards across all Yggdrasil services.

**Status:** COMPLETE (2026-03-09)

---

## Sprint 1 — Critical Reliability Fixes

| # | Issue | File | Fix |
|---|-------|------|-----|
| 1 | ygg-embed HTTP client has NO timeout | `ygg-embed/src/lib.rs:92` | Set 120s timeout on reqwest client |
| 2 | HA `timeout_secs` config is dead code | `ygg-ha/src/client.rs:48` | Apply `HaConfig::timeout_secs` to client builder |
| 3 | Semaphore `.expect()` panics in spawned tasks | `huginn/src/indexer.rs:119,302` | Return error instead of panic |
| 4 | Unbounded byte buffer in streaming proxy | `odin/src/proxy.rs:82` | Cap line buffer at 10MB |

## Sprint 2 — Resource Hardening

| # | Issue | File | Fix |
|---|-------|------|-----|
| 5 | No PgPool configuration | `ygg-store/src/lib.rs:14` | Add `PgPoolOptions` with limits |
| 6 | No engram field size limits | `mimir/src/handlers.rs:46` | Cap cause/effect at 100KB |
| 7 | No request body size limits | All services | Add `DefaultBodyLimit` layer |
| 8 | Watchdog loop never cancelled | All `main.rs` | Use `CancellationToken` |
| 9 | File watcher thread never joined | `huginn/src/watcher.rs:49` | Store JoinHandle, join on shutdown |
| 10 | Fire-and-forget tasks lost on shutdown | `mimir/src/handlers.rs:99` | Track with JoinSet or await |

## Sprint 3 — Resilience

| # | Issue | File | Fix |
|---|-------|------|-----|
| 11 | No Qdrant retry/backoff | `ygg-store/src/qdrant.rs` | Add 1-retry with backoff |
| 12 | Signal handler `.expect()` calls | All `main.rs` | Replace with `match` + log |
| 13 | HA service domain not validated | `ygg-mcp/src/tools.rs` | Add domain allowlist |
| 14 | No rate limiting | All services | Add `tower_governor` |
| 15 | Huginn watch_paths not validated | `huginn/src/main.rs:67` | Canonicalize + validate |

## Sprint 4 — Testing

| # | Issue | Fix |
|---|-------|-----|
| 16 | No integration tests | Add tests for key paths |

---

## Acceptance Criteria

- Zero `.expect()` / `.unwrap()` on fallible paths in spawned tasks
- All HTTP clients have explicit timeouts
- All services handle graceful shutdown (watchdog, background tasks)
- Input validation on all public API endpoints (size limits)
- All services compile and pass `cargo check`
