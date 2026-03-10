# Sprint: 005 - Odin Orchestrator
## Status: DONE

## Objective

Build Odin, the central Axum HTTP orchestrator that serves as the single entry point for all AI interactions in Yggdrasil. Odin exposes an OpenAI-compatible chat completions API, routes requests to the appropriate Ollama backend via a rule-based semantic router, injects RAG context from Muninn (code search) and Mimir (engram memory) before generation, streams responses back to clients via SSE, and stores completed interactions as engrams in Mimir. It also proxies Mimir's engram endpoints for Fergus client compatibility, aggregates model availability across all backends, and provides HA-aware routing with cached domain context injection. Odin runs on Munin (REDACTED_MUNIN_IP) alongside Mimir, connecting to remote Ollama instances on both Munin and Hugin.

## Scope

### In Scope
- Axum HTTP server on configurable address (default `0.0.0.0:8080`)
- `POST /v1/chat/completions` -- OpenAI-compatible chat completions with model routing and RAG injection
- `GET /v1/models` -- aggregate model listing from all configured backends (Ollama and OpenAI-compatible)
- `POST /api/v1/query` -- transparent proxy to Mimir (for Fergus client compatibility)
- `POST /api/v1/store` -- transparent proxy to Mimir (for Fergus client compatibility)
- `GET /health` -- Odin health status with backend and service availability aggregation
- `GET /metrics` -- Prometheus exposition format scrape endpoint
- Semantic router: rule-based keyword classification dispatching to coding, reasoning, or home_automation backends
- RAG pipeline: parallel query to Muninn (code context) and Mimir (engram memory), context injection into system prompt
- HA-aware RAG: home_automation intents skip Muninn code context, inject cached HA domain summary
- Engram integration: store completed interactions as cause-effect pairs in Mimir (configurable, on by default)
- SSE streaming: convert Ollama's newline-delimited JSON stream to OpenAI-compatible SSE `data: {...}\n\n` format
- OpenAI-compatible backend pass-through: forward requests directly to vLLM/OpenAI-compat backends without Ollama conversion
- Non-streaming mode: accumulate Ollama response and return complete ChatCompletion response with usage stats
- Backend concurrency limiting: per-backend semaphore with configurable `max_concurrent` (default 2), `try_acquire()` returns 503
- Dual backend type support: `BackendType::Ollama` (POST /api/chat) and `BackendType::Openai` (POST /v1/chat/completions)
- YAML config loading via `OdinConfig` from `ygg_domain::config`
- Home Assistant client integration: optional `HaConfig`, `HaClient` from `ygg-ha`
- HA domain summary cache: 60-second TTL, `RwLock<Option<(Instant, String)>>` with double-check pattern
- `tower-http` CORS layer (permissive for private LAN)
- Request body size limit: 2MB via `DefaultBodyLimit`
- Global concurrency limit: 64 in-flight requests via `tower::limit::ConcurrencyLimitLayer`
- Structured tracing via `tracing` and `tracing-subscriber` with `EnvFilter`
- Prometheus metrics: `ygg_http_requests_total`, `ygg_http_request_duration_seconds`, `ygg_routing_intent_total`, `ygg_llm_generation_duration_seconds`, `ygg_backend_active_requests`
- systemd sd-notify (Type=notify ready signal) and optional watchdog heartbeat
- Graceful error handling: JSON error responses with OpenAI-compatible error format
- Graceful shutdown on SIGTERM/SIGINT with in-flight request draining

### Out of Scope
- Authentication / authorization (private LAN, no auth)
- TLS termination (handled by reverse proxy if needed)
- Embedding-based semantic routing (v2 upgrade path)
- Function calling / tool use in the OpenAI API
- Image/multimodal inputs
- Conversation history management (client maintains context window)
- Token counting (Odin does not enforce context window limits)
- Rate limiting beyond per-backend concurrency semaphores and global 64-request limit
- WebSocket interface
- MCP integration (handled by ygg-mcp-server as standalone binary, Sprint 006)
- Request/response caching
- Model pull or management endpoints
- Retry logic on Ollama failures (client retries; Odin fails fast)

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** I/O-bound. Odin is a thin orchestration layer that proxies HTTP requests to Ollama backends, Mimir, and Muninn. CPU usage is negligible (keyword matching, JSON serialization, SSE framing). Memory usage is proportional to the number of concurrent streaming connections.
- **Target Hardware:** Munin (REDACTED_MUNIN_IP) -- Intel Core Ultra 185H (6P+8E+2LP cores, 16 threads), 48GB DDR5, ARC iGPU (used by IPEX-LLM Ollama container, not Odin), 2x 5Gb Ethernet
- **Backend Services:**
  - Munin Ollama (local): `http://localhost:11434` -- IPEX-LLM container, hosts qwen3-coder:30b-a3b-q4_K_M (coding, 15.9 tok/s) and qwen3-embedding (4096-dim)
  - Hugin Ollama (remote): `http://REDACTED_HUGIN_IP:11434` -- native Ollama, hosts qwen3:30b-a3b (reasoning, 26.48 tok/s) and qwen3-embedding (4096-dim)
  - Mimir (local): `http://localhost:9090` -- engram memory service on same host
  - Muninn (remote): `http://REDACTED_HUGIN_IP:9091` -- code retrieval service on Hugin
  - Home Assistant (optional): `http://REDACTED_CHIRP_IP:8123` -- chirp on Plume
- **Utilization Plan:**
  - Tokio runtime with default multi-threaded scheduler. The 185H's 16 hardware threads provide ample headroom for this I/O-bound workload.
  - Per-backend `tokio::sync::Semaphore` with `max_concurrent` permits (default 2 per backend). Ollama instances serialize inference on a single GPU, so higher concurrency wastes memory without improving throughput.
  - Global `tower::limit::ConcurrencyLimitLayer::new(64)` prevents resource exhaustion from connection floods.
  - Streaming: each active SSE connection holds one `reqwest::Response` body stream and one Axum `Sse` sender. Memory per connection ~4KB buffer + ~100KB worst-case RAG context. At 10 concurrent streams: ~1MB total.
  - RAG context fetch: Muninn and Mimir queried in parallel via `tokio::join!`. Combined latency is `max(muninn_latency, mimir_latency)`.
  - Single shared `reqwest::Client` with connection pooling (hyper internals).
  - Co-location with Mimir: both are I/O-bound with < 100MB RSS each. The 48GB DDR5 is more than sufficient.
  - HA context cache prevents redundant REST calls -- single fetch per 60 seconds regardless of request volume.
- **Fallback Strategy:** Odin uses no hardware-specific optimizations. All operations are standard async HTTP proxying. On a lesser machine (2 cores, 4GB RAM), Odin runs identically with reduced concurrent capacity. Correctness is unaffected. If HA is not configured (`ha: null` in YAML), all HA features are disabled gracefully (no error, no performance cost).

## Performance Targets

| Metric | Target | Measurement Method |
|--------|--------|--------------------|
| Routing decision P95 | < 5ms | `tracing` span on `SemanticRouter::classify()` |
| RAG context injection P95 (excluding model inference) | < 200ms | `tracing` span on `rag::fetch_context()` encompassing parallel Muninn + Mimir queries |
| Time to first token (TTFT) P95 (cached context) | < 500ms | `tracing` span from handler entry to first SSE `data:` chunk written to client (assumes Ollama model is loaded in memory) |
| Proxy overhead P95 (non-streaming) | < 10ms | Difference between Odin end-to-end latency and Ollama end-to-end latency for same request without RAG |
| Mimir proxy P95 (/api/v1/query, /api/v1/store) | < 15ms added over Mimir direct | `tracing` span on proxy handler minus Mimir response time |
| `/v1/models` P95 | < 100ms | `tracing` span on models handler (aggregates from all backends) |
| `/health` P95 | < 50ms | `tracing` span on health handler (includes 2s-timeout backend probes) |
| Memory ceiling (steady state, 0 active streams) | < 80MB RSS | `/proc/self/status` VmRSS |
| Memory ceiling (10 concurrent streams) | < 150MB RSS | Same |
| Startup time | < 2s | Wall clock from process start to "odin ready" log line |
| Concurrent streams supported | >= 10 without degradation | Verify P95 TTFT stays under 1s with 10 concurrent chat completion requests |

## Data Schemas

### Request: `POST /v1/chat/completions`

OpenAI-compatible ChatCompletionRequest:
```json
{
  "model": "string | null",
  "messages": [
    {
      "role": "system" | "user" | "assistant",
      "content": "string"
    }
  ],
  "stream": true,
  "temperature": 0.7,
  "max_tokens": null,
  "top_p": null,
  "stop": null
}
```

- `model`: Optional. When absent, the semantic router classifies the last user message and selects the model. When present, routes to the backend hosting that model or returns 400 if unknown.
- `messages`: Required, must be non-empty. Standard OpenAI conversation format.
- `stream`: Optional, defaults to `true`. When `true`, response is SSE. When `false`, response is a single JSON object.
- `temperature`, `max_tokens`, `top_p`, `stop`: Optional generation parameters forwarded to the backend.

Rust struct (defined in `odin::openai`):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    #[serde(default)]
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default = "default_stream")]
    pub stream: bool, // default: true
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { System, User, Assistant }
```

### Response: `POST /v1/chat/completions` (non-streaming)

```json
{
  "id": "chatcmpl-<uuid>",
  "object": "chat.completion",
  "created": 1709000000,
  "model": "qwen3-coder:30b-a3b-q4_K_M",
  "choices": [
    {
      "index": 0,
      "message": { "role": "assistant", "content": "response text" },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 128,
    "completion_tokens": 256,
    "total_tokens": 384
  }
}
```

- `usage` is populated from Ollama's `prompt_eval_count` and `eval_count` when available. `null` when the backend does not report token counts.

### Response: `POST /v1/chat/completions` (streaming SSE)

Each SSE event is `data: <json>\n\n`:
```json
{
  "id": "chatcmpl-<uuid>",
  "object": "chat.completion.chunk",
  "created": 1709000000,
  "model": "qwen3-coder:30b-a3b-q4_K_M",
  "choices": [
    {
      "index": 0,
      "delta": { "role": "assistant", "content": "token" },
      "finish_reason": null
    }
  ]
}
```

- First chunk: `delta.role = "assistant"`, subsequent chunks omit `role`.
- Final chunk: `finish_reason = "stop"`, followed by sentinel `data: [DONE]\n\n`.
- Stream line buffer capped at 10MB to prevent OOM from pathological input.

### Ollama Chat API (Upstream -- BackendType::Ollama)

Odin sends `POST {backend_url}/api/chat`:
```json
{
  "model": "qwen3-coder:30b-a3b-q4_K_M",
  "messages": [
    {"role": "system", "content": "...RAG context..."},
    {"role": "user", "content": "..."}
  ],
  "stream": true,
  "options": {
    "temperature": 0.7,
    "num_predict": 4096,
    "top_p": 0.9,
    "stop": null
  }
}
```

Ollama streaming response (newline-delimited JSON, NOT SSE):
```json
{"model":"qwen3-coder:30b-a3b-q4_K_M","message":{"role":"assistant","content":"token"},"done":false}
{"model":"qwen3-coder:30b-a3b-q4_K_M","message":{"role":"assistant","content":""},"done":true,"total_duration":123456789,"eval_count":256,"prompt_eval_count":128}
```

### OpenAI-Compatible Backend (Upstream -- BackendType::Openai)

Odin forwards `POST {backend_url}/v1/chat/completions` with the OpenAI request body unchanged (model field set from routing decision). The response is already in OpenAI SSE format and is passed through. For non-streaming, the response is deserialized directly into `ChatCompletionResponse`.

### Request: `POST /api/v1/query` (Mimir proxy)

Transparent passthrough. Body forwarded unchanged:
```json
{
  "text": "string (required)",
  "limit": 5
}
```

### Response: `POST /api/v1/query` (Mimir proxy)

Transparent passthrough. Returns flat `Vec<Engram>` (Fergus client contract):
```json
[
  {
    "id": "uuid",
    "cause": "string",
    "effect": "string",
    "similarity": 0.95
  }
]
```

### Request: `POST /api/v1/store` (Mimir proxy)

Transparent passthrough:
```json
{
  "cause": "string",
  "effect": "string"
}
```

### Response: `POST /api/v1/store` (Mimir proxy)

```json
{
  "id": "uuid"
}
```

### Response: `GET /v1/models`

OpenAI-compatible model listing, aggregated from all backends:
```json
{
  "object": "list",
  "data": [
    {
      "id": "qwen3-coder:30b-a3b-q4_K_M",
      "object": "model",
      "created": 1709000000,
      "owned_by": "ollama:munin"
    },
    {
      "id": "qwen3:30b-a3b",
      "object": "model",
      "created": 1709000000,
      "owned_by": "ollama:hugin"
    }
  ]
}
```

- Ollama backends: models fetched via `GET /api/tags`, tagged `owned_by: "ollama:{backend_name}"`.
- OpenAI backends: models fetched via `GET /v1/models`, tagged `owned_by: "openai:{backend_name}"`.
- Unreachable backends are skipped with a warning. Empty list returned if all backends are down.

### Response: `GET /health`

```json
{
  "status": "ok" | "degraded" | "error",
  "backends": {
    "munin": {
      "status": "ok",
      "models": ["qwen3-coder:30b-a3b-q4_K_M", "qwen3-embedding"]
    },
    "hugin": {
      "status": "ok",
      "models": ["qwen3:30b-a3b", "qwen3-embedding"]
    }
  },
  "services": {
    "mimir": "ok",
    "muninn": "ok"
  }
}
```

- Each backend probed via model listing API (Ollama: `/api/tags`, OpenAI: `/v1/models`) with 2s timeout.
- Each service probed via `GET {base_url}/health` with 2s timeout.
- Top-level status: `"ok"` if all pass, `"degraded"` if some pass, `"error"` if all fail.
- `error` field included in `BackendHealth` when probe fails (e.g., `"error": "timeout"`).
- Health endpoint always returns HTTP 200 -- degraded state conveyed in the body.

### Response: `GET /metrics`

Prometheus text exposition format (`text/plain; version=0.0.4`). Metrics:

| Metric Name | Type | Labels | Description |
|-------------|------|--------|-------------|
| `ygg_http_requests_total` | counter | service, endpoint, status | Total HTTP requests |
| `ygg_http_request_duration_seconds` | histogram | service, endpoint | Request duration |
| `ygg_routing_intent_total` | counter | intent | Routing decisions by intent |
| `ygg_llm_generation_duration_seconds` | histogram | model | LLM generation wall-clock time |
| `ygg_backend_active_requests` | gauge | backend | Currently in-flight requests per backend |

### Error Response Formats

OpenAI-compatible error format for `/v1/*` endpoints:
```json
{
  "error": {
    "message": "human-readable error description",
    "type": "invalid_request_error" | "server_error" | "service_unavailable",
    "code": null
  }
}
```

Standard error format for `/api/v1/*` proxy endpoints:
```json
{
  "error": "human-readable error description"
}
```

| OdinError variant | HTTP Status | type field | Trigger |
|-------------------|-------------|------------|---------|
| `BadRequest` | 400 | `invalid_request_error` | Empty messages, unknown model |
| `BackendUnavailable` | 503 | `service_unavailable` | Semaphore `try_acquire()` fails |
| `Upstream` | 502 | `server_error` | Ollama/OpenAI backend unreachable or error |
| `Proxy` | 502 | (plain format) | Mimir unreachable during transparent proxy |
| `Internal` | 500 | `server_error` | Backend not found in state (config error) |

## API Contracts

### HTTP Endpoints

| Method | Path | Request Body | Response Body | Status Codes |
|--------|------|-------------|---------------|--------------|
| `POST` | `/v1/chat/completions` | `ChatCompletionRequest` (JSON) | `ChatCompletionResponse` (JSON) or SSE stream of `ChatCompletionChunk` | 200, 400, 502, 503 |
| `GET` | `/v1/models` | None | `ModelList` (JSON) | 200 |
| `POST` | `/api/v1/query` | Passthrough JSON | Passthrough JSON from Mimir | 200, 400, 500, 502 |
| `POST` | `/api/v1/store` | Passthrough JSON | Passthrough JSON from Mimir | 201, 400, 409, 500, 502 |
| `GET` | `/health` | None | `HealthResponse` (JSON) | 200 (always) |
| `GET` | `/metrics` | None | Prometheus text | 200 |

### Ollama API Calls Made by Odin

| Ollama Endpoint | Purpose | Called By |
|----------------|---------|----------|
| `GET /api/tags` | List available models (Ollama backends) | `proxy::list_models` via `handlers::models_handler`, `handlers::health_handler` |
| `POST /api/chat` with `stream: true` | Streaming chat generation (Ollama backends) | `proxy::stream_chat` |
| `POST /api/chat` with `stream: false` | Non-streaming chat generation (Ollama backends) | `proxy::generate_chat` |

### OpenAI-Compatible API Calls Made by Odin

| Endpoint | Purpose | Called By |
|----------|---------|----------|
| `GET /v1/models` | List models (OpenAI-compat backends) | `proxy::list_models_openai` via `handlers::models_handler`, `handlers::health_handler` |
| `POST /v1/chat/completions` with `stream: true` | Streaming chat (OpenAI-compat backends) | `proxy::stream_chat_openai` |
| `POST /v1/chat/completions` with `stream: false` | Non-streaming chat (OpenAI-compat backends) | `proxy::generate_chat_openai` |

### Mimir API Calls Made by Odin

| Mimir Endpoint | Purpose | Called By |
|---------------|---------|----------|
| `POST /api/v1/query` | Fetch relevant engrams for RAG context | `rag::fetch_engram_context` |
| `POST /api/v1/store` | Store completed interaction as engram (fire-and-forget) | `handlers::spawn_engram_store` |
| `GET /health` | Health check probe | `handlers::check_service_health` |

### Muninn API Calls Made by Odin

| Muninn Endpoint | Purpose | Called By |
|----------------|---------|----------|
| `POST /api/v1/search` | Fetch relevant code chunks for RAG context | `rag::fetch_code_context` |
| `GET /health` | Health check probe | `handlers::check_service_health` |

### Home Assistant API Calls Made by Odin

| HA Endpoint | Purpose | Called By |
|-------------|---------|----------|
| `GET /api/states` | Fetch all entity states for HA domain summary | `rag::fetch_ha_domain_summary` via `ygg_ha::HaClient::get_states()` |

### Proxy Endpoints (Transparent)

For `/api/v1/query` and `/api/v1/store`, Odin acts as a transparent HTTP proxy to Mimir. The request body (`axum::body::Bytes`), response status code, content-type header, and response body are all forwarded unchanged. This allows the Fergus client's `EngramClient` to point at Odin's address without modification.

## Interface Boundaries

| Module | Owns | Exposes | Depends On |
|--------|------|---------|------------|
| `odin::main` | Process lifecycle, CLI parsing, config loading, Axum router setup, Prometheus recorder install, HA client construction, sd-notify, watchdog, graceful shutdown | Nothing (binary entrypoint) | `odin::handlers`, `odin::state`, `odin::router`, `odin::metrics`, `ygg_domain::config::OdinConfig`, `ygg_ha::HaClient` |
| `odin::state` | Shared application state: reqwest client, semantic router, backend semaphores, HA client, HA context cache, config | `AppState`, `BackendState` | `odin::router::SemanticRouter`, `ygg_domain::config::{BackendType, OdinConfig}`, `ygg_ha::HaClient` |
| `odin::handlers` | HTTP request/response translation, RAG orchestration, streaming lifecycle, engram store-on-completion, health checks, Mimir proxy | `chat_handler()`, `models_handler()`, `health_handler()`, `proxy_query()`, `proxy_store()` | `odin::state::AppState`, `odin::proxy`, `odin::rag`, `odin::openai`, `odin::error::OdinError`, `ygg_domain::config::BackendType` |
| `odin::router` | Keyword-based intent classification, model-to-backend resolution | `SemanticRouter`, `RoutingDecision`, `SemanticRouter::classify()`, `SemanticRouter::resolve_backend_for_model()` | `ygg_domain::config::{RoutingConfig, BackendConfig, BackendType}` |
| `odin::proxy` | Backend HTTP communication, Ollama NDJSON-to-SSE conversion, OpenAI SSE pass-through, non-streaming generation, model listing | `stream_chat()`, `generate_chat()`, `stream_chat_openai()`, `generate_chat_openai()`, `list_models()`, `list_models_openai()` | `odin::openai` types, `odin::error::OdinError`, `reqwest` |
| `odin::rag` | Parallel context fetching from Muninn and Mimir, HA domain summary cache, system prompt assembly | `fetch_context()`, `build_system_prompt()`, `RagContext` | `odin::state::AppState`, `ygg_ha::EntityState` |
| `odin::openai` | All OpenAI-compatible type definitions, Ollama type definitions | All request/response/chunk/model structs | None (leaf module, pure types) |
| `odin::error` | Error type unification, Axum `IntoResponse` implementation | `OdinError` enum | `axum::response`, `reqwest::Error`, `thiserror` |
| `odin::metrics` | Prometheus metrics middleware and recording helpers | `metrics_middleware()`, `record_routing_intent()`, `record_llm_generation()`, `adjust_backend_active()` | `axum::middleware`, `metrics` crate |

**Ownership rules:**
- Only `odin::proxy` communicates with Ollama `/api/chat` and `/api/tags` or OpenAI `/v1/chat/completions` and `/v1/models`. No other module touches backends directly.
- Only `odin::rag` communicates with Muninn and Mimir for context fetching (and HA for domain summary). The Mimir proxy endpoints in `odin::handlers` bypass `rag` and forward directly.
- Only `odin::router` decides which model and backend to use. `handlers` consults the router but contains no routing logic.
- Only `odin::handlers` deserializes HTTP requests or serializes HTTP responses. `proxy` and `rag` work with internal types.
- `odin::openai` is a pure type definition module with no I/O or business logic.
- `odin::state` is a data holder. Construction happens in `main.rs`. No business logic methods.
- Odin does NOT depend on `ygg-store` or `ygg-embed` at runtime. All interactions with Mimir, Muninn, and backends are via HTTP.

## Semantic Router Design

### Intent Classification

The semantic router uses keyword matching (v1) to classify the last user message into one of three intents:

| Intent | Keywords (substring match, case-insensitive) | Target Backend | Target Model |
|--------|----------------------------------------------|----------------|-------------|
| `coding` | implement, function, code, bug, error, compile, test, refactor, debug, syntax, struct, enum, trait, fn, class, method, variable, import, module, crate, cargo, rustc, clippy, lint, type | munin (localhost:11434) | qwen3-coder:30b-a3b-q4_K_M |
| `reasoning` | explain, why, how, analyze, design, architecture, plan, compare, evaluate, reason, think, consider, strategy, tradeoff, trade-off, pros, cons, overview | hugin (REDACTED_HUGIN_IP:11434) | qwen3:30b-a3b |
| `home_automation` | home assistant, home automation, smart home, iot, automation, light, switch, sensor, entity, hass, "ha ", thermostat, scene, script, trigger, action, climate, cover, fan, lock, alarm, binary_sensor, media_player, camera, vacuum, garage, door, temperature, humidity, motion, occupancy, energy, power, battery, turn on, turn off, toggle, brightness, color, heating, cooling | hugin (REDACTED_HUGIN_IP:11434) | qwen3:30b-a3b |
| `default` (no match) | N/A | munin (localhost:11434) | qwen3-coder:30b-a3b-q4_K_M |

### Classification Algorithm

1. Lowercase the user message.
2. For each compiled rule, count the number of keyword substrings found in the lowercased message.
3. The rule with the highest match count wins. Ties break by rule order (first defined wins).
4. If no rule has count > 0, use the default model/backend.

### Explicit Model Override

When `request.model` is set, the router skips classification and resolves the backend directly from the `model_to_backend` map. Returns 400 if the model is not found on any configured backend.

### HA-Aware Routing

When `intent == "home_assistant" || intent == "home_automation"`:
- Muninn code context fetch is **skipped** (irrelevant for HA queries).
- Mimir engram context fetch is **kept** (prior HA interactions provide conversational memory).
- HA domain summary is **injected** into the system prompt from a 60-second in-memory cache.

## RAG Pipeline Design

### Fetch Phase (parallel)

```
                                   +-- fetch_code_context() --> Muninn POST /api/v1/search (3s timeout)
tokio::join! --+
                                   +-- fetch_engram_context() --> Mimir POST /api/v1/query (3s timeout)
```

- For HA intents: Muninn leg is skipped, only Mimir is queried.
- Both queries are best-effort. Failures logged as warnings, not errors.
- Total latency: `max(muninn_latency, mimir_latency)`, not their sum.

### HA Context Fetch (sequential, after join)

For HA intents, `fetch_ha_domain_summary()` checks the 60s cache:
1. Fast path: read lock, return cached if fresh.
2. Slow path: write lock (with double-check to prevent thundering herd), call `HaClient::get_states()` with 5s timeout.
3. Build domain summary: group entities by domain, count per domain.

### Injection Phase

`build_system_prompt()` assembles the system message:

```
Base prompt (always present)
  |
  +-- ## Relevant Code Context (if Muninn returned results)
  |     {code_context}
  |
  +-- ## Relevant Memories (if Mimir returned results)
  |     - [0.95] Cause: "..." -> Effect: "..."
  |
  +-- ## Home Assistant Context (if HA intent + HA configured)
        - light: 12 entities
        - switch: 8 entities
        ...
```

The system prompt is prepended to or merged with the first message:
- If `messages[0].role == system`: append RAG context to existing system content.
- Otherwise: insert new system message at index 0.

## Streaming Architecture

### Ollama Backend (NDJSON to SSE conversion)

```
Ollama /api/chat --> bytes_stream --> line buffer (split on \n)
    --> parse OllamaStreamLine --> build ChatCompletionChunk
    --> serialize to JSON --> wrap as Event::default().data(json)
    --> yield; on done=true, also yield Event::default().data("[DONE]")
```

- Line buffer capped at 10MB to prevent OOM.
- Malformed JSON lines are skipped with a warning (stream continues).
- First chunk carries `delta.role = Some(Role::Assistant)`.
- Final chunk carries `finish_reason = Some("stop")`.

### OpenAI-Compatible Backend (SSE pass-through)

```
Backend /v1/chat/completions --> bytes_stream --> line buffer (split on \n)
    --> strip "data: " prefix --> yield as Event::default().data(payload)
    --> stop on "[DONE]"
```

The upstream already emits correct OpenAI SSE, so Odin re-frames each `data:` line into an Axum `Event` without JSON parsing.

## Concurrency Control

### Per-Backend Semaphore

Each `BackendState` holds an `Arc<tokio::sync::Semaphore>` with `max_concurrent` permits (default 2).

```rust
// Non-blocking acquire -- returns 503 immediately if at capacity.
let _permit = backend_state.semaphore.try_acquire()
    .map_err(|_| OdinError::BackendUnavailable(...))?;
```

The permit is held for the duration of the handler (non-streaming) or until the SSE stream is dropped (streaming). For non-streaming, the permit drops when the handler function returns. For streaming, the permit drops when the `Sse` response body is fully consumed or the client disconnects.

### Global Concurrency

`tower::limit::ConcurrencyLimitLayer::new(64)` bounds total in-flight requests across all routes.

## Configuration

### Config File: `configs/odin/node.yaml`

```yaml
node_name: "odin"
listen_addr: "0.0.0.0:8080"

backends:
  - name: "munin"
    url: "http://localhost:11434"
    backend_type: "ollama"
    models:
      - "qwen3-coder:30b-a3b-q4_K_M"
      - "qwen3-embedding"
    max_concurrent: 2
  - name: "hugin"
    url: "http://REDACTED_HUGIN_IP:11434"
    backend_type: "ollama"
    models:
      - "qwen3:30b-a3b"
      - "qwen3-embedding"
    max_concurrent: 2

routing:
  default_model: "qwen3-coder:30b-a3b-q4_K_M"
  rules:
    - intent: "coding"
      model: "qwen3-coder:30b-a3b-q4_K_M"
      backend: "munin"
    - intent: "reasoning"
      model: "qwen3:30b-a3b"
      backend: "hugin"
    - intent: "home_automation"
      model: "qwen3:30b-a3b"
      backend: "hugin"

mimir:
  url: "http://localhost:9090"
  query_limit: 3
  store_on_completion: true

muninn:
  url: "http://REDACTED_HUGIN_IP:9091"
  max_context_chunks: 10

ha:
  url: "http://REDACTED_CHIRP_IP:8123"
  token: "${HA_TOKEN}"
  timeout_secs: 10
```

### Config Structs (in `ygg_domain::config`)

| Struct | Fields | Purpose |
|--------|--------|---------|
| `OdinConfig` | node_name, listen_addr, backends, routing, mimir, muninn, ha (Option) | Top-level Odin config |
| `BackendConfig` | name, url, backend_type (default: Ollama), models, max_concurrent (default: 2) | Backend definition |
| `BackendType` | Ollama, Openai | Protocol selector |
| `RoutingConfig` | default_model, rules | Semantic router config |
| `RoutingRule` | intent, model, backend | Single routing rule |
| `MimirClientConfig` | url, query_limit (default: 5), store_on_completion (default: true) | Mimir connection config |
| `MuninnClientConfig` | url, max_context_chunks (default: 10) | Muninn connection config |
| `HaConfig` | url, token, timeout_secs (default: 10) | HA connection config |

### CLI

```
odin --config configs/odin/node.yaml [--listen-addr 0.0.0.0:9000]
```

- `--config` / `-c`: YAML config file path (default: `configs/odin/node.yaml`)
- `--listen-addr` / `ODIN_LISTEN_ADDR`: Override listen address from config

### systemd Unit

`deploy/systemd/yggdrasil-odin.service`:
- Type=notify, sd-notify ready signal on successful bind
- Requires=yggdrasil-mimir.service, waits for Mimir health
- ExecStartPre: `wait-for-health.sh http://localhost:9090/health 30`
- Environment: RUST_LOG=info, HA_TOKEN (populated during deployment)
- Restart=on-failure, RestartSec=5, LimitNOFILE=65536

## File Manifest

| File | Status | Lines | Description |
|------|--------|-------|-------------|
| `crates/odin/src/main.rs` | IMPLEMENTED | 273 | Process entry point, CLI, config, Axum router, sd-notify, watchdog, graceful shutdown |
| `crates/odin/src/lib.rs` | IMPLEMENTED | 19 | Module declarations |
| `crates/odin/src/openai.rs` | IMPLEMENTED | 229 | OpenAI + Ollama type definitions (pure types, no I/O) |
| `crates/odin/src/error.rs` | IMPLEMENTED | 107 | OdinError enum, IntoResponse impl, From<reqwest::Error> |
| `crates/odin/src/router.rs` | IMPLEMENTED | 353 | SemanticRouter: keyword classification, model resolution, BackendType tracking |
| `crates/odin/src/state.rs` | IMPLEMENTED | 54 | AppState, BackendState data holders |
| `crates/odin/src/proxy.rs` | IMPLEMENTED | 446 | Ollama + OpenAI backend HTTP proxy, NDJSON-to-SSE, SSE pass-through, model listing |
| `crates/odin/src/rag.rs` | IMPLEMENTED | 362 | Parallel RAG fetch, HA domain summary cache, system prompt assembly |
| `crates/odin/src/handlers.rs` | IMPLEMENTED | 593 | All route handlers: chat, models, health, proxy_query, proxy_store |
| `crates/odin/src/metrics.rs` | IMPLEMENTED | 78 | Prometheus metrics middleware and recording helpers |
| `crates/odin/Cargo.toml` | IMPLEMENTED | 36 | Dependencies: ygg-domain, ygg-ha, axum, reqwest, tokio, etc. |
| `configs/odin/node.yaml` | IMPLEMENTED | 45 | Production config with real model names and service URLs |
| `deploy/systemd/yggdrasil-odin.service` | IMPLEMENTED | 27 | systemd unit for production deployment |

## Acceptance Criteria

- [x] `cargo build --release -p odin` compiles with zero errors and zero warnings
- [x] `odin --config configs/odin/node.yaml` starts and logs "odin ready on 0.0.0.0:8080"
- [x] `GET /health` returns HTTP 200 with `HealthResponse` showing backend and service statuses
- [x] `GET /v1/models` returns HTTP 200 with `ModelList` aggregating models from all reachable backends
- [x] `POST /v1/chat/completions` with coding keywords routes to qwen3-coder:30b-a3b-q4_K_M on Munin
- [x] `POST /v1/chat/completions` with reasoning keywords routes to qwen3:30b-a3b on Hugin
- [x] `POST /v1/chat/completions` with HA keywords routes to qwen3:30b-a3b on Hugin and skips Muninn context
- [x] `POST /v1/chat/completions` with no matching keywords routes to default model on Munin
- [x] Explicit `model` field routes to the hosting backend regardless of message content
- [x] Unknown model returns 400 with OpenAI-compatible error format
- [x] Empty messages returns 400 with OpenAI-compatible error format
- [x] Streaming response: `Content-Type: text/event-stream`, `data: {...}\n\n` SSE events, ends with `data: [DONE]\n\n`
- [x] Each SSE event deserializes as valid `ChatCompletionChunk`
- [x] Non-streaming response returns `Content-Type: application/json` with valid `ChatCompletionResponse`
- [x] Non-streaming response includes `usage` (prompt_tokens, completion_tokens) from Ollama when available
- [x] RAG context injected into system prompt (code context from Muninn, engram context from Mimir)
- [x] RAG failure is graceful: chat completion works without Muninn or Mimir
- [x] HA domain summary injected for home_automation intent (60s cache)
- [x] Engram store-on-completion: fire-and-forget POST to Mimir after non-streaming completion
- [x] Per-backend semaphore: `try_acquire()` returns 503 when at capacity
- [x] `POST /api/v1/query` and `/api/v1/store` proxy transparently to Mimir (body, status, content-type preserved)
- [x] Fergus client `EngramClient` works against Odin proxy endpoints without modification
- [x] `GET /metrics` returns Prometheus exposition format text
- [x] CORS headers present on all responses
- [x] Graceful shutdown on SIGTERM/SIGINT drains in-flight requests
- [x] Config loads from YAML, listen address overridable via CLI or env var
- [x] systemd unit (Type=notify) sends ready signal after TCP bind
- [ ] Performance: Routing P95 < 5ms (not yet benchmarked)
- [ ] Performance: RAG fetch P95 < 200ms when services healthy (not yet benchmarked)
- [ ] Performance: TTFT P95 < 500ms with cached model (not yet benchmarked)
- [ ] Performance: RSS < 80MB at steady state (not yet measured in production)

## Dependencies

| Dependency | Type | Status | Blocking? |
|------------|------|--------|-----------|
| Sprint 001 (Foundation) | Sprint | DONE | No |
| Sprint 002 (Mimir MVP) | Sprint | DONE | Yes (Mimir required for RAG + proxy) |
| Sprint 003 (Huginn MVP) | Sprint | DONE | No (Odin degrades gracefully without indexed data) |
| Sprint 004 (Muninn MVP) | Sprint | DONE | No (Odin degrades gracefully without Muninn) |
| Sprint 011 (Hardening) | Sprint | DONE | Provides systemd unit and deploy scripts |
| Sprint 013 (Hugin MoE Swap) | Sprint | DONE | Updated Hugin model from QwQ-32B to qwen3:30b-a3b |
| Sprint 014 (Munin IPEX) | Sprint | DONE | IPEX-LLM container replaces native Ollama on Munin |
| Munin Ollama (IPEX-LLM container) | Infrastructure | Running | Yes |
| Hugin Ollama (native, bound to 0.0.0.0) | Infrastructure | Running | Yes |
| Mimir service (port 9090) | Runtime | Running | Yes |
| Muninn service (port 9091) | Runtime | Running | No (best-effort) |
| Home Assistant (chirp, port 8123) | Runtime | Running | No (optional, graceful degradation) |
| `ygg_domain` crate | Code | Complete | No |

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Ollama model names change after pull/update | Chat completions fail with 404 from Ollama | Medium | Config lists model names explicitly; `/v1/models` dynamically queries Ollama. Config update only, no code change. |
| Ollama streaming format changes between versions | SSE conversion produces malformed chunks | Low | `OllamaStreamLine` uses `#[serde(default)]` for optional fields. Malformed lines are skipped. IPEX-LLM container pins Ollama version. |
| RAG timeout adds latency | User sees slow TTFT even when Ollama is fast | Medium | 3-second timeout bounds worst case. HA intents skip Muninn entirely. |
| Semaphore starvation under concurrent load | Requests return 503 | Low | `try_acquire()` returns 503 immediately. Client can retry or pick different model. `max_concurrent: 2` matches Ollama's serial inference reality. |
| HA token not set in environment | HA integration silently fails | Medium | `${HA_TOKEN}` env var must be populated in systemd unit. If empty/invalid, HA features degrade gracefully (no HA context, no error). |
| Streaming engram store records placeholder instead of full response | Incomplete engram stored | Low | Known limitation: streaming stores `[streaming response via {model}]` as effect. Full accumulation would require holding the entire response in memory during stream. Acceptable for MVP. |
| Fergus client points to Mimir directly, bypassing Odin | Engrams stored but no RAG enrichment | Low | Document that Fergus should target Odin (port 8080). Proxy is transparent, so no client code changes needed. |

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | Odin listens on port 8080 | Matches ARCHITECTURE.md. Avoids collision with Mimir (9090) and Muninn (9091). |
| 2026-03-09 | Keyword-based routing (v1), not embedding-based | Sub-millisecond latency, sufficient for three-intent taxonomy. Embedding upgrade deferred to future sprint. |
| 2026-03-09 | Hardcode keyword sets per intent | Implementation detail of routing heuristic, not user config. Discarded entirely when upgrading to embedding-based routing. |
| 2026-03-09 | RAG queries are best-effort with 3s timeout | Chat must work even if Muninn/Mimir are down. |
| 2026-03-09 | Parallel RAG fetch via `tokio::join!` | Reduces latency from sum to max of both queries. |
| 2026-03-09 | OpenAI-compatible API format | De facto standard. Tools (Aider, Cursor, Continue, Open WebUI) expect `/v1/chat/completions`. |
| 2026-03-09 | Convert Ollama NDJSON to SSE explicitly | Full control over output format. Ollama's experimental OpenAI-compat endpoint has SSE framing inconsistencies. |
| 2026-03-09 | Per-backend semaphore with `try_acquire()` -> 503 | Fail-fast prevents cascading backpressure. 503 is standard "try again later." |
| 2026-03-09 | Fire-and-forget engram store via `tokio::spawn` | Non-critical side-effect. Avoids adding latency to response path. |
| 2026-03-09 | Transparent Mimir proxy (body forwarded as-is) | Avoids coupling Odin to Mimir's schema. `axum::body::Bytes` + reqwest forwarding preserves status codes and headers. |
| 2026-03-09 | Odin does NOT depend on ygg-store or ygg-embed | HTTP-only orchestration layer. Database and embedding are Mimir/Huginn responsibilities. |
| 2026-03-09 | Health endpoint always returns HTTP 200 | Odin is healthy even if backends are degraded. Reverse proxies and health checkers should not mark Odin down when only a backend fails. |
| 2026-03-09 | Dual BackendType (Ollama + Openai) | Future-proofs for vLLM or other OpenAI-compatible inference servers. Currently all backends are Ollama. |
| 2026-03-09 | HA integration wired into Odin (not deferred to separate sprint) | HA context injection into RAG is a natural extension of the RAG pipeline. The `ygg-ha` crate was already a dependency. Sprint 007 defines the HA MCP tools and automation generation, which are separate concerns. |
| 2026-03-09 | HA domain summary cached for 60 seconds with RwLock double-check | Prevents hammering HA REST API on every request. 60s is fresh enough for entity state changes. Double-check prevents thundering herd on cache expiry. |
| 2026-03-09 | Global 64-request concurrency limit via tower | Prevents resource exhaustion from connection floods without per-client tracking. |
| 2026-03-09 | 10MB stream line buffer cap | Prevents OOM from pathological or malicious streaming input. |

---

**Status:** This sprint is DONE. All source files are implemented, compiled, and deployed to Munin via systemd.

**Performance benchmarking:** The 4 unchecked acceptance criteria (P95 latency and RSS measurements) are deferred to `qa-compliance-auditor` for formal validation against the live deployment.

**Known limitation:** Streaming engram storage records a placeholder `[streaming response via {model}]` as the effect rather than the full accumulated response text. Addressing this would require accumulating the full streamed content in the proxy layer and spawning the store task after the `[DONE]` event, which adds memory overhead proportional to response length. Acceptable for current workloads.
