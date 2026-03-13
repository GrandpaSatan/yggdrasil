# Yggdrasil MCP Tools Reference

Complete reference for all 23 MCP tools exposed by the Yggdrasil ecosystem.

## Architecture

Tools are split across two servers:

| Server | Binary | Transport | Tools |
|--------|--------|-----------|-------|
| `ygg-mcp-remote` | Munin (REDACTED_MUNIN_IP:9093) | StreamableHTTP | 21 tools (all network tools) |
| `ygg-mcp-server` | Local (stdio) | stdio | 2 tools (filesystem-dependent) |

---

## Tool Status Overview

| # | Tool | Category | Status | Notes |
|---|------|----------|--------|-------|
| 1 | `search_code_tool` | Code | **Operational** | Via Muninn on Hugin |
| 2 | `query_memory_tool` | Memory | **Operational** | Via Mimir on Munin |
| 3 | `store_memory_tool` | Memory | **Operational** | SDR novelty gate (0.90) may block near-duplicates; use `id` param to bypass |
| 4 | `memory_intersect_tool` | Memory | **Operational** | SDR set operations |
| 5 | `generate_tool` | LLM | **Operational** | Via Odin → Ollama (Munin or Hugin) |
| 6 | `get_sprint_history_tool` | Sprint | **Operational** | Queries Mimir for sprint-tagged engrams |
| 7 | `list_models_tool` | LLM | **Operational** | Lists Ollama models via Odin |
| 8 | `service_health_tool` | Infra | **Operational** | Checks 6 services in parallel |
| 9 | `build_check_tool` | Dev | **Broken** | No cargo/rustup on Munin; use local `cargo check` |
| 10 | `memory_timeline_tool` | Memory | **Operational** | Temporal + tag filters |
| 11 | `context_offload_tool` | Context | **Operational** | Server-side context storage |
| 12 | `task_delegate_tool` | LLM | **Operational** | Context-enriched code generation via local LLM |
| 13 | `diff_review_tool` | LLM | **Operational** | Code review with project memory |
| 14 | `context_bridge_tool` | Context | **Operational** | Cross-IDE context export/import |
| 15 | `ast_analyze_tool` | Code | **Operational** | Tree-sitter symbol lookup via Huginn index |
| 16 | `impact_analysis_tool` | Code | **Operational** | BM25 reference search across codebase |
| 17 | `task_queue_tool` | Coordination | **Operational** | PostgreSQL-backed persistent task queue |
| 18 | `memory_graph_tool` | Memory | **Operational** | Engram relationship graph (link/traverse) |
| 19 | `ha_get_states_tool` | Home Assistant | **Operational** | Requires HA on Plume (REDACTED_CHIRP_IP) |
| 20 | `ha_list_entities_tool` | Home Assistant | **Operational** | Filter by domain |
| 21 | `ha_call_service_tool` | Home Assistant | **Operational** | Real device control — use with care |
| 22 | `ha_generate_automation_tool` | Home Assistant | **Operational** | LLM-generated automation YAML (10-60s) |
| 23 | `sync_docs_tool` | Docs (local) | **Operational** | Sprint lifecycle doc agent |
| 24 | `screenshot_tool` | Utility (local) | **Operational** | Headless Chromium page capture |

---

## Detailed Tool Reference

### 1. `search_code_tool`
**Category:** Code Search | **Server:** Remote

Search for code snippets in the indexed codebase using semantic search via Muninn's hybrid search pipeline (vector + BM25 + RRF).

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `query` | string | Yes | — | Natural language or code search query |
| `languages` | string[] | No | all | Filter by language (e.g. `["rust", "python"]`) |
| `limit` | u32 | No | 10 | Max results (max 50) |

**When to use:** Before reading files manually when searching for implementations, function usage, or verifying no orphaned code after refactoring. Prefer this over file-level grep for semantic queries.

**Example:**
```json
{ "query": "SDR encoding novelty gate", "languages": ["rust"], "limit": 5 }
```

---

### 2. `query_memory_tool`
**Category:** Memory | **Server:** Remote

Search engram memory for relevant past interactions, decisions, and stored knowledge. Returns cause/effect pairs ranked by SDR similarity.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `text` | string | Yes | — | Query text to search engram memory |
| `limit` | u32 | No | 5 | Max engrams to return (max 20) |

**When to use:** At session start (mandatory), before proposing architecture, when recalling prior decisions or debugging history. This is the primary cross-session memory tool.

**Example:**
```json
{ "text": "yggdrasil system topology services active sprint", "limit": 5 }
```

---

### 3. `store_memory_tool`
**Category:** Memory | **Server:** Remote

Store a new cause/effect memory engram. Uses SDR encoding for semantic deduplication (novelty gate at 0.90 similarity threshold).

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `cause` | string | Yes | — | The trigger or question (what happened) |
| `effect` | string | Yes | — | The outcome or answer (what resulted) |
| `tags` | string[] | No | — | Tags for categorization (e.g. `["decision", "project:yggdrasil"]`) |
| `id` | string | No | — | Engram UUID for update-by-ID (bypasses novelty gate) |

**When to use:** After completing meaningful work — sprint decisions, schema changes, deployment changes, bug root causes, or topology updates. Use `id` to update existing engrams that would otherwise be blocked by the novelty gate.

**Example:**
```json
{ "cause": "Sprint 039 planning", "effect": "Decided to add...", "tags": ["decision", "sprint", "project:yggdrasil"] }
```

---

### 4. `memory_intersect_tool`
**Category:** Memory | **Server:** Remote

Find engrams at the intersection, union, or symmetric difference of two or more concepts using SDR set operations. Returns Jaccard similarity between concepts.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `texts` | string[] | Yes | — | Two or more texts to embed and combine |
| `operation` | string | No | `"and"` | SDR operation: `"and"` (intersection), `"or"` (union), `"xor"` (difference) |
| `limit` | u32 | No | 5 | Max matching engrams (max 20) |

**When to use:** When you need to find memories that relate to multiple concepts simultaneously. Example: "What decisions involve both mesh networking AND energy management?"

**Example:**
```json
{ "texts": ["mesh networking", "energy management"], "operation": "and", "limit": 5 }
```

---

### 5. `generate_tool`
**Category:** LLM | **Server:** Remote

Generate a response from the local LLM fleet via Odin. Odin's semantic router selects the best backend model unless you specify one explicitly.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `prompt` | string | Yes | — | The prompt or question to send to the LLM |
| `model` | string | No | auto-routed | Model name (e.g. `"qwen3-coder-30b-a3b"`) |
| `max_tokens` | u64 | No | 4096 | Max tokens to generate |

**When to use:** For quick LLM queries routed through Odin. For code generation with full project context, prefer `task_delegate_tool` instead.

**Example:**
```json
{ "prompt": "Explain the difference between SDR and traditional embeddings", "max_tokens": 2048 }
```

---

### 6. `get_sprint_history_tool`
**Category:** Sprint | **Server:** Remote

Retrieve recent sprint history from engram memory. Returns sprint summaries newest first, optionally filtered by project.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `project` | string | No | all | Project name filter (e.g. `"yggdrasil"`) |
| `limit` | u32 | No | 5 | Max sprint summaries to return |

**When to use:** At session start (mandatory), before creating new sprints (verify numbering), and during QA audits.

**Example:**
```json
{ "project": "yggdrasil", "limit": 3 }
```

---

### 7. `list_models_tool`
**Category:** LLM | **Server:** Remote

List all LLM models available through Odin, including their backend assignments. Returns a markdown table.

**Parameters:** None.

**When to use:** Before calling `generate_tool` or `task_delegate_tool` to confirm the target model is loaded. Models get evicted from VRAM.

---

### 8. `service_health_tool`
**Category:** Infrastructure | **Server:** Remote

Check health of all Yggdrasil services in a single call. Probes Odin, Mimir, Muninn, Ollama (both nodes), and Qdrant in parallel.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `services` | string[] | No | all | Specific services to check: `odin`, `mimir`, `muninn`, `ollama_munin`, `ollama_hugin`, `postgres`, `qdrant` |

**When to use:** At session start to diagnose connectivity, before any tool call that depends on a specific service, and after infrastructure changes.

**Example:**
```json
{ "services": ["odin", "mimir", "qdrant"] }
```

---

### 9. `build_check_tool`
**Category:** Development | **Server:** Remote

Run `cargo check`, `clippy`, or `test --no-run` on the Yggdrasil workspace. Returns structured diagnostics with file locations.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `mode` | string | No | `"check"` | Mode: `"check"`, `"clippy"`, or `"test"` |
| `crate_name` | string | No | whole workspace | Specific crate (e.g. `"ygg-mcp"`) |

**Status: BROKEN** — No cargo/rustup installed on Munin. Use local `cargo check` via bash instead.

---

### 10. `memory_timeline_tool`
**Category:** Memory | **Server:** Remote

Query engram memory with temporal and tag filters. Supports time ranges, tag filtering, tier filtering, and optional semantic search.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `text` | string | No | — | Semantic search text (combined with filters) |
| `after` | string | No | — | ISO 8601 datetime — only engrams after this time |
| `before` | string | No | — | ISO 8601 datetime — only engrams before this time |
| `tags` | string[] | No | — | Filter: must have ALL listed tags |
| `tier` | string | No | — | Filter: `"core"`, `"recall"`, or `"archival"` |
| `limit` | u32 | No | 10 | Max results (max 50) |

**When to use:** When you need time-bounded memory queries (e.g. "What decisions were made this week?") or need to filter by specific tags.

**Example:**
```json
{ "after": "2026-03-01", "tags": ["decision", "project:yggdrasil"], "limit": 10 }
```

---

### 11. `context_offload_tool`
**Category:** Context Management | **Server:** Remote

Offload large content to the server and reference by short handle. Frees context window space when working with large files or diffs.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `action` | string | Yes | — | `"store"`, `"retrieve"`, or `"list"` |
| `content` | string | No | — | For `"store"`: the content to offload |
| `label` | string | No | — | For `"store"`: optional label |
| `handle` | string | No | — | For `"retrieve"`: handle ID to fetch |

**When to use:** When context window pressure is high — offload large file contents, search results, or diffs to the server and retrieve them later by handle.

---

### 12. `task_delegate_tool`
**Category:** LLM / Code Generation | **Server:** Remote

Delegate code generation to a local LLM with full project context. Automatically assembles context from code search + memory + reference patterns, then generates code via Ollama.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `task` | string | Yes | — | Natural language description of what to implement |
| `reference_pattern` | string | No | — | Existing code to use as a reference pattern |
| `scope_files` | string[] | No | — | Specific files/modules to focus on |
| `constraints` | string[] | No | — | Constraints (e.g. `["no unsafe", "must be async"]`) |
| `language` | string | No | inferred/rust | Target language |
| `model` | string | No | auto | Model override |
| `max_tokens` | u64 | No | 8192 | Max response tokens |

**When to use:** For boilerplate, new handlers, tests, repetitive code, and any self-contained function or module. Prefer this over `generate_tool` for code tasks — it includes project context automatically.

**Example:**
```json
{
  "task": "Add a health check endpoint that returns service version and uptime",
  "scope_files": ["crates/odin/src/handlers.rs"],
  "constraints": ["follow existing handler pattern", "must be async"]
}
```

---

### 13. `diff_review_tool`
**Category:** LLM / Code Review | **Server:** Remote

Review code changes using local LLM with full project memory. Checks for bugs, security issues, architecture violations, and performance regressions. Reviews are stored as engrams.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `content` | string | Yes | — | Git diff text or file content to review |
| `focus` | string | No | `"all"` | Review focus: `"security"`, `"performance"`, `"architecture"`, `"bugs"`, `"all"` |
| `description` | string | No | — | Description of the change's intent |

**When to use:** After significant code changes, before committing, or during QA audits. The review is informed by architectural decisions stored in memory.

---

### 14. `context_bridge_tool`
**Category:** Context Management | **Server:** Remote

Export or import context snapshots for cross-IDE continuity. Export context in one IDE, import in another to continue seamlessly.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `action` | string | Yes | — | `"export"` or `"import"` |
| `context_id` | string | No | — | For export: optional label. For import: snapshot ID |

**When to use:** When switching between IDEs (e.g. Claude Code → Firebase Studio) and need to carry context across.

---

### 15. `ast_analyze_tool`
**Category:** Code Analysis | **Server:** Remote

Look up code symbols (functions, structs, enums, traits, impls) by name, type, language, or file path. Uses Huginn's tree-sitter index.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | No | — | Symbol name (e.g. `"AppState"`, `"health_handler"`) |
| `chunk_type` | string | No | — | Type: `"function"`, `"struct"`, `"enum"`, `"impl"`, `"trait"`, `"module"` |
| `language` | string | No | — | Language: `"rust"`, `"go"`, `"python"`, `"typescript"` |
| `file_path` | string | No | — | Exact file path filter |
| `limit` | u32 | No | 20 | Max results (max 100) |

At least one filter is required.

**When to use:** When you need precise symbol lookup rather than semantic search. Faster and more exact than `search_code_tool` for known symbol names.

**Example:**
```json
{ "name": "McpServerConfig", "chunk_type": "struct", "language": "rust" }
```

---

### 16. `impact_analysis_tool`
**Category:** Code Analysis | **Server:** Remote

Find all references to a symbol across the indexed codebase using BM25 full-text search on code content. Use before refactoring to understand impact.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `symbol` | string | Yes | — | Symbol name to find references for |
| `language` | string | No | — | Language filter |
| `exclude_id` | string | No | — | UUID of definition chunk to exclude from results |
| `limit` | u32 | No | 20 | Max results (max 50) |

**When to use:** Before refactoring or deleting code — find ALL references to understand the blast radius. Part of the "Trace & Destroy" protocol.

**Example:**
```json
{ "symbol": "HaClient", "language": "rust" }
```

---

### 17. `task_queue_tool`
**Category:** Agent Coordination | **Server:** Remote

Persistent task queue backed by PostgreSQL for multi-agent coordination. Tasks survive server restarts.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `action` | string | Yes | — | `"push"`, `"pop"`, `"complete"`, `"cancel"`, `"list"` |
| `title` | string | push | — | Task title |
| `description` | string | No | — | Task description |
| `priority` | i32 | No | 0 | Higher = more urgent |
| `tags` | string[] | No | — | Categorization tags |
| `agent` | string | pop | — | Agent name claiming the task |
| `task_id` | string | complete/cancel | — | Task UUID |
| `success` | bool | No | true | Whether task succeeded (for complete) |
| `result` | string | No | — | Result or error message (for complete) |
| `project` | string | No | — | Project scope filter |
| `status` | string | list | — | Status filter: `"pending"`, `"in_progress"`, `"completed"`, `"failed"`, `"cancelled"` |
| `limit` | u32 | No | 20 | Max results for list |

**When to use:** For multi-step workflows that span agents or sessions. Push tasks during planning, pop during execution, complete when done.

---

### 18. `memory_graph_tool`
**Category:** Memory | **Server:** Remote

Manage relationships between engrams in a directed graph. Create, remove, query, and traverse edges.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `action` | string | Yes | — | `"link"`, `"unlink"`, `"neighbors"`, `"traverse"` |
| `source_id` | string | link/unlink | — | Source engram UUID |
| `target_id` | string | link/unlink | — | Target engram UUID |
| `relation` | string | No | — | `"related_to"`, `"depends_on"`, `"supersedes"`, `"caused_by"` |
| `weight` | f32 | No | 1.0 | Edge weight 0.0–1.0 |
| `engram_id` | string | neighbors | — | Engram UUID to query neighbors for |
| `direction` | string | No | `"both"` | `"outgoing"`, `"incoming"`, `"both"` |
| `start_id` | string | traverse | — | Starting UUID for BFS traversal |
| `max_depth` | u32 | No | 2 | Max BFS hops (max 5) |
| `limit` | u32 | No | 20 | Max results |

**When to use:** To build knowledge graphs over memory — link related decisions, trace dependency chains, or discover connected concepts via BFS traversal.

---

### 19. `ha_get_states_tool`
**Category:** Home Assistant | **Server:** Remote

Get all Home Assistant entity states. Returns a compact markdown summary grouped by domain or raw JSON.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `summary` | bool | No | true | `true` = markdown summary, `false` = raw JSON (first 50) |

**When to use:** Before any HA state change to verify current state. Never call `ha_call_service_tool` without checking states first.

---

### 20. `ha_list_entities_tool`
**Category:** Home Assistant | **Server:** Remote

List HA entities filtered by domain. Returns a markdown table with entity IDs, names, states, and timestamps.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `domain` | string | No | all | HA domain: `"light"`, `"switch"`, `"sensor"`, `"climate"`, `"automation"`, etc. |

**When to use:** When the user mentions a physical device — use this to discover the correct entity ID. NEVER guess entity IDs.

**Example:**
```json
{ "domain": "light" }
```

---

### 21. `ha_call_service_tool`
**Category:** Home Assistant | **Server:** Remote

Call a Home Assistant service to control a real device. **WARNING: This performs actual physical actions.**

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `domain` | string | Yes | — | Service domain (e.g. `"light"`, `"switch"`, `"climate"`) |
| `service` | string | Yes | — | Service name (e.g. `"turn_on"`, `"turn_off"`, `"toggle"`) |
| `data` | object | Yes | — | Service call data with `entity_id` and optional parameters |

**Safety protocol:** Always call `ha_get_states_tool` or `ha_list_entities_tool` FIRST to verify entity IDs and current state before calling this tool.

**Example:**
```json
{ "domain": "light", "service": "turn_on", "data": {"entity_id": "light.living_room", "brightness": 200} }
```

---

### 22. `ha_generate_automation_tool`
**Category:** Home Assistant | **Server:** Remote

Generate Home Assistant automation YAML from a natural-language description using the qwen3:30b-a3b reasoning model. Response time: 10–60 seconds.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `description` | string | Yes | — | Natural language description of the automation |

**When to use:** When the user wants to create HA automations. Always review the generated YAML before adding it to Home Assistant.

**Example:**
```json
{ "description": "Turn on living room lights at sunset and dim to 50% after 10pm" }
```

---

### 23. `sync_docs_tool` *(Local)*
**Category:** Documentation | **Server:** Local (stdio)

Sprint lifecycle documentation agent. Manages `/docs/` and `/sprints/` directories.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `event` | string | Yes | — | `"setup"`, `"sprint_start"`, or `"sprint_end"` |
| `sprint_id` | string | start/end | — | Sprint identifier (e.g. `"039"`) |
| `sprint_content` | string | start/end | — | Full sprint document content |
| `workspace_path` | string | No | config default | Workspace root override |

**Events:**
- `setup`: Initialize `/docs/` and `/sprints/`, scaffold required docs (ARCHITECTURE.md, NAMING_CONVENTIONS.md, USAGE.md)
- `sprint_start`: Auto-runs setup if needed, updates USAGE.md via LLM, checks invariants
- `sprint_end`: Archives sprint to Mimir, updates ARCHITECTURE.md, deletes sprint file

**When to use:** Mandatory at sprint lifecycle events. Called automatically by the sprint protocol.

---

### 24. `screenshot_tool` *(Local)*
**Category:** Utility | **Server:** Local (stdio)

Capture a screenshot of a web page via headless Chromium. Browser is launched lazily and reused for the session.

**Parameters:**
| Param | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `url` | string | Yes | — | Page URL to capture |
| `selector` | string | No | — | CSS selector to wait for before capture |
| `full_page` | bool | No | false | Capture full scrollable page height |
| `viewport_width` | u32 | No | 1280 | Viewport width in pixels |
| `viewport_height` | u32 | No | 720 | Viewport height in pixels |

Screenshots are saved to `/tmp/ygg-screenshots/`. Use the Read tool on the returned path to view the image.

---

## Mandatory Tool Usage Protocol

### Session Start (non-negotiable)
Run these three queries in parallel:
1. `query_memory_tool` — `"yggdrasil system topology services active sprint"`
2. `query_memory_tool` — current topic/task
3. `get_sprint_history_tool` — `project: "yggdrasil", limit: 3`

### Before Code Changes
1. `search_code_tool` — find existing patterns
2. `query_memory_tool` — check for prior architectural decisions

### Before HA Device Control
1. `ha_list_entities_tool` — discover entity IDs (never guess)
2. `ha_get_states_tool` — verify current state
3. `ha_call_service_tool` — execute action
4. `ha_get_states_tool` — post-change verification

### After Meaningful Work
- `store_memory_tool` — persist decisions, schemas, gotchas, next steps

### Sprint Lifecycle
- Sprint start: `sync_docs_tool(event: "sprint_start", ...)`
- Sprint end: `sync_docs_tool(event: "sprint_end", ...)`
