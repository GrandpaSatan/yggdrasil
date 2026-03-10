# Yggdrasil Architecture

## Overview

Yggdrasil is a distributed AI memory and retrieval system composed of specialized Rust services that communicate over HTTP/gRPC on a private LAN. It provides associative memory (engrams), code indexing, semantic retrieval, MCP tool integration for IDEs, and Home Assistant smart-home control for the Fergus AI assistant.

## System Topology

```mermaid
graph TB
    subgraph Munin["Munin (REDACTED_MUNIN_IP)"]
        Odin["odin :8080<br/>LLM Orchestrator"]
        Mimir["mimir :9090<br/>Engram Memory"]
        McpServer["ygg-mcp-server<br/>MCP stdio server"]
        OllamaM["Ollama :11434<br/>IPEX-LLM container<br/>qwen3-coder:30b-a3b-q4_K_M<br/>qwen3-embedding (4096-dim)"]
    end

    subgraph Hugin["Hugin (REDACTED_HUGIN_IP)"]
        Huginn["huginn<br/>Code Indexer (daemon)"]
        Muninn["muninn :9091<br/>Code Retrieval"]
        OllamaH["Ollama :11434<br/>qwen3:30b-a3b (reasoning)<br/>qwen3-embedding (4096-dim)"]
    end

    subgraph MuninDB["Munin (REDACTED_MUNIN_IP) - Database"]
        PG["PostgreSQL :5432<br/>pgvector Docker container<br/>yggdrasil schema"]
    end

    subgraph Hades["Hades (REDACTED_HADES_IP)"]
        QD["Qdrant :6334<br/>Vector Search<br/>4096-dim cosine"]
    end

    subgraph Plume["Plume (REDACTED_PLUME_IP)"]
        HA["chirp :8123<br/>Home Assistant"]
    end

    IDE["IDE Client<br/>(Claude Code / VS Code)"] -->|"MCP stdio<br/>JSON-RPC"| McpServer
    McpServer -->|"HTTP :8080<br/>/v1/chat/completions<br/>/api/v1/query, /api/v1/store"| Odin
    McpServer -->|"HTTP :9091<br/>/api/v1/search"| Muninn
    McpServer -->|"HTTP :8123<br/>/api/states, /api/services"| HA
    Fergus["fergus-rs<br/>(External Client)"] -->|"HTTP :8080<br/>/v1/chat/completions<br/>/api/v1/query, /api/v1/store"| Odin
    Odin -->|"HTTP proxy<br/>/api/v1/query, /api/v1/store"| Mimir
    Odin -->|"HTTP<br/>/api/v1/search (RAG)"| Muninn
    Odin -->|"HTTP<br/>/api/chat (coding)"| OllamaM
    Odin -->|"HTTP<br/>/api/chat (reasoning)"| OllamaH
    Odin -->|"HTTP<br/>/api/v1/query (RAG)"| Mimir
    Odin -->|"HTTP :8123<br/>HA context (cached)"| HA
    Mimir -->|HTTP /api/embeddings| OllamaM
    Mimir -->|SQL| PG
    Mimir -->|gRPC :6334| QD
    Huginn -->|HTTP /api/embeddings| OllamaH
    Huginn -->|SQL| PG
    Huginn -->|gRPC :6334| QD
    Muninn -->|HTTP /api/embeddings| OllamaH
    Muninn -->|SQL| PG
    Muninn -->|gRPC :6334| QD
```

## Service Registry

| Service | Crate | Binary | Port | Responsibility | Owned Data | Status |
|---------|-------|--------|------|----------------|------------|--------|
| **Odin** | `crates/odin` | `odin` | 8080 | OpenAI-compatible API gateway, semantic routing, RAG pipeline, SSE streaming, Mimir proxy, HA context injection, Prometheus metrics | Routing rules (in-memory from config), HA context cache (60s TTL) | DONE (Sprint 005) |
| **Mimir** | `crates/mimir` | `mimir` | 9090 | Engram memory CRUD, embedding, dedup, LSH indexing | `yggdrasil.engrams`, `yggdrasil.lsh_buckets`, Qdrant `engrams` collection | DONE (Sprint 002) |
| **Huginn** | `crates/huginn` | `huginn` | 9092 (health) | File watcher, tree-sitter AST chunking, code indexing | `yggdrasil.indexed_files`, `yggdrasil.code_chunks`, Qdrant `code_chunks` collection | DONE (Sprint 003) |
| **Muninn** | `crates/muninn` | `muninn` | 9091 | Semantic code retrieval (vector + BM25 fusion) | Read-only from Huginn's tables | DONE (Sprint 004) |
| **ygg-mcp-server** | `crates/ygg-mcp-server` | `ygg-mcp-server` | N/A (stdio) | MCP server exposing 9 tools (code search, memory, generation, 4 HA tools) and 2 resources to IDE clients via JSON-RPC over stdin/stdout | None (stateless bridge) | DONE (Sprint 006) |

## Shared Libraries

| Crate | Responsibility | Dependents |
|-------|---------------|------------|
| `ygg-domain` | All type definitions: `Engram`, `CodeChunk`, `MemoryTier`, config structs, domain errors. Leaf crate with zero I/O. | All services |
| `ygg-store` | PostgreSQL connection pool (`Store`), engram CRUD, chunk CRUD, Qdrant client (`VectorStore`). All database I/O. | mimir, huginn, muninn |
| `ygg-embed` | Ollama embedding HTTP client (`EmbedClient`). Single and batch embedding. | mimir, huginn, muninn |
| `ygg-mcp` | MCP tool/resource definitions, server handler, tool implementations (code search, memory, generation, HA). Library crate. | ygg-mcp-server |
| `ygg-ha` | Home Assistant REST API client (`HaClient`), automation YAML generation (`AutomationGenerator`). | ygg-mcp, odin |

## Data Flow: Engram Store

```mermaid
sequenceDiagram
    participant C as Fergus Client
    participant M as Mimir
    participant O as Ollama
    participant PG as PostgreSQL
    participant QD as Qdrant

    C->>M: POST /api/v1/store {cause, effect}
    M->>M: SHA-256(cause + effect) for dedup
    M->>O: POST /api/embeddings {model, prompt: cause}
    O-->>M: {embedding: [f32; 4096]}
    M->>PG: INSERT INTO engrams (embedding, hash, ...)
    PG-->>M: OK / 23505 (duplicate)
    M->>QD: Upsert point (id, embedding)
    QD-->>M: OK
    M->>M: LSH index insert
    M-->>C: 201 {id: "uuid"}
```

## Data Flow: Engram Query

```mermaid
sequenceDiagram
    participant C as Fergus Client
    participant M as Mimir
    participant O as Ollama
    participant QD as Qdrant
    participant PG as PostgreSQL

    C->>M: POST /api/v1/query {text, limit}
    M->>O: POST /api/embeddings {model, prompt: text}
    O-->>M: {embedding: [f32; 4096]}
    M->>QD: Search(embedding, limit)
    QD-->>M: [(uuid, score), ...]
    M->>PG: SELECT * FROM engrams WHERE id = ANY($1)
    PG-->>M: [Engram, ...]
    M->>PG: UPDATE access_count, last_accessed
    M-->>C: 200 [{id, cause, effect, similarity}, ...]
```

## Data Flow: Chat Completion (Odin Orchestrator)

```mermaid
sequenceDiagram
    participant C as Client (Fergus / UI)
    participant O as Odin :8080
    participant R as SemanticRouter
    participant Mn as Muninn :9091
    participant Mi as Mimir :9090
    participant OL as Ollama (Munin or Hugin)

    C->>O: POST /v1/chat/completions {messages, stream}
    O->>R: classify(last_user_message)
    R-->>O: RoutingDecision {model, backend_url}
    O->>O: acquire backend semaphore

    par RAG Context Fetch
        O->>Mn: POST /api/v1/search {query}
        Mn-->>O: {results, context}
    and
        O->>Mi: POST /api/v1/query {text}
        Mi-->>O: [{cause, effect, similarity}]
    end

    O->>O: build_system_prompt(code_context, engram_context)
    O->>O: inject system prompt into messages

    alt stream: true
        O->>OL: POST /api/chat {model, messages, stream: true}
        loop Newline-delimited JSON
            OL-->>O: {"message":{"content":"token"},"done":false}
            O-->>C: data: {"choices":[{"delta":{"content":"token"}}]}
        end
        OL-->>O: {"done":true}
        O-->>C: data: [DONE]
    else stream: false
        O->>OL: POST /api/chat {model, messages, stream: false}
        OL-->>O: {"message":{"content":"full response"},"done":true}
        O-->>C: {"choices":[{"message":{"content":"full response"}}]}
    end

    O->>O: release backend semaphore
    O-)Mi: POST /api/v1/store {cause, effect} (fire-and-forget)
```

## Data Flow: Mimir Proxy (Fergus Compatibility)

```mermaid
sequenceDiagram
    participant F as Fergus Client
    participant O as Odin :8080
    participant M as Mimir :9090

    F->>O: POST /api/v1/query {text, limit}
    O->>M: POST /api/v1/query {text, limit} (passthrough)
    M-->>O: [{id, cause, effect, similarity}]
    O-->>F: [{id, cause, effect, similarity}] (passthrough)
```

## Data Flow: MCP Tool Call (Sprint 006)

```mermaid
sequenceDiagram
    participant IDE as IDE Client (Claude Code)
    participant MCP as ygg-mcp-server (stdio)
    participant O as Odin :8080
    participant Mn as Muninn :9091

    IDE->>MCP: JSON-RPC tools/call {search_code, {query: "fn main"}}
    MCP->>Mn: POST /api/v1/search {query, limit}
    Mn-->>MCP: {results: [{file_path, content, score}]}
    MCP->>MCP: format results as markdown
    MCP-->>IDE: JSON-RPC result {content: [{type: text, text: "## Code Search..."}]}
```

## Data Flow: HA Automation Generation (Sprint 006)

```mermaid
sequenceDiagram
    participant IDE as IDE Client
    participant MCP as ygg-mcp-server (stdio)
    participant HA as Home Assistant :8123
    participant O as Odin :8080
    participant OL as Ollama (Hugin, qwen3:30b-a3b)

    IDE->>MCP: JSON-RPC tools/call {ha_generate_automation, {description: "..."}}
    MCP->>HA: GET /api/states (entity context)
    HA-->>MCP: [{entity_id, state, attributes}]
    MCP->>HA: GET /api/services (service context)
    HA-->>MCP: [{domain, services}]
    MCP->>MCP: build automation prompt with entity/service context
    MCP->>O: POST /v1/chat/completions {model: qwen3:30b-a3b, messages, stream: false}
    O->>OL: POST /api/chat {model: qwen3:30b-a3b, messages}
    OL-->>O: {message: {content: "```yaml\n..."}}
    O-->>MCP: {choices: [{message: {content: "```yaml\n..."}}]}
    MCP->>MCP: extract YAML from response
    MCP-->>IDE: JSON-RPC result {content: [{type: text, text: "## Generated Automation\n```yaml\n..."}]}
```

## External Services

| Service | Host | Port | Protocol | Used By |
|---------|------|------|----------|---------|
| Home Assistant | chirp (REDACTED_CHIRP_IP) | 8123 | HTTP REST + Bearer token | ygg-ha (via ygg-mcp-server and odin) |
| Ollama (Munin) | localhost (IPEX-LLM container) | 11434 | HTTP | odin, mimir |
| Ollama (Hugin) | REDACTED_HUGIN_IP | 11434 | HTTP | odin, huginn, muninn |
| PostgreSQL | Munin (localhost, pgvector Docker) | 5432 | SQL | mimir, huginn, muninn (via ygg-store) |
| Qdrant | hades (REDACTED_HADES_IP) | 6334 | gRPC | mimir, huginn, muninn (via ygg-store) |

## Database Schema

All tables live in the `yggdrasil` schema on PostgreSQL (pgvector Docker container on Munin, localhost:5432).

### Engram Tables (Migration 001)
- `yggdrasil.engrams` -- cause-effect memory pairs with pgvector embeddings
- `yggdrasil.lsh_buckets` -- LSH index persistence (table_idx, bucket_hash, engram_id)

### Code Index Tables (Migration 002)
- `yggdrasil.indexed_files` -- tracked source files with content hashes
- `yggdrasil.code_chunks` -- AST-extracted semantic units with tsvector for BM25

### Qdrant Collections (on Hades REDACTED_HADES_IP:6334)
- `engrams` -- 4096-dim cosine, point IDs match `engrams.id`
- `code_chunks` -- 4096-dim cosine, point IDs match `code_chunks.id`

## Configuration

Each service loads its config from `configs/<service>/config.yaml`. Config structs are defined in `ygg_domain::config`. CLI flags can override specific values (e.g., `--database-url`).

---

## Changelog

| Date | Change | Author |
|------|--------|--------|
| 2026-03-09 | Initial architecture document. Service registry, data flows, schema overview. | system-architect |
| 2026-03-09 | Updated topology: Huginn and Muninn on Hugin (REDACTED_HUGIN_IP), Odin and Mimir on Munin (REDACTED_MUNIN_IP). Added Odin chat completion and Mimir proxy data flows. Updated service registry with Sprint 005 Odin details. | system-architect |
| 2026-03-09 | Added ygg-mcp-server to topology and service registry (Sprint 006). Added MCP tool call data flow. Added chirp (Home Assistant) to topology. Added HA automation generation data flow (Sprint 007). Added External Services table. Updated ygg-mcp and ygg-ha library descriptions. | system-architect |
| 2026-03-09 | Sprint 008 planned: Mimir Advanced Memory Management -- hierarchical summarization, Core tier injection, sliding-window eviction. Sprint 009 planned: Hardware Optimization -- iGPU SYCL, AVX-512, Exo eval, candle embedder. Sprint 010 planned: Production Hardening -- systemd units, Prometheus metrics, backup, deployment scripts, graceful degradation. Huginn gains health listener on port 9092. | system-architect |
| 2026-03-09 | Sprint 005 finalized as DONE. Corrected stale references: Hugin model updated from QwQ-32B to qwen3:30b-a3b (Sprint 013). Embedding dimension corrected from 1024 to 4096 (qwen3-embedding actual output). PostgreSQL location corrected from Hades to Munin pgvector Docker container. Munin Ollama annotated as IPEX-LLM container (Sprint 014). Huginn port 9092 added to service registry. All service statuses updated to DONE. | system-architect |
| 2026-03-09 | Sprint 006 finalized as DONE. ygg-mcp-server status updated to DONE in service registry. HA tools merged into Sprint 006 (originally planned for Sprint 007). HA automation data flow re-attributed from Sprint 007 to Sprint 006. 9 tools + 2 resources fully implemented. Known discrepancy: AutomationGenerator requests model qwq-32b but actual Hugin model is qwen3:30b-a3b. | system-architect |
| 2026-03-09 | Sprint 010 (Production Hardening) finalized as DONE. Bug fixes applied: (1) all qwq-32b/QwQ-32B model references in ygg-ha and ygg-mcp-server replaced with qwen3:30b-a3b -- resolves the discrepancy noted in the Sprint 006 changelog entry; (2) HA_TOKEN env var expansion added to ygg-mcp-server startup; (3) backup-hades.sh PG host corrected from Hades (REDACTED_HADES_IP/postgres) to Munin (127.0.0.1/yggdrasil); (4) WatchdogSec=30 re-enabled in all 4 daemon systemd units (odin, mimir, huginn, muninn). Two deploy-only items remain for infra-devops: backup cron job installation on Munin, and NetworkHardware.md model reference update. 57 tests pass, zero qwq references remaining. | system-architect |
