# Sprint: 006 - MCP Integration
## Status: DONE

## Objective

Expose Yggdrasil's capabilities to MCP-aware IDE clients (Claude Code, VS Code Copilot Chat, Cursor, etc.) by building an MCP server using the official `rmcp` Rust SDK (v1.1) with `schemars` 1.x for JSON Schema generation. The server runs as a standalone stdio binary (`ygg-mcp-server`) that communicates with Odin, Mimir, Muninn, and Home Assistant over HTTP. It provides 9 MCP tools (5 core + 4 HA) and 2 MCP resources, making code search, engram memory, LLM generation, and Home Assistant control available as first-class MCP capabilities without modifying any existing service's process boundary.

## Scope

### In Scope
- Binary crate `crates/ygg-mcp-server/` producing the `ygg-mcp-server` executable
- Library crate `crates/ygg-mcp/` containing all tool/resource/server logic
- MCP transport: **stdio** (JSON-RPC 2.0 over stdin/stdout)
- **Five core MCP tools:**
  1. `search_code` -- semantic code search via Muninn POST /api/v1/search (direct, not through Odin)
  2. `query_memory` -- engram recall via Odin POST /api/v1/query (which proxies to Mimir)
  3. `store_memory` -- engram creation via Odin POST /api/v1/store (which proxies to Mimir)
  4. `generate` -- LLM chat completion via Odin POST /v1/chat/completions (non-streaming)
  5. `list_models` -- model listing via Odin GET /v1/models
- **Four HA MCP tools** (conditionally registered when `config.ha` is present):
  6. `ha_get_states` -- get all HA entity states, formatted as domain-grouped markdown or raw JSON
  7. `ha_list_entities` -- list HA entities filtered by domain, formatted as markdown table
  8. `ha_call_service` -- call an HA service with domain allowlist enforcement
  9. `ha_generate_automation` -- generate HA automation YAML via Odin's reasoning model
- **Two MCP resources:**
  1. `yggdrasil://models` -- model listing (delegates to `list_models` tool logic)
  2. `yggdrasil://memory/stats` -- engram tier statistics (graceful fallback if endpoint unavailable)
- Tool implementations in `crates/ygg-mcp/src/tools.rs` with `schemars::JsonSchema` parameter structs
- Resource implementations in `crates/ygg-mcp/src/resources.rs`
- MCP `ServerHandler` impl in `crates/ygg-mcp/src/server.rs` using `#[tool_router]` and `#[tool]` macros
- `McpServerConfig` struct in `ygg_domain::config` with `odin_url`, `muninn_url` (Option), `timeout_secs`, `ha` (Option<HaConfig>)
- Config file: `configs/mcp-server/config.yaml`
- CLI with `--config` flag and `YGG_MCP_CONFIG` env var override
- `rmcp` 1.1 workspace dependency with `features = ["server", "transport-io"]`
- `schemars` 1.x workspace dependency (must match rmcp's re-exported version)
- Structured tracing to **stderr** (stdout is the JSON-RPC channel)
- HA domain allowlist in `ha_call_service` for safety (19 allowed domains, `lock` excluded)
- Input size validation: 100KB max for search/memory fields, 1MB max for generate prompts
- Unit tests for HA domain allowlist enforcement

### Out of Scope
- WebSocket or SSE MCP transport (stdio is sufficient for IDE use)
- Embedding the MCP server inside Odin's process
- Streaming tool responses (MCP tools return complete results)
- MCP prompts (no predefined prompt templates)
- MCP sampling (client-side concern)
- Authentication (private LAN)
- Mimir `/api/v1/stats` endpoint (resource returns "not available" gracefully)
- HA automation YAML validation (user reviews before applying)

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** I/O-bound. The MCP server is a thin stdio-to-HTTP bridge. It reads JSON-RPC from stdin, makes HTTP calls to Odin/Mimir/Muninn/HA, and writes JSON-RPC responses to stdout. CPU usage is negligible.
- **Target Hardware:** Munin (REDACTED_MUNIN_IP) -- Intel Core Ultra 185H (6P+8E+2LP cores, 16 threads), 48GB DDR5. The MCP server runs on the same host as Odin for lowest latency to the orchestrator. Launched as a subprocess by the IDE client on any machine with SSH access or local binary.
- **Utilization Plan:**
  - Single-threaded Tokio runtime is sufficient (`#[tokio::main]` with default scheduler). MCP protocol is request-response over stdio, not multiplexed.
  - Memory footprint: < 20MB RSS idle, < 40MB during tool execution. The server holds one `reqwest::Client` (connection pool) and processes one tool call at a time.
  - Largest in-memory object: `search_code` response (up to 50 chunks at ~2KB each = ~100KB peak), or `ha_get_states` response (all HA entities, typically < 500 entities at ~200 bytes each = ~100KB).
  - Network: HTTP calls to Odin at `REDACTED_MUNIN_IP:8080` (< 1ms if local, < 2ms from LAN), Muninn at `REDACTED_HUGIN_IP:9091` (< 2ms LAN), HA at `REDACTED_CHIRP_IP:8123` (< 5ms cross-VLAN).
  - Co-location with Odin and Mimir on Munin. Combined RSS of all three services < 200MB, well within 48GB available.
- **Fallback Strategy:** No hardware-specific optimizations. The binary runs identically on any platform that supports Rust's `std::io::stdin/stdout`. On a different workstation, only HTTP call latency to backend services changes.

## Performance Targets

| Metric | Target | Measurement Method |
|--------|--------|--------------------|
| `search_code` P95 end-to-end | < 500ms | `tracing` span from JSON-RPC request to response write |
| `query_memory` P95 end-to-end | < 300ms | Same |
| `store_memory` P95 end-to-end | < 300ms | Same |
| `generate` P95 (excluding model inference) | < 100ms overhead | Difference between MCP server E2E and Odin non-streaming E2E |
| `list_models` P95 | < 200ms | `tracing` span |
| `ha_get_states` P95 | < 2000ms | `tracing` span (HA REST API can be slow) |
| `ha_list_entities` P95 | < 2000ms | Same |
| `ha_call_service` P95 | < 1000ms | `tracing` span |
| `ha_generate_automation` P95 (excluding LLM) | < 5000ms | `tracing` span (fetches entities + services + LLM call) |
| Resource read P95 | < 200ms | `tracing` span |
| Memory ceiling (idle) | < 20MB RSS | `/proc/self/status` VmRSS |
| Memory ceiling (during tool call) | < 40MB RSS | Same |
| Startup time | < 500ms | Wall clock from process start to MCP `initialize` response sent |

## Data Schemas

### MCP Tool: `search_code`

Input schema (JSON Schema, generated by `schemars::JsonSchema` derive):
```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Natural language or code search query"
    },
    "languages": {
      "type": ["array", "null"],
      "items": { "type": "string" },
      "description": "Optional filter by programming language (e.g., [\"rust\", \"python\"])"
    },
    "limit": {
      "type": ["integer", "null"],
      "description": "Maximum number of results (default 10, max 50)",
      "default": 10
    }
  },
  "required": ["query"]
}
```

Rust parameter struct (in `ygg-mcp/src/tools.rs`):
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchCodeParams {
    pub query: String,
    pub languages: Option<Vec<String>>,
    #[serde(default = "default_search_limit")]
    pub limit: Option<u32>,
}
```

Internal HTTP call: `POST http://<muninn_url>/api/v1/search`
```json
{
  "query": "<query>",
  "languages": ["rust"],
  "limit": 10
}
```

Tool response content (MCP `TextContent`):
```
## Code Search Results for: "<query>"

### 1. src/main.rs (rust) [score: 0.87]
```rust
fn main() { ... }
```

### 2. src/lib.rs (rust) [score: 0.82]
...
```

Error responses: `is_error: true` with descriptive message for Muninn unreachable, timeout, input too large (>100KB), or missing `muninn_url` config.

### MCP Tool: `query_memory`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "text": {
      "type": "string",
      "description": "Query text to search engram memory"
    },
    "limit": {
      "type": ["integer", "null"],
      "description": "Maximum number of engrams to return (default 5, max 20)",
      "default": 5
    }
  },
  "required": ["text"]
}
```

Rust parameter struct:
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct QueryMemoryParams {
    pub text: String,
    #[serde(default = "default_query_limit")]
    pub limit: Option<u32>,
}
```

Internal HTTP call: `POST http://<odin_url>/api/v1/query`
```json
{ "text": "<text>", "limit": 5 }
```

Tool response content:
```
## Memory Results for: "<text>"

1. **Cause:** User asked about Tokio runtime configuration
   **Effect:** Explained multi-threaded vs current-thread schedulers
   **Similarity:** 0.91
```

### MCP Tool: `store_memory`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "cause": { "type": "string", "description": "The trigger or question (what happened)" },
    "effect": { "type": "string", "description": "The outcome or answer (what resulted)" },
    "tags": {
      "type": ["array", "null"],
      "items": { "type": "string" },
      "description": "Optional tags for categorization (accepted for forward-compat, currently dropped)"
    }
  },
  "required": ["cause", "effect"]
}
```

Rust parameter struct:
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StoreMemoryParams {
    pub cause: String,
    pub effect: String,
    pub tags: Option<Vec<String>>,
}
```

Internal HTTP call: `POST http://<odin_url>/api/v1/store`
```json
{ "cause": "<cause>", "effect": "<effect>" }
```

Note: `tags` are accepted for forward-compatibility but silently dropped. The Mimir schema has no tags column.

Tool response: `"Memory stored successfully. ID: <uuid>"`

### MCP Tool: `generate`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "prompt": { "type": "string", "description": "The prompt or question to send to the LLM" },
    "model": {
      "type": ["string", "null"],
      "description": "Model name (e.g., 'qwen3-coder-30b-a3b', 'qwen3:30b-a3b'). If omitted, uses Odin's default routing."
    },
    "max_tokens": {
      "type": ["integer", "null"],
      "description": "Maximum tokens to generate (optional, default 4096)"
    }
  },
  "required": ["prompt"]
}
```

Rust parameter struct:
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GenerateParams {
    pub prompt: String,
    pub model: Option<String>,
    pub max_tokens: Option<u64>,
}
```

Internal HTTP call: `POST http://<odin_url>/v1/chat/completions`
```json
{
  "model": "<model or null>",
  "messages": [{ "role": "user", "content": "<prompt>" }],
  "stream": false,
  "max_tokens": 4096
}
```

Tool response: raw `choices[0].message.content` string from Odin.

Input validation: prompt max 1MB (`MAX_PROMPT_BYTES`).

### MCP Tool: `list_models`

Input schema: `{ "type": "object", "properties": {}, "required": [] }` (no parameters)

Internal HTTP call: `GET http://<odin_url>/v1/models`

Tool response:
```
## Available Models

| Model | Backend |
|-------|---------|
| qwen3-coder:30b-a3b-q4_K_M | ollama:munin |
| qwen3:30b-a3b | ollama:hugin |
```

### MCP Tool: `ha_get_states`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "summary": {
      "type": "boolean",
      "description": "If true (default), return markdown summary grouped by domain. If false, return raw JSON (first 50 entities).",
      "default": true
    }
  },
  "required": []
}
```

Rust parameter struct:
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HaGetStatesParams {
    #[serde(default = "default_true")]
    pub summary: bool,
}
```

Internal HTTP call: `GET http://<ha_url>/api/states` (bearer token auth)

Tool response (summary mode): Domain-grouped markdown tables with domain-specific columns:
- `light`: Entity, State, Brightness
- `sensor`/`binary_sensor`: Entity, State, Unit
- Other domains: Entity, State

Error: `"Home Assistant is not configured."` when `config.ha` is None.

### MCP Tool: `ha_list_entities`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "domain": {
      "type": ["string", "null"],
      "description": "HA domain to filter by (e.g., 'light', 'switch', 'sensor'). Omit for all."
    }
  },
  "required": []
}
```

Internal HTTP call: `GET http://<ha_url>/api/states` (fetches all, filters client-side by entity_id prefix)

Tool response: Markdown table with Entity ID, Friendly Name, State, Last Changed columns.

### MCP Tool: `ha_call_service`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "domain": { "type": "string", "description": "HA service domain (e.g., 'light', 'switch', 'climate')" },
    "service": { "type": "string", "description": "Service name (e.g., 'turn_on', 'turn_off', 'toggle')" },
    "data": { "type": "object", "description": "Service call data JSON" }
  },
  "required": ["domain", "service", "data"]
}
```

Internal HTTP call: `POST http://<ha_url>/api/services/{domain}/{service}` (bearer token auth, JSON body)

**Domain allowlist** (security measure):
```
light, switch, cover, fan, media_player, scene, script, input_boolean,
input_number, input_select, input_text, automation, climate, vacuum,
button, number, select, humidifier, water_heater
```

Domains NOT allowed: `lock`, `alarm_control_panel`, `camera`, `device_tracker`, `person`, `zone`, `notify`, `persistent_notification`, `system_log`, `homeassistant`.

Tool response on success: `"Service called successfully: {domain}.{service}\n\nData sent:\n{pretty_json}"`

### MCP Tool: `ha_generate_automation`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "description": { "type": "string", "description": "Natural language description of the desired automation" }
  },
  "required": ["description"]
}
```

Execution flow:
1. Fetch entity states from HA (`GET /api/states`) -- builds entity context grouped by domain (max 200 per domain)
2. Fetch available services from HA (`GET /api/services`) -- builds service context
3. Construct system prompt with entity/service summaries and generation rules
4. POST to Odin `/v1/chat/completions` with model `qwq-32b` (non-streaming)
5. Extract YAML from fenced code block in response (falls back to raw content if no fence)

Tool response: `"## Generated Automation\n\n```yaml\n{yaml}\n```\n\nNote: Review this automation carefully..."`

**Known discrepancy:** The `AutomationGenerator` is hardcoded to request model `qwq-32b`, but the actual model on Hugin is now `qwen3:30b-a3b` (Sprint 013 replaced QwQ). Odin's routing may handle this via model name mapping, or the model name may need updating. See Risks section.

### MCP Resource: `yggdrasil://models`

URI: `yggdrasil://models`
MIME type: `text/plain`
Content: Same markdown table as `list_models` tool output (shared via `models_table()` function).

### MCP Resource: `yggdrasil://memory/stats`

URI: `yggdrasil://memory/stats`
MIME type: `text/plain`
Content: Attempts `GET http://<odin_url>/api/v1/stats`. If endpoint returns valid JSON with `total`, `recall_count`, `archive_count` fields, formats as markdown table. Otherwise returns `"Memory statistics not available."` gracefully.

### Configuration: `McpServerConfig`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default = "default_odin_url")]
    pub odin_url: String,               // default: "http://localhost:8080"
    #[serde(default)]
    pub muninn_url: Option<String>,      // direct Muninn URL for search_code
    #[serde(default = "default_mcp_timeout")]
    pub timeout_secs: u64,              // default: 30
    #[serde(default)]
    pub ha: Option<HaConfig>,           // optional HA integration
}
```

Deployed config file `configs/mcp-server/config.yaml`:
```yaml
odin_url: "http://REDACTED_MUNIN_IP:8080"
muninn_url: "http://REDACTED_HUGIN_IP:9091"
timeout_secs: 300
ha:
  url: "http://REDACTED_CHIRP_IP:8123"
  token: "${HA_TOKEN}"
  timeout_secs: 10
```

Note: `timeout_secs: 300` (5 minutes) accommodates slow `ha_generate_automation` calls where the reasoning model may take 60+ seconds. The `${HA_TOKEN}` placeholder requires env var expansion in the config loader or pre-processing.

## API Contracts

### MCP Protocol (JSON-RPC 2.0 over stdio)

| MCP Method | Handler | Description |
|------------|---------|-------------|
| `initialize` | `rmcp` built-in | Returns server info with capabilities (tools, resources) |
| `tools/list` | `rmcp` `#[tool_router]` | Returns 9 tool definitions with JSON schemas (5 core + 4 HA) |
| `tools/call` | `rmcp` `#[tool_router]` dispatch | Dispatches to tool handler by name |
| `resources/list` | `ServerHandler::list_resources()` | Returns 2 resource URIs |
| `resources/read` | `ServerHandler::read_resource()` | Returns resource content by URI |

### Server Info (returned in `initialize` response)

```json
{
  "name": "yggdrasil",
  "version": "0.1.0",
  "capabilities": {
    "tools": {},
    "resources": {}
  }
}
```

### HTTP Calls Made by ygg-mcp-server

| Target | Endpoint | Called By | Purpose |
|--------|----------|-----------|---------|
| Odin (REDACTED_MUNIN_IP:8080) | `POST /v1/chat/completions` | `generate` tool, `ha_generate_automation` tool (via AutomationGenerator) | Non-streaming LLM generation |
| Odin (REDACTED_MUNIN_IP:8080) | `GET /v1/models` | `list_models` tool, `yggdrasil://models` resource | Model listing |
| Odin (REDACTED_MUNIN_IP:8080) | `POST /api/v1/query` | `query_memory` tool | Engram search (Mimir proxy) |
| Odin (REDACTED_MUNIN_IP:8080) | `POST /api/v1/store` | `store_memory` tool | Engram creation (Mimir proxy) |
| Odin (REDACTED_MUNIN_IP:8080) | `GET /api/v1/stats` | `yggdrasil://memory/stats` resource | Memory stats (may not exist) |
| Muninn (REDACTED_HUGIN_IP:9091) | `POST /api/v1/search` | `search_code` tool | Semantic code search |
| HA (REDACTED_CHIRP_IP:8123) | `GET /api/states` | `ha_get_states`, `ha_list_entities`, `ha_generate_automation` | Entity state retrieval |
| HA (REDACTED_CHIRP_IP:8123) | `GET /api/services` | `ha_generate_automation` | Service discovery |
| HA (REDACTED_CHIRP_IP:8123) | `POST /api/services/{d}/{s}` | `ha_call_service` | Device control |

### Error Handling

MCP tool errors are returned as `CallToolResult` with `is_error: true` and a human-readable error message. The MCP server never panics on bad input or downstream HTTP failures.

| Scenario | MCP Response |
|----------|-------------|
| Odin unreachable | `is_error: true`, "Odin is not reachable at {url}. Ensure the Yggdrasil orchestrator is running. Error: {e}" |
| Muninn unreachable | `is_error: true`, "Code search unavailable. Muninn is not reachable at {url}: {e}" |
| `muninn_url` not configured | `is_error: true`, "Code search unavailable. No Muninn URL configured." |
| HA not configured | `is_error: true`, "Home Assistant is not configured." |
| HA domain not in allowlist | `is_error: true`, "Domain '{d}' is not in the allowed list. Allowed: {list}" |
| HTTP 4xx from downstream | `is_error: true`, "{operation} failed (HTTP {status}): {body}" |
| HTTP 5xx from downstream | `is_error: true`, "Internal error from {service}: {status}" |
| Request timeout | `is_error: true`, "Request timed out after {N} seconds" |
| Input exceeds size limit | `is_error: true`, "{field} exceeds maximum size of {N} bytes" |
| HA automation generation fails | `is_error: true`, "Automation generation failed: {e}" |
| Unknown resource URI | JSON-RPC `InvalidParams` error: "Unknown resource URI: {uri}" |

## Interface Boundaries

| Module | Owns | Exposes | Depends On |
|--------|------|---------|------------|
| `ygg-mcp-server::main` | Process lifecycle, CLI parsing (`clap`), config loading (`serde_yaml`), tracing init (stderr), `rmcp` stdio transport startup | Nothing (binary entrypoint) | `ygg-mcp::server::YggdrasilServer`, `ygg_domain::config::McpServerConfig` |
| `ygg-mcp::server` | MCP `ServerHandler` impl, `#[tool_router]` dispatch, resource handler dispatch, `YggdrasilServer` construction (including optional `HaClient` + `AutomationGenerator`) | `YggdrasilServer` struct, `YggdrasilServer::from_config()` | `ygg-mcp::tools`, `ygg-mcp::resources`, `ygg-ha`, `rmcp` |
| `ygg-mcp::tools` | Tool parameter structs (with `JsonSchema` derive), all 9 tool execution functions, HTTP call construction, response formatting, input validation, error wrapping | `search_code()`, `query_memory()`, `store_memory()`, `generate()`, `list_models()`, `ha_get_states()`, `ha_list_entities()`, `ha_call_service()`, `ha_generate_automation()`, `models_table()`, all `*Params` structs | `reqwest`, `ygg_domain::config::McpServerConfig`, `ygg_ha::HaClient`, `ygg_ha::AutomationGenerator` |
| `ygg-mcp::resources` | Resource URI constants, resource content fetching | `read_models_resource()`, `read_memory_stats_resource()`, `RESOURCE_MODELS`, `RESOURCE_MEMORY_STATS` | `reqwest`, `ygg_domain::config::McpServerConfig`, `ygg-mcp::tools::models_table()` |
| `ygg-mcp::lib` | Tool name constants (9 constants), public module re-exports | `TOOL_*` constants, `server`, `tools`, `resources` modules | Nothing |
| `ygg_domain::config` | `McpServerConfig` struct definition | `McpServerConfig` | `serde`, `HaConfig` |
| `ygg-ha::client` | `HaClient` struct, `EntityState`, `DomainServices`, `ServiceDef` types, HA REST API methods | `HaClient::from_config()`, `get_states()`, `get_entity()`, `list_entities()`, `get_services()`, `call_service()` | `reqwest`, `ygg_domain::config::HaConfig` |
| `ygg-ha::automation` | `AutomationGenerator` struct, prompt construction, YAML extraction | `AutomationGenerator::new()`, `generate_automation()` | `reqwest`, `ygg-ha::client::HaClient` |

**Ownership rules:**
- Only `ygg-mcp::tools` makes HTTP calls to Odin/Muninn. `server` dispatches to tools but does not construct HTTP requests.
- Only `ygg-mcp::tools` (HA functions) calls `HaClient` methods. The server holds `Option<HaClient>` and passes references.
- Only `ygg-mcp::resources` fetches resource content. It reuses `tools::models_table()` for the models resource.
- `ygg-mcp-server::main` owns the stdio transport lifecycle.
- No module in `ygg-mcp` or `ygg-mcp-server` depends on `ygg-store`, `ygg-embed`, or any database/Qdrant client.

## File Manifest

### Crate: `crates/ygg-mcp-server/` (binary)

| File | Purpose | Status |
|------|---------|--------|
| `Cargo.toml` | Binary crate manifest. Deps: ygg-mcp, ygg-domain, rmcp, tokio, tracing, tracing-subscriber, clap, serde_yaml, anyhow | IMPLEMENTED |
| `src/main.rs` | Entrypoint: CLI args, config load, tracing init (stderr), `YggdrasilServer::from_config()`, rmcp stdio transport, graceful shutdown | IMPLEMENTED |

### Crate: `crates/ygg-mcp/` (library)

| File | Purpose | Status |
|------|---------|--------|
| `Cargo.toml` | Library manifest. Deps: ygg-domain, ygg-ha, rmcp, reqwest, serde, serde_json, schemars, thiserror, tracing, tokio | IMPLEMENTED |
| `src/lib.rs` | Module exports, 9 tool name constants (`TOOL_SEARCH_CODE` through `TOOL_HA_GENERATE_AUTOMATION`) | IMPLEMENTED |
| `src/server.rs` | `YggdrasilServer` struct (client, config, ha_client, generator, tool_router). `#[tool_router]` impl with 9 `#[tool]`-annotated methods. `ServerHandler` impl with `get_info()`, `list_resources()`, `read_resource()`. `from_config()` constructor. | IMPLEMENTED |
| `src/tools.rs` | 9 parameter structs with `JsonSchema` derive. 9 tool execution functions. Internal HTTP response types. `tool_ok()`/`tool_error()` helpers. `models_table()` shared helper. Input validation constants. `format_states_summary()` for HA domain-grouped output. 3 unit tests for domain allowlist. | IMPLEMENTED |
| `src/resources.rs` | `RESOURCE_MODELS` and `RESOURCE_MEMORY_STATS` constants. `read_models_resource()` and `read_memory_stats_resource()` functions. Internal `StatsResponse` type. | IMPLEMENTED |

### Crate: `crates/ygg-ha/` (library, dependency)

| File | Purpose | Status |
|------|---------|--------|
| `Cargo.toml` | HA client library manifest | IMPLEMENTED |
| `src/lib.rs` | Module exports: `AutomationGenerator`, `HaClient`, `EntityState`, `DomainServices`, `ServiceDef`, `HaError` | IMPLEMENTED |
| `src/client.rs` | `HaClient` struct: `from_config()`, `get_states()`, `get_entity()`, `list_entities()`, `get_services()`, `call_service()`. 2 unit tests. | IMPLEMENTED |
| `src/automation.rs` | `AutomationGenerator` struct: `new()`, `generate_automation()`. System prompt template with entity/service context. `extract_yaml()` helper. `build_entity_summary()`, `build_service_summary()` helpers. 3 unit tests. | IMPLEMENTED |
| `src/error.rs` | `HaError` enum: Http, Api, Parse, NotConfigured, Timeout, Generation | IMPLEMENTED |

### Config: `ygg_domain::config` (updated)

| Item | Purpose | Status |
|------|---------|--------|
| `McpServerConfig` struct | odin_url, muninn_url (Option), timeout_secs, ha (Option<HaConfig>) | IMPLEMENTED |
| `HaConfig` struct | url, token, timeout_secs | IMPLEMENTED |

### Config Files

| File | Purpose | Status |
|------|---------|--------|
| `configs/mcp-server/config.yaml` | Deployed MCP server config with Odin, Muninn, HA URLs | IMPLEMENTED |

### Workspace

| Item | Purpose | Status |
|------|---------|--------|
| `Cargo.toml` workspace members | `ygg-mcp-server` added | IMPLEMENTED |
| `Cargo.toml` workspace deps | `rmcp = "1.1"` with server+transport-io features, `schemars = "1"` | IMPLEMENTED |

## Client Configuration Examples

### Claude Code (`~/.claude/settings.json`)

```json
{
  "mcpServers": {
    "yggdrasil": {
      "command": "/opt/yggdrasil/bin/ygg-mcp-server",
      "args": ["--config", "/etc/yggdrasil/mcp-server/config.yaml"]
    }
  }
}
```

### VS Code (`.vscode/settings.json`)

```json
{
  "mcp.servers": {
    "yggdrasil": {
      "command": "/opt/yggdrasil/bin/ygg-mcp-server",
      "args": ["--config", "/etc/yggdrasil/mcp-server/config.yaml"]
    }
  }
}
```

## Acceptance Criteria

- [x] `ygg-mcp-server` binary compiles cleanly (`cargo check` passes with zero warnings)
- [x] All unit tests pass (`cargo test --package ygg-mcp --package ygg-ha` -- 8 tests total)
- [x] `ygg-mcp-server` starts cleanly and completes the MCP `initialize` handshake over stdio
- [x] `tools/list` returns exactly 9 tools (5 core + 4 HA) with correct JSON schemas
- [x] `resources/list` returns exactly 2 resources (`yggdrasil://models`, `yggdrasil://memory/stats`)
- [x] `search_code` tool routes to Muninn POST /api/v1/search and formats results as markdown
- [x] `query_memory` tool routes to Odin POST /api/v1/query and formats engram results
- [x] `store_memory` tool routes to Odin POST /api/v1/store and returns UUID
- [x] `generate` tool routes to Odin POST /v1/chat/completions (non-streaming) and returns response text
- [x] `list_models` tool routes to Odin GET /v1/models and formats as markdown table
- [x] `ha_get_states` tool fetches from HA GET /api/states and formats as domain-grouped markdown
- [x] `ha_list_entities` tool filters entities by domain prefix and formats as markdown table
- [x] `ha_call_service` tool enforces domain allowlist (19 domains allowed, `lock` excluded)
- [x] `ha_call_service` rejects disallowed domains with descriptive error
- [x] `ha_generate_automation` tool fetches entities/services, constructs prompt, calls Odin, extracts YAML
- [x] All tools return `is_error: true` with descriptive messages when downstream services are unreachable
- [x] All tools return `is_error: true` when HA is not configured (for HA tools)
- [x] Tracing output appears on stderr, not stdout (stdout reserved for JSON-RPC)
- [x] `McpServerConfig` includes optional `ha: Option<HaConfig>` field
- [x] Config file exists at `configs/mcp-server/config.yaml` with Odin, Muninn, and HA URLs
- [x] Input validation enforced: 100KB max for search/memory fields, 1MB max for prompts
- [ ] End-to-end test: `ygg-mcp-server` registered in Claude Code MCP settings and functional (requires deployed services)
- [ ] Memory ceiling verified under 40MB RSS during tool execution (requires runtime measurement)
- [ ] HA token env var expansion tested with deployed config

## Dependencies

| Dependency | Type | Status |
|------------|------|--------|
| Sprint 005 (Odin) | Must be running | DONE -- Odin serves as HTTP gateway for 4 core tools + HA automation |
| Sprint 004 (Muninn) | Must be running | DONE -- `search_code` calls Muninn directly |
| Sprint 002 (Mimir) | Must be running (via Odin proxy) | DONE -- `query_memory` and `store_memory` |
| Sprint 003 (Huginn) | Must be running | DONE -- must have indexed files for search_code results |
| `rmcp` crate v1.1 | External dependency | RESOLVED -- pinned in workspace Cargo.toml, features: server, transport-io |
| `schemars` crate v1 | External dependency | RESOLVED -- pinned in workspace, matches rmcp's re-exported version |
| `ygg-ha` crate | Internal dependency | IMPLEMENTED -- provides HaClient and AutomationGenerator |
| Ollama on Munin | Infrastructure | RUNNING -- IPEX-LLM container with qwen3-coder:30b-a3b-q4_K_M |
| Ollama on Hugin | Infrastructure | RUNNING -- native Ollama with qwen3:30b-a3b |
| Home Assistant on chirp | Infrastructure | RUNNING -- http://REDACTED_CHIRP_IP:8123, requires HA_TOKEN env var |

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| `rmcp` API breaks between 1.1 and future versions | Version pinned to `1.1` in workspace Cargo.toml. The `#[tool]` macro API is stable. |
| `schemars` version mismatch between rmcp and ygg-mcp | Both use `schemars = "1"` from workspace. rmcp re-exports schemars; the `JsonSchema` trait must resolve to the same type. Workspace dep ensures this. |
| `ha_generate_automation` requests model `qwq-32b` but actual Hugin model is `qwen3:30b-a3b` | Odin's model routing may alias `qwq-32b` to the current reasoning model. If not, the AutomationGenerator's model field needs updating to `qwen3:30b-a3b`. **Action: `core-executor` should verify Odin model routing or update the hardcoded model name.** |
| HA token in config uses `${HA_TOKEN}` placeholder | serde_yaml does not perform env var expansion. The config loader or a pre-processing step must expand this. If the token is not expanded, all HA tool calls will fail with 401 from HA. **Action: `core-executor` should verify env var expansion works in the deploy pipeline, or switch to reading HA_TOKEN directly from env in code.** |
| `ha_call_service` domain allowlist may be too restrictive or too permissive | The 19-domain allowlist covers common device control domains. `lock` is intentionally excluded as a safety measure. Users needing lock control can be directed to use HA directly. The allowlist can be expanded in config if needed. |
| `generate` tool with long prompts blocks the stdio channel | MCP is request-response; the client waits. The 300s timeout in config accommodates slow inference. Future: MCP progress notifications if the spec supports them. |
| Memory stats resource endpoint (`/api/v1/stats`) does not exist on Odin | The resource handler returns "Memory statistics not available." gracefully. No impact on other functionality. |
| MCP clients have subtle protocol expectations not covered by rmcp | Primary target is Claude Code. rmcp 1.1 handles the MCP protocol correctly for all tested clients. |

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | Standalone stdio binary instead of embedding MCP in Odin's process | MCP stdio transport requires exclusive ownership of stdin/stdout. Odin uses an Axum TCP listener. A separate binary is the standard pattern used by all MCP server implementations. |
| 2026-03-09 | Use `rmcp` official SDK (v1.1) with `schemars` 1.x | `rmcp` is the official Rust MCP SDK (modelcontextprotocol/rust-sdk). The `#[tool]` and `#[tool_router]` macros eliminate boilerplate. v1.1 requires schemars 1.x (not 0.8). |
| 2026-03-09 | stdio transport only, no SSE/WebSocket | All major MCP clients support stdio. SSE adds deployment complexity with no benefit for local IDE use. |
| 2026-03-09 | `search_code` calls Muninn directly, not through Odin | Odin does not proxy Muninn's `/api/v1/search`. Direct call avoids extra hop. The config has a dedicated `muninn_url` field. |
| 2026-03-09 | `store_memory` accepts `tags` but drops them silently | Forward-compatibility: when Mimir adds tag support, the MCP tool already accepts the parameter. |
| 2026-03-09 | Include HA tools in Sprint 006 (merged from planned Sprint 007 scope) | The `ygg-ha` crate was ready. Including HA tools in the same sprint as core tools avoids a separate implementation pass. All 9 tools share the same server handler architecture. |
| 2026-03-09 | HA domain allowlist excludes `lock` | Safety measure: lock control via MCP tool could have unintended consequences. Users needing lock control should use HA directly. |
| 2026-03-09 | Config timeout set to 300s (5 minutes) | Accommodates slow `ha_generate_automation` calls where the reasoning model may take 60+ seconds. The reqwest client timeout applies per-request. |
| 2026-03-09 | `AutomationGenerator` hardcoded to model `qwq-32b` | Originally the reasoning model on Hugin. Sprint 013 replaced QwQ with qwen3:30b-a3b but the AutomationGenerator was not updated. This is a known discrepancy to be addressed. |
