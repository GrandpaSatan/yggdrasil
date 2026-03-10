# QA Compliance Auditor Memory

## Project: Yggdrasil
- Workspace root: `/home/jesus/Documents/HardwareSetup/yggdrasil/`
- Language: Rust (edition 2024)
- Build system: Cargo workspace with 9 crates
- Sprint docs: `/sprints/sprint_NNN_*.md`

## Crate Architecture
- `ygg-domain`: Leaf crate, no I/O. All types (Engram, config structs, errors).
- `ygg-store`: PostgreSQL (sqlx) + Qdrant client. Owns all DB I/O.
- `ygg-embed`: Ollama HTTP client for embeddings.
- `mimir`: Axum HTTP server for engram memory CRUD.
- Service crates: odin, huginn, muninn, ygg-mcp, ygg-ha.

## Testing Patterns
- No workspace-level `/tests/` directory exists. Use `#[cfg(test)] mod tests` in source files.
- External dependencies (PostgreSQL, Qdrant, Ollama) require integration test infrastructure that does not exist yet.
- LSH module is fully testable in isolation (no I/O deps).
- Handlers and state modules require mocking of external services for unit tests.

## Known Findings (Sprint 002)
- See `sprint_002_findings.md` for detailed findings.
- SHA-256 uses `\n` separator (improvement over spec's raw concatenation).
- LSH backfill runs synchronously in AppState::new(), blocking server bind (contradicts sprint doc decision log).
- Comment in state.rs line 75-76 is incorrect about backfill timing.
- `is_empty()` method on LshIndex is not in sprint spec but is required by clippy for `len()` impl.

## Fergus Client Contract
- File: `/home/jesus/Documents/Rust/Fergus_Agent/fergus-rs/crates/fergus-server/src/engram_client.rs`
- Expects: `Vec<Engram>` flat array from query, `StoreResponse { id }` from store.
- Uses `#[serde(default)]` on `similarity` field -- tolerates missing/extra fields.
- Circuit breaker with 3-failure threshold, 30s cooldown.
