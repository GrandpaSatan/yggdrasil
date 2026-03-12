# Sprint 029: Parent AI Enhancement Layer

**Status:** Active
**Date:** 2026-03-11
**Project:** yggdrasil

---

## Vision

Transform Yggdrasil from an internal memory system into **the MCP server that makes any AI coding environment smarter**. Five pillars:

1. **Session Resilience** — COMPLETE (Sprint 028 carryover)
2. **Foundation Tools** — service_health, build_check, memory_timeline, context_offload
3. **Task Delegation** — Parent AI delegates code generation to local LLMs with full project context
4. **Intelligent Review** — Diff review with memory-aware code analysis + compile verification
5. **Firebase Studio Integration** — Expose MCP server externally, document `.idx/mcp.json` config

After this sprint, Yggdrasil works with Claude Code, Firebase Studio (Antigravity), Cursor, and any MCP-capable IDE — all sharing persistent memory and local compute.

---

## Phase A: Session Resilience — COMPLETE

Implemented in previous session:
- Migration `004_sessions.up.sql` — sessions table with 24h TTL, project context carryover
- `ygg-store/src/postgres/sessions.rs` — full CRUD (create, get, touch, update_state, cleanup, delete, get_latest_for_project)
- `ygg-mcp-remote/src/session_manager.rs` — PersistentSessionManager wrapping rmcp LocalSessionManager
- Background cleanup task (5min interval, 24h TTL)
- Conditional: uses PG if `database_url` configured, falls back to in-memory otherwise

**Remaining issue:** Client-side reconnect after restart still requires IDE restart. This is an rmcp/client limitation, not fixable server-side. Documented as known behavior.

---

## Phase E: Foundation Tools (moved up — all low effort, high compound value)

### E1. `service_health_tool`
**Problem:** Phase 1 init fails silently when services are down (Mimir 500s, Muninn empty results). No way to know what's broken without calling each tool.

**Solution:** Single tool that probes all services in parallel with 2s timeout.

**File:** `crates/ygg-mcp/src/tools.rs`

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ServiceHealthParams {
    /// Optional: only check specific services (default: all)
    #[serde(default)]
    pub services: Option<Vec<String>>,
}
```

Probes: Odin `/health`, Mimir `/health`, Muninn `/health`, Ollama `/api/tags` (both backends), PostgreSQL (connection test), Qdrant `/collections`.

Returns markdown table: service, status (up/down/degraded), latency_ms, error.

### E2. `build_check_tool`
**Problem:** After code generation, no way to verify compilation without leaving the AI workflow.

**Solution:** Run `cargo check --message-format=json` on the workspace, return structured diagnostics.

**File:** `crates/ygg-mcp/src/tools.rs`

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuildCheckParams {
    /// "check", "clippy", or "test" (default: "check")
    #[serde(default)]
    pub mode: Option<String>,
    /// Optional: specific crate to check (default: whole workspace)
    #[serde(default)]
    pub crate_name: Option<String>,
}
```

Runs on the workspace host (Munin or local). Returns: pass/fail, error count, warning count, structured diagnostics with file:line:col and suggested fixes.

### E3. `memory_timeline_tool`
**Problem:** `query_memory_tool` has no concept of time. Can't ask "what changed since last week?" or "decisions from sprint 027?"

**Solution:** Add temporal + tag filters to engram queries.

**File:** `crates/ygg-mcp/src/tools.rs` + new Mimir endpoint `POST /api/v1/timeline`

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryTimelineParams {
    /// Optional: semantic search text (combined with time filters)
    #[serde(default)]
    pub text: Option<String>,
    /// ISO 8601 datetime — only engrams after this time
    #[serde(default)]
    pub after: Option<String>,
    /// ISO 8601 datetime — only engrams before this time
    #[serde(default)]
    pub before: Option<String>,
    /// Filter: engrams must have ALL of these tags
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Filter: memory tier (core/recall/archival)
    #[serde(default)]
    pub tier: Option<String>,
    /// Max results (default 10, max 50)
    #[serde(default)]
    pub limit: Option<u32>,
}
```

### E4. `context_offload_tool`
**Problem:** AI context windows are finite. Large file contents, diffs, and search results consume tokens that could be used for reasoning.

**Solution:** Server-side key-value store. Store large text → get short handle. Retrieve by handle when needed.

**File:** `crates/ygg-mcp/src/tools.rs` + new Mimir endpoint `POST /api/v1/context`

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextOffloadParams {
    /// "store", "retrieve", or "list"
    pub action: String,
    /// For store: the content to offload
    #[serde(default)]
    pub content: Option<String>,
    /// For store: optional label
    #[serde(default)]
    pub label: Option<String>,
    /// For retrieve: the handle ID
    #[serde(default)]
    pub handle: Option<String>,
}
```

Session-scoped TTL (dies with session). Stored in PG (`yggdrasil.context_offloads` table).

### Verification
- [ ] `service_health_tool` returns status for all 7+ services
- [ ] `build_check_tool(mode: "check")` returns structured cargo diagnostics
- [ ] `build_check_tool(mode: "clippy")` returns lint warnings
- [ ] `memory_timeline_tool(after: "2026-03-01", tags: ["decision"])` filters correctly
- [ ] `context_offload_tool(action: "store", content: <10KB text>)` returns handle
- [ ] `context_offload_tool(action: "retrieve", handle: "...")` returns original content
- [ ] Context offloads expire with session

---

## Phase B: Task Delegation Tool

### Problem
Parent AIs (Claude Opus, Gemini Pro) are expensive per token. Heavy code generation burns API credits. Yggdrasil has two local Qwen3-30B instances sitting idle most of the time.

### Solution
`task_delegate_tool` — Parent AI describes what to build, Yggdrasil assembles full context (code search + memory + existing patterns) and delegates to local LLM. Parent AI reviews the result. Heavy lifting is free.

### Changes

#### B1. `TaskDelegateParams` and handler
**File:** `crates/ygg-mcp/src/tools.rs`

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskDelegateParams {
    /// Natural language description of what to implement
    pub task: String,
    /// Optional: existing code to use as reference pattern
    #[serde(default)]
    pub reference_pattern: Option<String>,
    /// Optional: specific files/modules to focus on
    #[serde(default)]
    pub scope_files: Option<Vec<String>>,
    /// Optional: constraints ("no unsafe", "must be async", "follow existing error handling")
    #[serde(default)]
    pub constraints: Option<Vec<String>>,
    /// Optional: target language (default: infer from scope_files or "rust")
    #[serde(default)]
    pub language: Option<String>,
    /// Optional: model override
    #[serde(default)]
    pub model: Option<String>,
    /// Max tokens for response (default: 8192)
    #[serde(default)]
    pub max_tokens: Option<u64>,
}
```

#### B2. Implementation pipeline
1. **Context assembly** (parallel):
   - `search_code(task)` → relevant code from Muninn
   - `query_memory(task)` → architectural decisions from Mimir
   - If `scope_files` → fetch from Muninn search
   - If `reference_pattern` → include verbatim
2. **Prompt construction** with assembled context
3. **Delegate** to local LLM via Odin `/v1/chat/completions`
4. **Return** generated code + metadata (model, context sources, token count)

### Verification
- [ ] Returns compilable Rust for "implement a health check endpoint"
- [ ] `reference_pattern` causes output to match given style
- [ ] Falls back gracefully if Muninn or Mimir unreachable
- [ ] Works from both Claude Code and Firebase Studio

---

## Phase C: Diff Review Tool

### Problem
AI IDEs generate code, but review is manual. No review tool has access to project memory.

### Solution
`diff_review_tool` — Takes a diff or file content, reviews using local LLM with full project context.

### Changes

#### C1. `DiffReviewParams` and handler
**File:** `crates/ygg-mcp/src/tools.rs`

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiffReviewParams {
    /// Git diff text, or file content to review
    pub content: String,
    /// Review focus: "security", "performance", "architecture", "bugs", "all" (default: "all")
    #[serde(default)]
    pub focus: Option<String>,
    /// Description of the change's intent
    #[serde(default)]
    pub description: Option<String>,
}
```

#### C2. Pipeline
1. Extract identifiers from diff (file paths, function names)
2. Parallel fetch: `search_code` + `query_memory` for context
3. Construct review prompt with focus area
4. Delegate to local LLM
5. Store review as engram (for future reference)
6. Return structured review (issues with severity, architecture alignment, suggestions)

### Verification
- [ ] Reviews a git diff and identifies real bugs
- [ ] Flags architectural violations based on memory context
- [ ] Review stored as engram for future reference

---

## Phase D: Firebase Studio Integration

### Problem
User works across Claude Code and Firebase Studio (Antigravity). Each IDE starts fresh.

### Solution
1. Expose MCP server externally via Cloudflare Tunnel with auth
2. Document Firebase Studio config
3. `context_bridge_tool` for cross-IDE context transfer

### Changes

#### D1. External Access
- Set up Cloudflare Tunnel for `ygg-mcp-remote` on Munin:9093
- Add Bearer token authentication to ygg-mcp-remote
- Document tunnel setup in USAGE.md

#### D2. Firebase Studio Configuration
**File:** `.idx/mcp.json` (in project root when opened in Firebase Studio)

```json
{
  "mcpServers": {
    "yggdrasil": {
      "url": "https://<tunnel-domain>/mcp",
      "headers": {
        "Authorization": "Bearer <token>"
      }
    }
  }
}
```

#### D3. `context_bridge_tool`
**File:** `crates/ygg-mcp/src/tools.rs`

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextBridgeParams {
    /// "export" or "import"
    pub action: String,
    /// For export: label. For import: context ID to import.
    #[serde(default)]
    pub context_id: Option<String>,
}
```

- **Export**: Gather session SDR, recent engrams, active sprint. Store as tagged engram. Return snapshot ID.
- **Import**: Fetch snapshot, restore session SDR, inject context. Return summary.

### Verification
- [ ] Firebase Studio connects to yggdrasil MCP and lists all tools
- [ ] Bearer token auth blocks unauthorized access
- [ ] Context export/import round-trips across IDEs

---

## Implementation Order

```
Phase E (Foundation Tools)       ← Low effort, immediate compound value
  ├─ E1: service_health_tool
  ├─ E2: build_check_tool
  ├─ E3: memory_timeline_tool
  └─ E4: context_offload_tool
  ↓
Phase B (Task Delegate)          ← Highest user value, uses full stack
  ↓
Phase C (Diff Review)            ← Incremental on B architecture
  ↓
Phase D (Firebase Studio)        ← External access + context bridge
```

## Files Modified (estimated)

| File | Phase | Change |
|:---|:---|:---|
| `crates/ygg-mcp/src/tools.rs` | E,B,C,D | 7 new tool handlers |
| `crates/ygg-mcp/src/server.rs` | E,B,C,D | Register 7 new tools |
| `crates/mimir/src/handlers.rs` | E | timeline + context endpoints |
| `crates/mimir/src/main.rs` | E | Register new routes |
| `migrations/005_context_offloads.up.sql` | E | Context offload table |
| `crates/ygg-store/src/postgres.rs` | E | Context offload queries |
| `crates/ygg-mcp-remote/src/main.rs` | D | Bearer token auth middleware |
| `docs/USAGE.md` | D | Firebase Studio setup docs |

## Known Risks

| Risk | Mitigation |
|------|-----------|
| `build_check_tool` needs Rust toolchain on MCP host | Munin has toolchain; run via SSH if needed |
| Qwen3-30B may produce low-quality code | Allow model override; parent AI retries |
| Muninn search_code returns 0 results (stale index) | Task delegate falls back to memory-only context |
| Cloudflare Tunnel setup complexity | Start with local-network-only; tunnel is Phase D stretch goal |
| Firebase Studio StreamableHTTP compatibility | Test early; fall back to stdio proxy via tunnel |
