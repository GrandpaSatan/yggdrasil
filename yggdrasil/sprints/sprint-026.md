# Sprint 026: Memory Rationalization + Project-Scoped Sessions

**Status:** Active
**Date:** 2026-03-11
**Project:** yggdrasil

---

## Goals

Rationalize the overlapping memory/context systems, make CLAUDE.md project-agnostic, introduce
project-scoped session continuity across multiple Claude Code windows, and add sprint lifecycle
tooling to automate documentation and archival.

---

## Track A: Non-Code Changes (DONE)

### A1. CLAUDE.md Rewrite
- Removed all Yggdrasil-specific content from `~/.claude/CLAUDE.md`
- Phase 1 queries now use generic `{project}` placeholders
- Added `get_sprint_history_tool` to MCP tool table
- Section 7 updated: `USAGE.md` now required in `/docs/`
- Sprint lifecycle rules added: call `sync_docs_tool` on sprint start/end
- Section 8 (Yggdrasil topology) deleted — lives in engrams only

### A2. MEMORY.md Cleanup
- Trimmed `~/.claude/projects/.../memory/MEMORY.md` to ~120 lines
- Removed topology tables (now in Mimir engrams)
- Created `memory/sprint-history.md` for sprint 015–023 archive

### A3. Sprint Archival
- All 15 sprint files (000–023) archived to Mimir engrams via `store_memory_tool`
- Tags: `["sprint", "project:yggdrasil"]`
- All sprint files deleted from `/sprints/`

### A4. USAGE.md Creation (this task)
- New `/docs/USAGE.md` covering all service endpoints, startup commands, deploy commands

### A5. Topology Engram Update
- New engram stored (UUID: 57ad3148) with Sprint 026 as active, SSHFS details, archival note

---

## Track B: Code Changes (DONE)

### B1. `get_sprint_history` MCP Tool
- **File:** `crates/ygg-mcp/src/tools.rs`
- New `GetSprintHistoryParams { project: Option<String>, limit: Option<u32> }`
- `get_sprint_history()` — queries Odin `/api/v1/query` with "{project} sprint history"
- Filters by cause starting with "Sprint " (archival convention)
- Returns markdown list newest-first

### B2. Project-Scoped Session Tracking
- **`crates/ygg-domain/src/config.rs`** — Added `project: Option<String>` and `workspace_path: Option<String>` to `McpServerConfig`
- **`crates/odin/src/openai.rs`** — Added `project_id: Option<String>` to `ChatCompletionRequest`
- **`crates/odin/src/session.rs`** — New `SessionSummary` struct; `ConversationSession` gains `project_id`; `SessionStore` gains `project_sessions: Arc<DashMap<String, VecDeque<SessionSummary>>>`. `resolve()` takes `project_id` param. `set_summary()` pushes to project ring buffer. `reap_expired()` moves evicted session summaries to project store.
- **`crates/odin/src/context.rs`** — Added `previous_sessions: Option<&str>` param to `ContextBudget::pack()`. New priority slot 5 (capped 500 tokens) before older history.
- **`crates/odin/src/handlers.rs`** — Passes `project_id` to `resolve()`, loads project history, passes to `pack()`.
- **`configs/mcp-server/config.yaml`** — Added `project: "yggdrasil"` and `workspace_path: "./yggdrasil"`

### B3. `sync_docs` MCP Tool
- **File:** `crates/ygg-mcp/src/tools.rs`
- New `SyncDocsParams { event: String, sprint_id: String, sprint_content: String }`
- `sync_docs()` dispatches to `sync_docs_sprint_start()` or `sync_docs_sprint_end()`
- **sprint_start:** Reads USAGE.md, calls Odin (Qwen3-Coder) to update it, writes back, checks /docs/ + /sprints/ invariants
- **sprint_end:** Generates sprint summary via Odin, stores in Mimir with sprint tags, appends ARCHITECTURE.md delta, deletes sprint file
- **`crates/ygg-mcp/src/server.rs`** — Registered `get_sprint_history_tool` and `sync_docs_tool`

---

## API Changes

### New Odin Request Field
- `POST /v1/chat/completions` now accepts `project_id: Optional[String]`
- When provided, Odin injects previous project session summaries as lowest-priority context (slot 5, capped 500 tokens)

### New MCP Tools
- `get_sprint_history_tool(project?, limit?)` — retrieve sprint history from Mimir
- `sync_docs_tool(event, sprint_id, sprint_content)` — sprint lifecycle doc agent

---

## Deployment

- Built: `odin` (Munin) + `ygg-mcp-server` (workstation)
- Deployed odin to Munin via rsync + `systemctl restart yggdrasil-odin`
- MCP server binary updated at `/home/jesus/.local/bin/ygg-mcp-server`
- **Restart required:** Claude Code MCP server must be restarted to pick up new tools

---

## Verification Checklist

- [ ] New Claude Code session: MEMORY.md loads without truncation
- [ ] `query_memory_tool("yggdrasil topology")` returns current node IPs/ports
- [ ] `get_sprint_history_tool(project: "yggdrasil")` returns archived sprints
- [ ] `/sprints/` contains exactly one file (sprint-026.md)
- [ ] `/docs/` contains ARCHITECTURE.md + NetworkHardware.md + NAMING_CONVENTIONS.md + USAGE.md
- [ ] `generate_tool` with two Claude windows → both sessions share Odin project context
- [ ] New session with `project_id: "yggdrasil"` → previous session summaries in context
