# Sprint: 007 - Home Assistant Integration
## Status: PLANNING

## Objective

Wire the existing `ygg-ha` crate into Odin and the MCP server so that users can query Home Assistant entity states, call HA services (e.g., turn on lights, lock doors), and generate automation YAML through the AI reasoning pipeline. The HA instance runs on the chirp VM (REDACTED_CHIRP_IP:8123). This sprint extends `HaClient` with entity filtering and automation prompt generation, adds an `automation` module for YAML generation via Odin's reasoning model, wires four HA-related MCP tools into the Sprint 006 MCP server, and adds HA intent routing to Odin's semantic router so that natural-language HA requests are directed to the reasoning model (QwQ-32B on Hugin).

## Scope

### In Scope
- Extend `HaClient` in `crates/ygg-ha/src/client.rs`:
  - `list_entities(domain: Option<&str>)` -- filter `get_states()` by HA domain (e.g., `light`, `switch`, `automation`, `sensor`)
  - `get_services()` -- call `GET /api/services` to list available services per domain
- New module `crates/ygg-ha/src/automation.rs`:
  - `AutomationGenerator` struct holding a reference to Odin's URL and a `reqwest::Client`
  - `generate_automation(description: &str) -> Result<String, HaError>` -- sends a structured prompt to Odin's `/v1/chat/completions` with the reasoning model, requesting YAML automation output
  - System prompt template for automation generation (includes HA automation schema reference, safety constraints, output format requirements)
- Update `HaError` enum in `crates/ygg-ha/src/error.rs`:
  - Add `Timeout(String)` variant
  - Add `Generation(String)` variant for automation generation failures
- Update `crates/ygg-ha/src/lib.rs` to export the `automation` module
- Add `HaConfig` to `OdinConfig` as an optional field: `pub ha: Option<HaConfig>`
- Extend `HaConfig` in `ygg_domain::config` with additional fields for automation generation
- Four MCP tools registered in the Sprint 006 MCP server (tool name constants already defined in `ygg-mcp/src/lib.rs`):
  1. `ha_get_states` -- list all entity states (optionally summarized)
  2. `ha_list_entities` -- list entities filtered by domain
  3. `ha_call_service` -- call an HA service with domain, service name, and data payload
  4. `ha_generate_automation` -- generate automation YAML from natural language description
- MCP tool implementations in `crates/ygg-mcp/src/tools.rs` (extend the file from Sprint 006)
- Add `ha` section to MCP server config (`McpServerConfig`) with HA base URL and token
- Update `configs/mcp-server/config.yaml` with HA section
- Update `configs/odin/node.yaml` with HA section
- Add `"home_automation"` intent to Odin's semantic router:
  - Keywords: home, light, switch, sensor, thermostat, lock, door, garage, automation, scene, script, climate, fan, cover, vacuum, alarm, media_player, camera
  - Routes to reasoning model `qwq-32b` on Hugin backend
- Odin RAG pipeline: when intent is `"home_automation"`, skip code context fetch from Muninn (irrelevant) but still fetch engram context from Mimir (prior HA interactions are useful memory)
- Add HA-specific system prompt injection in Odin when intent is `"home_automation"`:
  - Include available HA domains and entity counts
  - Include instruction to the model about HA YAML syntax

### Out of Scope
- HA WebSocket API (push events, real-time state changes). REST API is sufficient for tool-based interaction
- HA dashboard or UI integration
- Persistent HA entity state caching in PostgreSQL (all queries are live against the HA API)
- HA authentication management (token is stored in config, not rotated or refreshed)
- HA add-on installation or management
- HA configuration.yaml modification
- Voice control or wake-word integration
- HA area/floor management
- HA device registry queries (entity-level is sufficient)
- HA long-lived access token creation (user provides token in config)
- Validating generated automation YAML against HA's schema (the model generates best-effort YAML; user is responsible for review)
- Deploying generated automations to HA (user copies the YAML manually)

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** I/O-bound with a GPU-bound component for automation generation.
  - Entity queries and service calls: pure I/O (HTTP REST calls to chirp)
  - Automation generation: triggers LLM inference on Hugin (QwQ-32B reasoning model)
- **Target Hardware:**
  - MCP server + Odin: Munin (REDACTED_MUNIN_IP) -- Intel Core Ultra 185H, 48GB DDR5
  - HA instance: chirp VM on Plume (REDACTED_CHIRP_IP) -- 2 cores, 4GB RAM, no GPU
  - Reasoning model: Hugin (REDACTED_HUGIN_IP) -- AMD Ryzen 7 255 (Zen 5, 8C/16T), 64GB DDR5, iGPU
- **Utilization Plan:**
  - HA REST API calls are lightweight HTTP GETs/POSTs. Chirp has 2 cores and 4GB RAM; it can handle dozens of concurrent API requests without stress. The HA REST API is not a bottleneck.
  - Automation generation routes through Odin to the QwQ-32B model on Hugin. Hugin's 64GB DDR5 is sufficient for the 32B parameter model. Generation is single-request (non-streaming from the MCP tool's perspective). The Odin backend semaphore (max_concurrent: 2) prevents overloading Hugin.
  - Entity state responses from HA can be large (hundreds of entities). The `ha_get_states` tool formats a summary rather than dumping raw JSON to keep MCP response sizes manageable (< 100KB).
  - `ha_list_entities` with domain filter reduces response size by 10-50x compared to full state dump.
- **Fallback Strategy:**
  - If chirp (HA) is unreachable: all HA tools return `is_error: true` with "Home Assistant is not reachable". Odin continues to function for all non-HA requests.
  - If Hugin (reasoning model) is unreachable: `ha_generate_automation` falls back to the coding model on Munin (qwen3-coder-30b-a3b). Quality may be lower but the tool remains functional. The fallback is logged as a warning.
  - If HA token is invalid/expired: HA API returns 401. Tools surface this as "Home Assistant authentication failed. Check the access token in config."

## Performance Targets

| Metric | Target | Measurement Method |
|--------|--------|--------------------|
| `ha_get_states` P95 | < 2s | `tracing` span at MCP tool level. HA API can be slow with many entities. |
| `ha_list_entities` P95 (single domain) | < 1s | `tracing` span |
| `ha_call_service` P95 | < 3s | `tracing` span. Some HA services (e.g., garage door) are physically slow. |
| `ha_generate_automation` P95 (excluding LLM inference) | < 500ms overhead | Difference between MCP tool end-to-end and Odin generation end-to-end |
| `ha_generate_automation` typical wall clock | 10-60s | Depends on QwQ-32B inference speed on Hugin. Not a P95 target -- LLM inference is inherently variable. |
| HA entity summary formatting | < 50ms for 500 entities | CPU time for in-memory string formatting |
| Memory overhead of HA integration | < 5MB additional RSS | Measured as difference in Odin/MCP-server RSS with and without HA configured |
| Odin HA intent routing | < 5ms | Same measurement as existing routing P95 (keyword match) |

## Data Schemas

### HA REST API Responses (from chirp)

**`GET /api/states`** response (array of entity states):
```json
[
  {
    "entity_id": "light.living_room",
    "state": "on",
    "attributes": {
      "friendly_name": "Living Room Light",
      "brightness": 255,
      "color_temp": 370
    },
    "last_changed": "2026-03-09T10:30:00+00:00"
  }
]
```

Already modeled by `ygg_ha::client::EntityState` (confirmed from source).

**`GET /api/services`** response (object keyed by domain):
```json
{
  "light": {
    "services": {
      "turn_on": {
        "name": "Turn on",
        "description": "Turn on a light",
        "fields": {
          "entity_id": { "description": "Entity ID", "example": "light.living_room" },
          "brightness": { "description": "Brightness (0-255)", "example": 128 }
        }
      },
      "turn_off": { "..." : "..." }
    }
  }
}
```

New Rust struct (in `ygg-ha/src/client.rs`):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainServices {
    pub domain: String,
    pub services: HashMap<String, ServiceDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDef {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub fields: HashMap<String, serde_json::Value>,
}
```

**`POST /api/services/<domain>/<service>`** request:
```json
{
  "entity_id": "light.living_room",
  "brightness": 128
}
```

Already handled by `HaClient::call_service()` which accepts `serde_json::Value` as data.

### MCP Tool: `ha_get_states`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "summary": {
      "type": "boolean",
      "description": "If true, return a compact summary instead of full state details (default: true)",
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

Tool response (summary mode, MCP `TextContent`):
```
## Home Assistant Entity States (247 entities)

### Lights (12)
| Entity | State | Brightness |
|--------|-------|------------|
| light.living_room (Living Room Light) | on | 255 |
| light.bedroom (Bedroom Light) | off | - |
...

### Switches (8)
| Entity | State |
|--------|-------|
| switch.garage_door (Garage Door) | off |
...

### Sensors (45)
| Entity | State | Unit |
|--------|-------|------|
| sensor.outdoor_temp (Outdoor Temperature) | 22.5 | C |
...

[... grouped by domain ...]
```

Tool response (full mode): raw JSON array (truncated to first 50 entities if > 50).

### MCP Tool: `ha_list_entities`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "domain": {
      "type": "string",
      "description": "HA domain to filter by (e.g., 'light', 'switch', 'sensor', 'automation', 'climate')"
    }
  },
  "required": []
}
```

Rust parameter struct:
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HaListEntitiesParams {
    pub domain: Option<String>,
}
```

Internal call: `HaClient::list_entities(domain.as_deref())`

Tool response (MCP `TextContent`):
```
## Entities: light (12 found)

| Entity ID | Friendly Name | State | Last Changed |
|-----------|---------------|-------|--------------|
| light.living_room | Living Room Light | on | 2026-03-09 10:30 |
| light.bedroom | Bedroom Light | off | 2026-03-09 08:00 |
...
```

### MCP Tool: `ha_call_service`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "domain": {
      "type": "string",
      "description": "HA service domain (e.g., 'light', 'switch', 'cover', 'climate')"
    },
    "service": {
      "type": "string",
      "description": "Service name (e.g., 'turn_on', 'turn_off', 'toggle', 'set_temperature')"
    },
    "data": {
      "type": "object",
      "description": "Service call data (e.g., {\"entity_id\": \"light.living_room\", \"brightness\": 128})"
    }
  },
  "required": ["domain", "service", "data"]
}
```

Rust parameter struct:
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HaCallServiceParams {
    pub domain: String,
    pub service: String,
    pub data: serde_json::Value,
}
```

Internal call: `HaClient::call_service(&domain, &service, data)`

Tool response (MCP `TextContent`):
```
Service called successfully: light.turn_on

Data sent:
{
  "entity_id": "light.living_room",
  "brightness": 128
}
```

On error:
```
Service call failed: light.turn_on
Error: 400: Entity light.nonexistent not found
```

### MCP Tool: `ha_generate_automation`

Input schema:
```json
{
  "type": "object",
  "properties": {
    "description": {
      "type": "string",
      "description": "Natural language description of the desired automation (e.g., 'Turn on the living room lights at sunset')"
    }
  },
  "required": ["description"]
}
```

Rust parameter struct:
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HaGenerateAutomationParams {
    pub description: String,
}
```

Internal flow:
1. Call `HaClient::get_states()` to get current entity list (provides context for valid entity IDs)
2. Call `HaClient::get_services()` to get available services (provides context for valid service calls)
3. Build automation generation prompt with entity/service context and user description
4. Call Odin `POST /v1/chat/completions` with model `qwq-32b`, non-streaming
5. Extract YAML from response (look for ```yaml fenced code block)
6. Return the YAML

System prompt template (in `automation.rs`):
```
You are a Home Assistant automation expert. Generate valid Home Assistant automation YAML based on the user's description.

## Available Entities
{entity_summary}

## Available Services
{service_summary}

## Rules
- Output ONLY valid Home Assistant automation YAML inside a ```yaml code fence
- Use only entity IDs and services that exist in the lists above
- Include appropriate triggers, conditions, and actions
- Add a meaningful alias and description
- Use time patterns, state triggers, sun triggers, or numeric state triggers as appropriate
- For time-based automations, use the 'time' platform
- For state-based automations, use the 'state' platform
- Always include 'mode: single' unless the user specifies otherwise

## Output Format
Return ONLY the YAML automation block. No explanation before or after.
```

Tool response (MCP `TextContent`):
```
## Generated Automation

```yaml
alias: "Turn on living room lights at sunset"
description: "Automatically turn on the living room lights when the sun sets"
trigger:
  - platform: sun
    event: sunset
action:
  - service: light.turn_on
    target:
      entity_id: light.living_room
    data:
      brightness: 200
mode: single
```

**Note:** Review this automation carefully before adding it to your Home Assistant configuration.
```

### Extended HaConfig

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HaConfig {
    pub url: String,
    pub token: String,
    #[serde(default = "default_ha_timeout")]
    pub timeout_secs: u64,
}

fn default_ha_timeout() -> u64 {
    10
}
```

### Extended McpServerConfig (from Sprint 006)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub odin_url: String,
    #[serde(default)]
    pub muninn_url: Option<String>,
    #[serde(default = "default_mcp_timeout")]
    pub timeout_secs: u64,
    /// Optional HA config. If present, HA tools are registered.
    #[serde(default)]
    pub ha: Option<HaConfig>,
}
```

### Extended OdinConfig

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdinConfig {
    pub node_name: String,
    pub listen_addr: String,
    pub backends: Vec<BackendConfig>,
    pub routing: RoutingConfig,
    pub mimir: MimirClientConfig,
    pub muninn: MuninnClientConfig,
    /// Optional Home Assistant integration. If absent, HA features are disabled.
    #[serde(default)]
    pub ha: Option<HaConfig>,
}
```

## API Contracts

### HaClient Methods (extended)

| Method | Signature | HA Endpoint | Returns |
|--------|-----------|-------------|---------|
| `get_states` | `async fn get_states(&self) -> Result<Vec<EntityState>, HaError>` | `GET /api/states` | All entity states |
| `get_entity` | `async fn get_entity(&self, id: &str) -> Result<EntityState, HaError>` | `GET /api/states/{id}` | Single entity state |
| `call_service` | `async fn call_service(&self, domain: &str, service: &str, data: Value) -> Result<(), HaError>` | `POST /api/services/{domain}/{service}` | Unit (success/failure) |
| `list_entities` | `async fn list_entities(&self, domain: Option<&str>) -> Result<Vec<EntityState>, HaError>` | `GET /api/states` + client-side filter | Filtered entity states |
| `get_services` | `async fn get_services(&self) -> Result<Vec<DomainServices>, HaError>` | `GET /api/services` | All available services |

### AutomationGenerator Methods

| Method | Signature | Calls | Returns |
|--------|-----------|-------|---------|
| `generate_automation` | `async fn generate_automation(&self, ha: &HaClient, description: &str) -> Result<String, HaError>` | `ha.get_states()`, `ha.get_services()`, Odin `/v1/chat/completions` | YAML string |

### MCP Tools (registered in Sprint 006 server)

| Tool Name | MCP Method | Calls | Notes |
|-----------|-----------|-------|-------|
| `ha_get_states` | `tools/call` | `HaClient::get_states()` | Formats as markdown summary or raw JSON |
| `ha_list_entities` | `tools/call` | `HaClient::list_entities(domain)` | Filters by HA domain |
| `ha_call_service` | `tools/call` | `HaClient::call_service(domain, service, data)` | Side-effecting: actually controls devices |
| `ha_generate_automation` | `tools/call` | `AutomationGenerator::generate_automation(description)` | Triggers LLM inference, returns YAML |

### Odin Semantic Router Extension

New routing rule added to `RoutingConfig.rules`:
```yaml
- intent: home_automation
  model: qwq-32b
  backend: hugin
```

New keywords for `"home_automation"` intent in `SemanticRouter`:
```
home, light, switch, sensor, thermostat, lock, door, garage, automation,
scene, script, climate, fan, cover, vacuum, alarm, media_player, camera,
temperature, humidity, motion, occupancy, energy, power, battery,
turn on, turn off, toggle, brightness, color, heating, cooling,
home assistant, smart home, iot
```

### Odin HA-Aware RAG Behavior

When `SemanticRouter::classify()` returns intent `"home_automation"`:
- **Skip** Muninn code context fetch (code search is irrelevant for HA queries)
- **Keep** Mimir engram context fetch (prior HA interactions provide useful memory)
- **Inject** HA context into system prompt: available domains and entity counts fetched from `HaClient::get_states()` (cached for 60s to avoid hammering the HA API on every chat message)

HA system prompt injection (appended to the standard system prompt):
```
## Home Assistant Context
You have access to a Home Assistant instance with the following entity domains:
- light: 12 entities
- switch: 8 entities
- sensor: 45 entities
- automation: 15 entities
- climate: 3 entities
[...]

You can reference specific entities by their entity_id (e.g., light.living_room).
When the user asks about home automation, provide specific entity IDs and service calls.
```

### Config Files

**`configs/odin/node.yaml`** (updated):
```yaml
node_name: odin
listen_addr: "0.0.0.0:8080"
backends:
  - name: munin
    url: "http://localhost:11434"
    models: ["qwen3-coder-30b-a3b", "qwen3-embedding"]
    max_concurrent: 2
  - name: hugin
    url: "http://REDACTED_HUGIN_IP:11434"
    models: ["qwq-32b", "qwen3-embedding"]
    max_concurrent: 2
routing:
  default_model: "qwen3-coder-30b-a3b"
  rules:
    - intent: reasoning
      model: qwq-32b
      backend: hugin
    - intent: coding
      model: qwen3-coder-30b-a3b
      backend: munin
    - intent: home_automation
      model: qwq-32b
      backend: hugin
mimir:
  url: "http://localhost:9090"
  query_limit: 5
  store_on_completion: true
muninn:
  url: "http://REDACTED_HUGIN_IP:9091"
  max_context_chunks: 10
ha:
  url: "http://REDACTED_CHIRP_IP:8123"
  token: "${HA_TOKEN}"
  timeout_secs: 10
```

Note: `${HA_TOKEN}` is the long-lived access token. It must be set as an environment variable `HA_TOKEN` or replaced in the config file. The token is sensitive and must not be committed to version control.

**`configs/mcp-server/config.yaml`** (updated from Sprint 006):
```yaml
odin_url: "http://localhost:8080"
muninn_url: "http://REDACTED_HUGIN_IP:9091"
timeout_secs: 30
ha:
  url: "http://REDACTED_CHIRP_IP:8123"
  token: "${HA_TOKEN}"
  timeout_secs: 10
```

## Interface Boundaries

| Module | Owns | Exposes | Depends On |
|--------|------|---------|------------|
| `ygg-ha::client` | HA REST API communication, entity state parsing, service call execution, entity filtering | `HaClient`, `EntityState`, `DomainServices`, `ServiceDef` | `reqwest`, `ygg_domain::config::HaConfig` |
| `ygg-ha::automation` | Automation prompt template, LLM call for YAML generation, YAML extraction from model response | `AutomationGenerator`, `AutomationGenerator::generate_automation()` | `reqwest`, `ygg-ha::client::HaClient` |
| `ygg-ha::error` | Error type definitions | `HaError` enum | `thiserror` |
| `ygg-mcp::tools` (extended) | HA MCP tool parameter structs, HA tool execution, response formatting | `ha_get_states()`, `ha_list_entities()`, `ha_call_service()`, `ha_generate_automation()` | `ygg-ha::client::HaClient`, `ygg-ha::automation::AutomationGenerator` |
| `ygg-mcp::server` (extended) | Conditional registration of HA tools based on config | `YggdrasilServer` (updated) | `ygg-mcp::tools`, `ygg_domain::config::McpServerConfig` |
| `odin::router` (extended) | `"home_automation"` intent keywords, routing to reasoning backend | `SemanticRouter::classify()` (unchanged signature, new intent value) | `ygg_domain::config::RoutingConfig` |
| `odin::rag` (extended) | HA-aware context fetch logic (skip Muninn for HA intent), HA system prompt injection | `fetch_context()` (updated to accept intent), `build_system_prompt()` (updated) | `odin::state::AppState`, `ygg-ha::client::HaClient` |
| `odin::state` (extended) | Optional `HaClient` instance in `AppState` | `AppState` (updated field) | `ygg-ha::client::HaClient`, `ygg_domain::config::HaConfig` |

**Ownership rules:**
- Only `ygg-ha::client` communicates with the HA REST API. No other module makes HTTP calls to chirp.
- Only `ygg-ha::automation` constructs the automation generation prompt and calls Odin for LLM inference. The MCP tool layer (`ygg-mcp::tools`) calls `AutomationGenerator` but does not build prompts itself.
- `ygg-mcp::tools` is the translation boundary between MCP JSON-RPC and `ygg-ha` library types. It formats `EntityState` into markdown tables and extracts YAML from automation responses.
- Odin's `rag` module owns the decision to skip Muninn for HA intents. The `router` module only classifies intent; it does not dictate RAG behavior.
- The `ha` field in `OdinConfig` and `McpServerConfig` is `Option<HaConfig>`. When `None`, all HA features are disabled gracefully. No HA tools appear in MCP `tools/list`, no HA intent is registered in the router, no HA context is injected.

## File-Level Implementation Plan

### `crates/ygg-ha/src/client.rs` (MODIFY)

Add two new methods to `HaClient`:

**`list_entities`:**
- Call `self.get_states()`
- If `domain` is `Some(d)`, filter results where `entity_id` starts with `"{d}."`
- Return filtered `Vec<EntityState>`

**`get_services`:**
- `GET {base_url}/api/services`
- Bearer auth with token
- Parse response as `Vec<DomainServices>` (HA returns an array of objects with `domain` and `services` keys)
- Return `Vec<DomainServices>`

Add new structs: `DomainServices`, `ServiceDef` (see Data Schemas section).

### `crates/ygg-ha/src/automation.rs` (NEW)

- `AutomationGenerator` struct: `odin_url: String`, `http: reqwest::Client`, `model: String`
- Constructor: `AutomationGenerator::new(odin_url: &str, model: &str) -> Self`
- `generate_automation(&self, ha: &HaClient, description: &str) -> Result<String, HaError>`:
  1. `ha.get_states().await?` -- get entity context
  2. `ha.get_services().await?` -- get service context
  3. Build entity summary: group by domain, list `entity_id` and `friendly_name`
  4. Build service summary: list domains and their service names
  5. Construct system prompt from template (see Data Schemas)
  6. Construct user message: the raw `description`
  7. `POST {odin_url}/v1/chat/completions` with `model`, `stream: false`, system + user messages
  8. Parse `ChatCompletionResponse`, extract `choices[0].message.content`
  9. Extract YAML from ```yaml fenced code block (regex or simple string search)
  10. If no fenced block found, return the entire content (model may have returned raw YAML)
  11. Return the YAML string

### `crates/ygg-ha/src/error.rs` (MODIFY)

Add variants:
```rust
#[error("HA request timed out: {0}")]
Timeout(String),
#[error("HA automation generation failed: {0}")]
Generation(String),
```

### `crates/ygg-ha/src/lib.rs` (MODIFY)

Add: `pub mod automation;`
Add: `pub use automation::AutomationGenerator;`

### `crates/ygg-ha/Cargo.toml` (MODIFY)

Add `tokio` dependency (needed for timeout):
```toml
tokio = { workspace = true }
```

### `crates/ygg-domain/src/config.rs` (MODIFY)

1. Extend `HaConfig`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HaConfig {
    pub url: String,
    pub token: String,
    #[serde(default = "default_ha_timeout")]
    pub timeout_secs: u64,
}

fn default_ha_timeout() -> u64 {
    10
}
```

2. Add `ha: Option<HaConfig>` to `OdinConfig`:
```rust
pub struct OdinConfig {
    // ... existing fields ...
    #[serde(default)]
    pub ha: Option<HaConfig>,
}
```

3. Add `ha: Option<HaConfig>` to `McpServerConfig` (from Sprint 006):
```rust
pub struct McpServerConfig {
    // ... existing fields ...
    #[serde(default)]
    pub ha: Option<HaConfig>,
}
```

### `crates/ygg-mcp/src/tools.rs` (MODIFY -- extend from Sprint 006)

Add four HA tool functions:

**`ha_get_states`:** Call `HaClient::get_states()`, format as markdown summary or raw JSON based on `summary` param.

**`ha_list_entities`:** Call `HaClient::list_entities(domain)`, format as markdown table.

**`ha_call_service`:** Call `HaClient::call_service(domain, service, data)`, return success/failure message.

**`ha_generate_automation`:** Call `AutomationGenerator::generate_automation(description)`, return the YAML in a markdown code fence.

All HA tools check that HA is configured before executing. If `ha` config is `None`, return `is_error: true` with "Home Assistant is not configured."

### `crates/ygg-mcp/src/server.rs` (MODIFY -- extend from Sprint 006)

- If `config.ha` is `Some`, construct `HaClient` and `AutomationGenerator` and store in `YggdrasilServer`
- Register the four HA tools in the `#[tool_router]` block (conditionally: only if HA is configured)
- `tools/list` returns 5 base tools + 4 HA tools = 9 tools when HA is configured, 5 tools when not

### `crates/odin/src/router.rs` (MODIFY -- from Sprint 005)

- Add `"home_automation"` keyword set to `SemanticRouter`
- Keywords listed in API Contracts section above

### `crates/odin/src/rag.rs` (MODIFY -- from Sprint 005)

- `fetch_context()` accepts `intent: &str` parameter
- When `intent == "home_automation"`: skip Muninn query, only query Mimir
- When `intent == "home_automation"` and `AppState` has `ha_client`: fetch HA domain summary (cached 60s via `tokio::sync::RwLock<Option<(Instant, String)>>`) and inject into system prompt

### `crates/odin/src/state.rs` (MODIFY -- from Sprint 005)

- Add `pub ha_client: Option<HaClient>` to `AppState`
- Add `pub ha_context_cache: Arc<tokio::sync::RwLock<Option<(tokio::time::Instant, String)>>>` for 60s HA context cache
- Construct `HaClient::from_config(ha_config)` in `main.rs` if `config.ha` is `Some`

### `crates/ygg-mcp/Cargo.toml` (MODIFY)

Add `ygg-ha` dependency:
```toml
ygg-ha = { path = "../ygg-ha" }
```

## Acceptance Criteria

- [ ] `HaClient::list_entities(Some("light"))` returns only entities with IDs starting with `light.`
- [ ] `HaClient::list_entities(None)` returns all entities (same as `get_states`)
- [ ] `HaClient::get_services()` returns at least one domain with at least one service definition
- [ ] `ha_get_states` MCP tool returns a formatted markdown summary grouped by domain
- [ ] `ha_list_entities` MCP tool with `domain: "light"` returns only light entities
- [ ] `ha_call_service` MCP tool with `domain: "light", service: "toggle", data: {"entity_id": "light.living_room"}` toggles the light (manual verification on HA dashboard)
- [ ] `ha_call_service` MCP tool with invalid entity returns `is_error: true` with the HA API error message
- [ ] `ha_generate_automation` MCP tool with description "Turn on living room lights at sunset" returns valid YAML containing `platform: sun` and `event: sunset`
- [ ] When HA config is omitted, `tools/list` returns exactly 5 tools (no HA tools)
- [ ] When HA config is present, `tools/list` returns exactly 9 tools
- [ ] All HA tools return `is_error: true` with descriptive message when chirp (HA) is unreachable
- [ ] Odin routes messages containing "turn on the light" to the `qwq-32b` model on Hugin
- [ ] Odin skips Muninn code context fetch when intent is `"home_automation"`
- [ ] Odin still fetches Mimir engram context when intent is `"home_automation"`
- [ ] Odin injects HA domain summary into system prompt when intent is `"home_automation"` and HA is configured
- [ ] HA context cache in Odin refreshes after 60s staleness
- [ ] `ha_generate_automation` falls back to coding model when Hugin is unreachable (logged as warning)
- [ ] HA token is not logged or exposed in any tracing output (token value is redacted)
- [ ] No panics on HA API timeouts, 401 responses, or malformed entity state JSON

## Dependencies

| Dependency | Type | Status |
|------------|------|--------|
| Sprint 005 (Odin) | Must be implemented | Semantic router, RAG pipeline, AppState to extend |
| Sprint 006 (MCP server) | Must be implemented | MCP server to add HA tools to |
| Home Assistant on chirp (REDACTED_CHIRP_IP) | Must be running | Target HA instance |
| HA long-lived access token | Must be created | User creates via HA UI: Profile -> Security -> Long-lived access tokens |
| QwQ-32B on Hugin | Must be running | Reasoning model for automation generation |
| `ygg-ha` crate | Already scaffolded | `HaClient` with `get_states`, `get_entity`, `call_service` already implemented |

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| HA REST API is slow for `get_states` with many entities (500+ entities, 2-5s) | `ha_get_states` uses summary mode by default. `ha_generate_automation` caches entity/service lists in the `AutomationGenerator` with a 5-minute TTL to avoid repeated slow calls. |
| Generated automation YAML is invalid or uses nonexistent entities | The system prompt includes the actual entity list from HA. The model is instructed to use only listed entities. A disclaimer is included in every `ha_generate_automation` response telling the user to review the YAML. |
| HA long-lived token expires or is revoked | All HA tools surface 401 errors as "Authentication failed. Check the access token." The token is not auto-refreshed. User must create a new token in HA and update the config. |
| `ha_call_service` used for destructive actions (e.g., unlock door, open garage) | This is a feature, not a bug -- the tool provides full service call capability. The MCP client (Claude Code, VS Code) already requires user confirmation for tool calls. No additional safety layer is added in Yggdrasil. |
| HA entity IDs change (user renames entities) | Generated automations reference entity IDs at generation time. If IDs change, previously generated automations break. This is a standard HA limitation, not a Yggdrasil issue. |
| QwQ-32B produces poor automation YAML | The system prompt is carefully engineered with HA schema reference and examples. If quality is insufficient, the prompt template in `automation.rs` can be iterated without changing any interfaces. |
| Large entity counts (1000+) exceed token budget when injecting HA context | HA context injection in Odin uses a summary (domain + count) not full entity listing. The `ha_generate_automation` tool's prompt includes full entity list but truncates to 200 entities per domain if count exceeds that. |
| `${HA_TOKEN}` env var expansion not supported by serde_yaml | Implement a simple env var expansion in config loading (regex replace `${VAR}` with `std::env::var("VAR")`), or document that the user must replace the placeholder manually. The env var approach is preferred for security. |

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | HA tools in MCP server, not as Odin HTTP endpoints | HA tools are for IDE users (Claude Code, VS Code). There is no external client that needs HA via HTTP. MCP is the correct interface. Odin's HA integration is limited to routing and context injection for chat completions. |
| 2026-03-09 | `ha` config is `Option` in both `OdinConfig` and `McpServerConfig` | HA is not a required component. Users who do not have Home Assistant should not need to configure it. Graceful degradation when absent. |
| 2026-03-09 | Automation generation uses Odin's `/v1/chat/completions`, not a direct Ollama call | Reuses Odin's model routing, backend semaphores, and engram storage. The `AutomationGenerator` does not need to know about Ollama backends or manage concurrency -- Odin handles that. |
| 2026-03-09 | Entity/service context included in automation prompt | Without context, the model generates plausible but incorrect entity IDs. Including the actual entity list from HA dramatically improves accuracy. The cost is a larger prompt (~2-5K tokens for entity/service listing) which is well within QwQ-32B's 32K context window. |
| 2026-03-09 | HA context cache (60s) in Odin for chat completions | Calling `get_states()` on every chat message would add 1-3s latency. A 60s cache provides fresh-enough data for conversational use. The cache is a simple `RwLock<Option<(Instant, String)>>` -- no external cache service needed. |
| 2026-03-09 | `ha_call_service` has no confirmation step | MCP clients handle tool call confirmation. Adding a server-side confirmation flow would violate the MCP tool execution model. The tool description clearly states it performs real actions on the HA instance. |
| 2026-03-09 | Route HA intent to QwQ-32B reasoning model | HA queries often require multi-step reasoning (e.g., "if the temperature drops below 20 and it's after sunset, turn on the heater"). The reasoning model handles these better than the coding model. The coding model is the fallback if Hugin is unreachable. |
| 2026-03-09 | Skip Muninn code search for HA intents | Code context is irrelevant for home automation queries. Skipping it saves 200-500ms per request and avoids polluting the system prompt with unrelated code snippets. Mimir engram context is preserved because prior HA conversations are valuable memory. |
