# Yggdrasil USAGE Guide

All commands and endpoints for running, deploying, and operating the Yggdrasil AI homelab.

---

## Service Endpoints

### Odin — LLM Orchestrator (Munin :8080)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/chat/completions` | OpenAI-compatible chat. Body: `{model?, messages, stream?, session_id?, project_id?}` |
| `GET`  | `/v1/models` | List all LLM models across backends |
| `POST` | `/api/v1/query` | Proxy to Mimir: semantic engram query. Body: `{text, limit?}` |
| `POST` | `/api/v1/store` | Proxy to Mimir: store engram. Body: `{cause, effect, tags?}` |
| `POST` | `/api/v1/sdr/operations` | SDR set operations (and, or, xor, jaccard) on N input texts. Body: `{texts: [string], operation: "and"|"or"|"xor"|"jaccard"}` → returns `{sdr_hex: string, popcount: int, matched_engrams: [EngramEvent], jaccard_matrix?: [[float]]}` |
| `GET`  | `/health` | Health check (always HTTP 200, status in body) |
| `GET`  | `/metrics` | Prometheus text metrics |

**Session continuity:** Pass `session_id` (string UUID) to maintain multi-turn context. Odin stores history server-side. Pass `project_id` (e.g. `"yggdrasil"`) to enable cross-window context injection from previous sessions.

**Streaming:** `stream: true` (default) returns SSE chunks. `stream: false` returns JSON response. Response includes `x-session-id` header with the resolved session ID.

---

### Mimir — Engram Memory (Munin :9090)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/store` | Store new engram. Body: `{cause, effect, tags?: [string]}` |
| `POST` | `/api/v1/recall` | SDR dual-system recall. Body: `{text, limit?}` → returns `{engrams: [EngramEvent]}` |
| `POST` | `/api/v1/query` | Legacy semantic query (uses SDR). Body: `{text, limit?}` → returns `{results: [{cause, effect, similarity}]}` |
| `POST` | `/api/v1/sdr/operations` | SDR set operations (and, or, xor, jaccard) on N input texts. Body: `{texts: [string], operation: "and"|"or"|"xor"|"jaccard"}` → returns `{sdr_hex: string, popcount: int, matched_engrams: [EngramEvent], jaccard_matrix?: [[float]]}` |
| `GET`  | `/health` | Health check |
| `GET`  | `/metrics` | Prometheus metrics |

**Note:** Call Mimir via Odin proxy (`/api/v1/query`, `/api/v1/store`) rather than directly. Odin's proxy routes are compatible with the Fergus `EngramClient`.

---

### Muninn — Code Retrieval (Hugin :9091)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/search` | Hybrid code search (BM25 + Qdrant + RRF). Body: `{query, languages?: [string], limit?}` |
| `GET`  | `/health` | Health check |
| `GET`  | `/stats` | Index statistics (chunk count, last indexed, etc.) |
| `GET`  | `/metrics` | Prometheus metrics |

**Response shape:** `{results: [{chunk: {file_path, language, content, name, start_line, end_line}, score}], context: string}`

---

### Huginn — Code Indexer (Hugin :9092)

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/health` | Health check (also used for systemd watchdog) |
| `GET`  | `/metrics` | Prometheus metrics |

Huginn is a background daemon — no user-facing API. It watches configured paths, chunks code with tree-sitter, embeds chunks via ONNX, and writes to PostgreSQL + Qdrant.

**Watch paths (Hugin):**
- `/home/jhernandez/repos/Yggdrasil`
- `/home/jhernandez/repos/Fergus_Agent`
- `/mnt/workstation/docs/HardwareSetup` (SSHFS from workstation `REDACTED_WORKSTATION_IP`)

---

### MCP Servers — Claude Code Integration

The MCP layer is split into two servers (Sprint 027):

1. **`yggdrasil`** (remote, StreamableHTTP) — 12 network tools + 3 resources. Always-on, shared across IDE windows.
2. **`yggdrasil-local`** (local, stdio) — Filesystem tools only. One process per IDE window.

#### Remote Server — `yggdrasil` (Munin :9093)

**Binary:** `ygg-mcp-remote` at `/opt/yggdrasil/bin/ygg-mcp-remote`
**Config:** `/etc/yggdrasil/mcp-remote/config.yaml` (on Munin)
**Systemd:** `yggdrasil-mcp-remote.service`
**Claude Code config:** `type: "http"`, `url: "http://REDACTED_MUNIN_IP:9093/mcp"`

| Tool | Description |
|------|-------------|
| `search_code_tool` | Semantic code search via Muninn |
| `query_memory_tool` | Query Mimir engram memory |
| `store_memory_tool` | Store engram in Mimir |
| `generate_tool` | Generate via Odin (Qwen3-Coder) with session continuity |
| `list_models_tool` | List Ollama models via Odin |
| `get_sprint_history_tool` | Retrieve archived sprint summaries from Mimir |
| `ha_get_states_tool` | Get all Home Assistant entity states |
| `ha_list_entities_tool` | List HA entities by domain |
| `ha_call_service_tool` | Call HA service (device control) |
| `ha_generate_automation_tool` | Generate HA automation YAML via LLM |
| `memory_intersect_tool` | Proxied SDR set operations for Claude tool use |
| `task_delegate_tool` | Delegate code generation task to local Qwen3-30B with full Muninn+Mimir context |
| `diff_review_tool` | Perform memory-aware code review via local LLM |
| `context_bridge_tool` | Share context across IDE windows using Antigravity |

| Resource URI | Description |
|--------------|-------------|
| `yggdrasil://models` | Available models markdown table |
| `yggdrasil://memory/stats` | Mimir engram statistics |
| `yggdrasil://context/session` | Prefetched active sprint context |

#### Local Server — `yggdrasil-local` (Workstation stdio)

**Binary:** `ygg-mcp-server` at `target/release/ygg-mcp-server`
**Config:** `~/.config/yggdrasil/local-mcp.yaml`
**Claude Code config:** `type: "stdio"`, command points to binary + `--config` flag

| Tool | Description |
|------|-------------|
| `sync_docs_tool` | Sprint lifecycle: setup workspace, update USAGE.md on start, archive on end |

---

## Startup Commands

### Start services on Munin (REDACTED_MUNIN_IP)

```bash
# Start all Yggdrasil services
sudo systemctl start yggdrasil-odin yggdrasil-mimir

# Start Ollama IPEX container (if not running)
sudo systemctl start yggdrasil-ollama-ipex

# Check status
sudo systemctl status yggdrasil-odin yggdrasil-mimir
```

### Start services on Hugin (REDACTED_HUGIN_IP)

```bash
# Start Huginn + Muninn
sudo systemctl start yggdrasil-huginn yggdrasil-muninn

# Start native Ollama
sudo systemctl start ollama

# Check status
sudo systemctl status yggdrasil-huginn yggdrasil-muninn
```

### Local development (workstation)

```bash
# Run Odin locally (uses configs/odin/node.yaml by default)
cd /home/jesus/Documents/HardwareSetup/yggdrasil
cargo run --release --bin odin -- --config configs/odin/node.yaml

# Run MCP server locally
cargo run --release --bin ygg-mcp-server -- --config configs/mcp-server/config.yaml
```

---

## View Logs

```bash
# Munin
sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "sudo journalctl -u yggdrasil-odin -f"
sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "sudo journalctl -u yggdrasil-mimir -f"

# Hugin
sshpass -p 723559 ssh jhernandez@REDACTED_HUGIN_IP "sudo journalctl -u yggdrasil-huginn -f"
sshpass -p 723559 ssh jhernandez@REDACTED_HUGIN_IP "sudo journalctl -u yggdrasil-muninn -f"
```

---

## Deploy Commands

### Full install (first time on a node)

```bash
cd /home/jesus/Documents/HardwareSetup/yggdrasil
deploy/install.sh munin   # installs odin + mimir + systemd units + configs
deploy/install.sh hugin   # installs huginn + muninn + SSHFS mount
```

### Rolling update (redeploy a service)

```bash
# Update odin on Munin
deploy/update.sh munin odin

# Update mimir on Munin
deploy/update.sh munin mimir

# Update huginn + muninn on Hugin
deploy/update.sh hugin huginn
deploy/update.sh hugin muninn
```

The update script:
1. Builds the release binary locally (`cargo build --release --bin <service>`)
2. Rsyncs to the target node's home dir, sudo-mvs to `/opt/yggdrasil/bin/`
3. Restarts the systemd unit and waits for health

### Manual binary deploy (without update.sh)

```bash
cargo build --release --bin odin
rsync target/release/odin jhernandez@REDACTED_MUNIN_IP:/home/jhernandez/odin.new
sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
  "sudo mv /home/jhernandez/odin.new /opt/yggdrasil/bin/odin && \
   sudo systemctl restart yggdrasil-odin"
```

### Rollback

```bash
deploy/rollback.sh munin odin  # restores odin.prev binary and restarts
```

### Update MCP server binary (workstation)

```bash
cd /home/jesus/Documents/HardwareSetup/yggdrasil
cargo build --release --bin ygg-mcp-server
rsync target/release/ygg-mcp-server /home/jesus/.local/bin/ygg-mcp-server
# Restart Claude Code to reload the MCP server
```

---

## Database Admin

### PostgreSQL (Munin, via Docker)

```bash
# Connect to yggdrasil DB
sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
  "docker exec -it ygg-postgres psql -U jhernandez -d yggdrasil"

# Run migration manually
sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
  "cd /opt/yggdrasil && ./bin/odin --migrate-only"  # if supported

# Check engram count
sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
  "docker exec ygg-postgres psql -U jhernandez -d yggdrasil -c 'SELECT count(*) FROM engrams;'"
```

### Qdrant (Hades :6333)

```bash
# List collections
curl http://REDACTED_HADES_IP:6333/collections

# Collection details
curl http://REDACTED_HADES_IP:6333/collections/engrams_sdr
curl http://REDACTED_HADES_IP:6333/collections/code_chunks
```

---

## Backup

```bash
# Manual backup (runs on Munin cron at 03:00 daily)
sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "sudo /opt/yggdrasil/deploy/backup-hades.sh"

# Backup script location (on Munin)
# /opt/yggdrasil/deploy/backup-hades.sh
# - pg_dump yggdrasil → TrueNAS Hades via rsync
# - Qdrant snapshot → Hades

# Check last backup
sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "ls -la /mnt/hades/backups/ 2>/dev/null || echo 'check Hades mount'"
```

---

## Monitoring

- **Grafana:** http://REDACTED_NIGHTJAR_IP:3000 (dashboard uid: `ygg-observability`)
- **Prometheus:** http://REDACTED_NIGHTJAR_IP:9099 (scrapes `/metrics` from all 4 services)

### Quick health check (all services)

```bash
# Odin
curl -sf http://REDACTED_MUNIN_IP:8080/health | python3 -m json.tool

# Mimir
curl -sf http://REDACTED_MUNIN_IP:9090/health

# Muninn
curl -sf http://REDACTED_HUGIN_IP:9091/health

# Huginn
curl -sf http://REDACTED_HUGIN_IP:9092/health
```

---

## SSH Shortcuts

```bash
# Munin (Odin + Mimir)
sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP

# Hugin (Huginn + Muninn + Ollama)
sshpass -p 723559 ssh jhernandez@REDACTED_HUGIN_IP

# Hades (Qdrant + TrueNAS)
sshpass -p K6m4B129CF9u ssh jhernandez@REDACTED_HADES_IP

# Nightjar (Grafana + Prometheus)
sshpass -p 723559 ssh jhernandez@REDACTED_NIGHTJAR_IP
```

---

## SSHFS (Hugin → Workstation)

Hugin mounts the workstation's `/home/jesus/Documents` at `/mnt/workstation/docs` so Huginn can index local code.

```bash
# Check mount status on Hugin
sshpass -p 723559 ssh jhernandez@REDACTED_HUGIN_IP "systemctl status mnt-workstation-docs.mount"

# Remount if dropped
sshpass -p 723559 ssh jhernandez@REDACTED_HUGIN_IP "sudo systemctl restart mnt-workstation-docs.mount"

# Workstation sshd must be running
sudo systemctl status ssh  # on workstation (jesus@REDACTED_WORKSTATION_IP)
```

### Claude Code MCP Config (`~/.claude.json`)

```json
{
  "mcpServers": {
    "yggdrasil": {
      "type": "http",
      "url": "http://REDACTED_MUNIN_IP:9093/mcp"
    },
    "yggdrasil-local": {
      "type": "stdio",
      "command": "/path/to/ygg-mcp-server",
      "args": ["--config", "~/.config/yggdrasil/local-mcp.yaml"],
      "env": {}
    }
  }
}
```

### Workstation Bootstrap

```bash
cd ~/Yggdrasil
./deploy/workstation/ClaudeClient_Install
# → Project dir: /home/user/Yggdrasil (auto-detected, no parent .git)
# → project=yggdrasil  workspace=/home/user/Yggdrasil
```

---

## Sprint Lifecycle

### Starting a sprint

1. Create `/sprints/sprint-NNN.md` with the full sprint plan.
2. Call `sync_docs_tool(event: "sprint_start", sprint_id: "NNN", sprint_content: <full plan>)` — this updates USAGE.md and checks /docs/ invariants.

### Ending a sprint

1. Call `sync_docs_tool(event: "sprint_end", sprint_id: "NNN", sprint_content: <full plan>)` — this:
   - Generates a condensed summary via Qwen3-Coder
   - Archives to Mimir with tags `["sprint", "project:yggdrasil"]`
   - Appends architecture delta to ARCHITECTURE.md
   - Deletes the sprint file
2. Verify `/sprints/` is empty (ready for next sprint).

### Retrieving sprint history

```
get_sprint_history_tool(project: "yggdrasil", limit: 5)
```

---

## Antigravity Integration

The Antigravity integration enables cross-IDE context sharing via the `context_bridge_tool`. This requires setting up an Antigravity server and configuring the `context_bridge_tool` to communicate with it.

To enable this functionality, ensure:
1. The Antigravity server is running
2. The `context_bridge_tool` is properly configured in the MCP server
3. Claude Code is configured to use the `yggdrasil` MCP server with the appropriate URL

The `context_bridge_tool` allows sharing context between different IDE windows by bridging them through the Antigravity infrastructure.