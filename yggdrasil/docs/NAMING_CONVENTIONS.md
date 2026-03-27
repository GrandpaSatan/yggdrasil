# Yggdrasil Naming Conventions

## Crates / Modules

- **Service crates:** Norse names, lowercase, no prefix. Examples: `mimir`, `odin`, `huginn`, `muninn`.
- **Library crates:** Prefixed with `ygg-`. Examples: `ygg-domain`, `ygg-store`, `ygg-embed`, `ygg-mcp`, `ygg-ha`, `ygg-config`, `ygg-server`, `ygg-voice`, `ygg-mesh`, `ygg-cloud`, `ygg-energy`, `ygg-gaming`, `ygg-sentinel`, `ygg-node`, `ygg-installer`.
- **Internal modules:** Rust `snake_case`. Examples: `handlers`, `state`, `lsh`, `error`.

## Binaries

- Binary name matches the crate name exactly. Example: crate `mimir` produces binary `mimir`.

## API Endpoints

- Base path: `/api/v1/`
- Resource names: lowercase, plural where representing collections. Examples: `/api/v1/store`, `/api/v1/query`, `/api/v1/stats`, `/api/v1/promote`.
- Health check: `/health` (no versioned prefix).

## Database

- **Schema:** `yggdrasil` (single schema for all tables).
- **Table names:** `snake_case`, plural. Examples: `engrams`, `lsh_buckets`, `indexed_files`, `code_chunks`.
- **Column names:** `snake_case`. Examples: `cause_embedding`, `content_hash`, `access_count`, `last_accessed`.
- **Index names:** `idx_<table>_<column(s)>`. Examples: `idx_engrams_tier`, `idx_engrams_cause_embedding`, `idx_chunks_search_vec`.
- **Constraint names:** Use PostgreSQL defaults (auto-generated) unless explicitly named for clarity.

## Qdrant Collections

- Collection names: `snake_case`, matching the PostgreSQL table they mirror. Examples: `engrams`, `code_chunks`.

## Environment Variables

- Prefixed with the service name in uppercase. Format: `<SERVICE>_<SETTING>`.
- Examples: `MIMIR_DATABASE_URL`, `ODIN_LISTEN_ADDR`.

## Configuration Keys (JSON / YAML)

- `snake_case` throughout. Nested objects for logical grouping.
- Format is auto-detected by file extension (`.json` default, `.yaml`/`.yml` supported).
- Supports `${ENV_VAR}` expansion for secrets (e.g., `"postgres://${DB_USER}:${DB_PASS}@localhost/yggdrasil"`).
- Examples: `listen_addr`, `database_url`, `embed.ollama_url`, `lsh.num_tables`, `tiers.recall_capacity`.

## Rust Types

- **Domain types:** `PascalCase`, descriptive. Examples: `Engram`, `NewEngram`, `MemoryTier`, `MemoryStats`, `CodeChunk`.
- **Error enums:** `PascalCase` with `Error` suffix. Examples: `DomainError`, `StoreError`, `MimirError`, `EmbedError`.
- **Config structs:** `PascalCase` with `Config` suffix. Examples: `MimirConfig`, `EmbedConfig`, `LshConfig`.

## Error Codes

- HTTP status codes follow REST conventions: 200 (OK), 201 (Created), 400 (Bad Request), 404 (Not Found), 409 (Conflict), 500 (Internal Server Error), 502 (Bad Gateway).
- Error response body: `{ "error": "human-readable message" }`.

---

Last updated: 2026-03-26
