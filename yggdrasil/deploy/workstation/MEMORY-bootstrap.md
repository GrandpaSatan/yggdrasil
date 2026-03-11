# Project Memory (Bootstrap)

## Yggdrasil - Local AI Coding Homelab
- **Type:** Rust workspace (edition 2024), 11 crates
- **Purpose:** Local AI orchestrator for coding + Home Assistant
- **Status:** DEPLOYED AND RUNNING. All services auto-start on boot.
- **Topology + services:** Loaded at Phase 1 via `query_memory_tool("{project} system topology active sprint")`.

## Key Conventions
- Crate naming: `ygg-{role}` (hyphen in Cargo, underscore in imports)
- DB schema: `yggdrasil` (pgvector PG16 Docker on Munin, localhost:5432)
- Uses `sqlx::query()` with runtime binding — NOT `query!` macro
- Odin config is `node.yaml` (NOT `config.yaml`); all other services use `config.yaml`
- Rust 2024 edition: `gen` is reserved — use `rng.r#gen::<T>()`

## MCP Architecture
- **Remote:** `ygg-mcp-remote` on Munin:9093 (StreamableHTTP, always-on)
  - Tools: query_memory, store_memory, generate, list_models, search_code, get_sprint_history, ha_*
- **Local:** `ygg-mcp-server` on workstation (stdio, per IDE window)
  - Tools: sync_docs_tool only

## Note
This is a bootstrap file. Full topology and sprint history are loaded from
engram memory via MCP tools at session start (Phase 1). This file will be
updated automatically by Claude Code's auto-memory as you work.
