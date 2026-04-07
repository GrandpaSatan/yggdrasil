Looking at the sprint document and current ARCHITECTURE.md, I need to identify any architectural changes introduced by this sprint.

The sprint document shows several changes that affect the architecture:

1. New workspace member crates (ygg-test-harness) with Mock builders
2. Schema consolidation in ygg-domain 
3. New McpServerConfig fields for workspace_id, antigravity_url, ide_type
4. AgentStepEvent + /v1/agent/stream SSE endpoint
5. ContextBridgeParams.workspace_id for cross-window isolation
6. PersistentSessionManager with workspace_id session isolation

These changes suggest updates to the service registry and crate architecture documentation.

Here's the updated ARCHITECTURE.md with the architectural delta:

```markdown
# Yggdrasil Architecture

## Overview

Yggdrasil is a distributed AI infrastructure platform composed of specialized Rust services running on a private LAN. It provides associative memory (engrams), semantic code indexing, LLM orchestration, MCP tool integration for IDE clients, Home Assistant smart-home control, cloud gaming VM management, and inference VM scheduling. Services communicate over HTTP/gRPC; all secrets and IPs are injected via environment variables — never hardcoded.

## Node Topology

| Node | Role | Services |
|------|------|----------|
| **Munin** `<munin-ip>` | Primary compute | Odin :8080, Mimir :9090, Sentinel, MCP Remote :9093, ygg-mcp-server (stdio), ygg-node, Ollama :11434 (IPEX-LLM), STT :9097, TTS :9095 |
| **Hugin** `<hugin-ip>` | Code indexing | Huginn :9092 (health), Muninn :9091, ygg-node, Ollama :11434 |
| **Hades** `<hades-ip>` | Storage only | PostgreSQL :5432 (pgvector), Qdrant :6334 |
| **Thor** `<thor-ip>` | Proxmox VE — compute | Gaming VMs (Harpy), Inference VMs (Morrigan), managed by ygg-gaming |
| **Plume** `<plume-ip>` | Proxmox VE — services | Nightjar (Docker/media), Chirp (Home Assistant :8123), Gitea (LXC), Peckhole (LXC) |

## System Topology

```mermaid
graph TB
    subgraph Munin["Munin (<munin-ip>)"]
        Odin["odin :8080\nLLM Orchestrator"]
        Mimir["mimir :9090\nEngram Memory"]
        McpRemote["ygg-mcp-remote :9093\n32 network tools"]
        STT["STT :9097\nSenseVoiceSmall (NPU)"]
        TTS["TTS :9095\nKokoro v1.0 (CPU)"]
        OllamaM["Ollama :11434\nIPEX-LLM container"]
    end

    subgraph Hugin["Hugin (<hugin-ip>)"]
        Huginn["huginn :9092\nCode Indexer"]
        Muninn["muninn :9091\nCode Retrieval"]
        OllamaH["Ollama :11434"]
    end

    subgraph Hades["Hades (<hades-ip>)"]
        PG["PostgreSQL :5432\npgvector"]
        QD["Qdrant :6334\n4096-dim cosine"]
    end

    subgraph Thor["Thor (<thor-ip>) — Proxmox VE"]
        Harpy["Harpy VM\nGaming (Sunshine)"]
        Morrigan["Morrigan VM\nInference (llama-server)"]
    end

    subgraph Plume["Plume (<plume-ip>) — Proxmox VE"]
        Chirp["chirp LXC\nHome Assistant :8123"]
        Nightjar["nightjar LXC\nDocker / Media"]
    end

    IDE["IDE (Claude Code)"] -->|"MCP StreamableHTTP"| McpRemote
    IDE -->|"VS Code Extension"| YggLocal["yggdrasil-local\n(MCP stdio + UI)"]
    McpRemote --> Odin
    McpRemote --> Muninn
    McpRemote --> Chirp
    Odin --> Mimir
    Odin --> Muninn
    Odin --> OllamaM
    Odin --> OllamaH
    Mimir --> PG
    Mimir --> QD
    Huginn --> PG
    Huginn --> QD
    Muninn --> PG
    Muninn --> QD
```

## Service Registry

| Service | Crate | Port | Responsibility |
|---------|-------|------|----------------|
| **Odin** | `crates/odin` | 8080 | OpenAI-compatible API gateway, semantic routing, RAG pipeline, SSE streaming, Mimir proxy, HA context injection, voice WebSocket pipeline (VAD → SDR skill cache → omni chat → legacy STT fallback), SDR skill cache (512 skills, LRU) |
| **Mimir** | `crates/mimir` | 9090 | Engram CRUD, embedding, SHA-256 dedup, LSH indexing, autonomous auto-ingest with SDR template matching |
| **Huginn** | `crates/huginn` | 9092 (health) | File watcher, tree-sitter AST chunking, code indexing daemon |
| **Muninn** | `crates/muninn` | 9091 | Semantic code retrieval (vector + BM25 fusion), read-only |
| **yggdrasil-local** | `extensions/yggdrasil-local` | stdio | VS Code extension + MCP server: 2 tools (`sync_docs`, `screenshot`), status bar, memory dashboard, JSONL event watcher |
| **ygg-mcp-remote** | `crates/ygg-mcp-remote` | 9093 | Remote MCP server: 32 tools + 3 resources over StreamableHTTP (code search, memory, LLM, HA, gaming, vault, deploy, config sync, web search) |

## Crate Architecture

### Service Crates

| Crate | Binary | Purpose |
|-------|--------|---------|
| `odin` | `odin` | LLM orchestrator (see Service Registry) |
| `mimir` | `mimir` | Engram memory service |
| `huginn` | `huginn` | Code indexer daemon |
| `muninn` | `muninn` | Code retrieval service |
| `ygg-gaming` | `ygg-gaming` | Multi-host Proxmox orchestrator — GPU pool, WoL, VM lifecycle per `VmRole` |
| `ygg-mcp-server` | `ygg-mcp-server` | Local MCP stdio server |
| `ygg-mcp-remote` | `ygg-mcp-remote` | Remote MCP HTTP server |
| `ygg-node` | `ygg-node` | Mesh node daemon (mDNS, heartbeats, gate policy, energy) |
| `ygg-sentinel` | `ygg-sentinel` | Log monitoring with SDR anomaly detection and voice alerts |
| `ygg-voice` | `ygg-voice` | Local audio bridge — mic capture → Odin WebSocket → TTS playback |
| `ygg-installer` | `ygg-installer` | Cross-platform install tool (systemd/launchd/Windows Service) |

### Library Crates

| Crate | Responsibility | Consumers |
|-------|---------------|-----------|
| `ygg-domain` | All type definitions: `Engram`, `CodeChunk`, `MemoryTier`, tool catalog (`tools::ALL_TOOLS`), domain errors. Zero I/O. | All crates |
| `ygg-store` | PostgreSQL connection pool, engram/chunk CRUD, Qdrant client | mimir, huginn, muninn |
| `ygg-embed` | Ollama embedding HTTP client — single and batch | mimir, huginn, muninn |
| `ygg-mcp` | MCP tool definitions, server handler, `memory_merge` module | ygg-mcp-server, ygg-mcp-remote |
| `ygg-ha` | Home Assistant REST client, automation YAML generation | ygg-mcp, odin |
| `ygg-config` | JSON/YAML config loader with `${ENV_VAR}` expansion, hot-reload | All services |
| `ygg-server` | Shared HTTP boilerplate: metrics middleware, graceful shutdown, sd_notify | odin, mimir, muninn, huginn, ygg-node |
| `ygg-mesh` | Mesh networking: mDNS discovery, gate policy, node registry, HTTP proxy | ygg-node |
| `ygg-energy` | Wake-on-LAN, power status, `ProxmoxClient` REST wrapper | ygg-gaming, ygg-node |
| `ygg-cloud` | Cloud LLM fallback adapters (OpenAI, Claude, Gemini) with rate limiting | odin |

## Data Flow: Standard Chat Completion

```
Claude Code → ygg-mcp-remote → Odin :8080
                                    │
                          ┌─────────┴─────────┐
       

## Sprint 051 Changes

- Added `ygg-test-harness` crate with MockOllamaBuilder, MockMimirBuilder, MockMuninnBuilder for testing
- Consolidated 32 parameter structs into `ygg-domain/src/tool_params.rs`
- Added `workspace_id` session isolation to `PersistentSessionManager`
- Introduced `AgentStepEvent` and `/v1/agent/stream` SSE endpoint
- Added `ContextBridgeParams.workspace_id` for cross-window isolation
- Extended `McpServerConfig` with `workspace_id`, `antigravity_url`, and `ide_type` fields
- Added 4 circuit breaker integration tests
- Implemented retry jitter (50-150% of base delay)
```

## Sprint 054 Changes

Internal error from Odin (HTTP 502 Bad Gateway): {"error":{"code":null,"message":"openai backend connection failed: error sending request for url (http://morrigan.local:8080/v1/chat/completions)","type":"server_error"}}