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
| `POST` | `/api/v1/timeline` | Proxy to Mimir: query engram timeline. Body: `{after?, before?, tags?, limit?}` |
| `POST` | `/api/v1/sprints/list` | Proxy to Mimir: list sprint engrams. Body: `{project?, limit?}` |
| `POST` | `/api/v1/context` | Proxy to Mimir: store context blob. Body: `{content, label?}` |
| `GET`  | `/api/v1/context` | Proxy to Mimir: list stored context blobs |
| `GET`  | `/api/v1/context/{handle}` | Proxy to Mimir: retrieve context blob by handle |
| `POST` | `/api/v1/tasks/push` | Proxy to Mimir: push task. Body: `{title, description?, priority?, tags?}` |
| `POST` | `/api/v1/tasks/pop` | Proxy to Mimir: pop next task. Body: `{agent}` |
| `POST` | `/api/v1/tasks/complete` | Proxy to Mimir: complete task. Body: `{task_id, result?}` |
| `POST` | `/api/v1/tasks/cancel` | Proxy to Mimir: cancel task. Body: `{task_id}` |
| `POST` | `/api/v1/tasks/list` | Proxy to Mimir: list tasks. Body: `{status?, agent?, limit?}` |
| `POST` | `/api/v1/graph/link` | Proxy to Mimir: link engrams. Body: `{source_id, target_id, relation, weight?}` |
| `POST` | `/api/v1/graph/unlink` | Proxy to Mimir: unlink engrams. Body: `{source_id, target_id, relation?}` |
| `POST` | `/api/v1/graph/neighbors` | Proxy to Mimir: get neighbors. Body: `{engram_id, direction, relation?, depth?}` |
| `POST` | `/api/v1/graph/traverse` | Proxy to Mimir: traverse graph. Body: `{start_id, max_depth?, relation?, limit?}` |
| `POST` | `/api/v1/symbols` | Proxy to Muninn: symbol lookup. Body: `{name?, chunk_type?, language?, file_path?, limit?}` |
| `POST` | `/api/v1/references` | Proxy to Muninn: find references. Body: `{symbol, language?, limit?}` |
| `POST` | `/api/v1/notify` | Send HA notification. Body: `{title, message, target?}` |
| `POST` | `/api/v1/webhook` | Home Assistant webhook receiver |
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
| `GET`  | `/api/v1/stats` | Engram statistics (count, tier breakdown, recall capacity) |
| `POST` | `/api/v1/promote` | Promote engram tier. Body: `{id, tier}` |
| `GET`  | `/api/v1/core` | Get core (highest tier) engrams |
| `POST` | `/api/v1/timeline` | Query engram timeline. Body: `{after?, before?, tags?, limit?}` |
| `POST` | `/api/v1/sprints/list` | List sprint engrams. Body: `{project?, limit?}` |
| `POST` | `/api/v1/context` | Store context blob. Body: `{content, label?}` |
| `GET`  | `/api/v1/context` | List stored context blobs |
| `GET`  | `/api/v1/context/{handle}` | Retrieve context blob by handle |
| `POST` | `/api/v1/tasks/push` | Push task. Body: `{title, description?, priority?, tags?}` |
| `POST` | `/api/v1/tasks/pop` | Pop next task. Body: `{agent}` |
| `POST` | `/api/v1/tasks/complete` | Complete task. Body: `{task_id, result?}` |
| `POST` | `/api/v1/tasks/cancel` | Cancel task. Body: `{task_id}` |
| `POST` | `/api/v1/tasks/list` | List tasks. Body: `{status?, agent?, limit?}` |
| `POST` | `/api/v1/graph/link` | Link engrams. Body: `{source_id, target_id, relation, weight?}` |
| `POST` | `/api/v1/graph/unlink` | Unlink engrams. Body: `{source_id, target_id, relation?}` |
| `POST` | `/api/v1/graph/neighbors` | Get neighbors. Body: `{engram_id, direction, relation?, depth?}` |
| `POST` | `/api/v1/graph/traverse` | Traverse graph. Body: `{start_id, max_depth?, relation?, limit?}` |
| `GET`  | `/health` | Health check |
| `GET`  | `/metrics` | Prometheus metrics |

**Note:** Call Mimir via Odin proxy (`/api/v1/query`, `/api/v1/store`) rather than directly. Odin's proxy routes are compatible with the Fergus `EngramClient`.

---

### Muninn — Code Retrieval (Hugin :9091)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/search` | Hybrid code search (BM25 + Qdrant + RRF). Body: `{query, languages?: [string], limit?}` |
| `POST` | `/api/v1/symbols` | Symbol lookup. Body: `{name?, chunk_type?, language?, file_path?, limit?}` |
| `POST` | `/api/v1/references` | Find references. Body: `{symbol, language?, limit?}` |
| `GET`  | `/api/v1/stats` | Index statistics (chunk count, file count, language breakdown) |
| `GET`  | `/health` | Health check |
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
- `/home/$USER/repos/Yggdrasil`
- `/home/$USER/repos/Fergus_Agent`
- `/mnt/workstation/docs/HardwareSetup` (SSHFS from workstation `<workstation-ip>`)

---

### MCP Servers — Claude Code Integration

The MCP layer is split into two servers (Sprint 027):

1. **`yggdrasil`** (remote, StreamableHTTP) — 12 network tools + 3 resources. Always-on, shared across IDE windows.
2. **`yggdrasil-local`** (local, stdio) — Filesystem tools only. One process per IDE window.

#### Remote Server — `yggdrasil` (Munin :9093)

**Binary:** `ygg-mcp-remote` at `/opt/yggdrasil/bin/ygg-mcp-remote`
**Config:** `/etc/yggdrasil/mcp-remote/config.json` (on Munin)
**Systemd:** `yggdrasil-mcp-remote.service`
**Claude Code config:** `type: "http"`, `url: "http://<munin-ip>:9093/mcp"`

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

### Start services on Munin

```bash
# Start all Yggdrasil services
sudo systemctl start yggdrasil-odin yggdrasil-mimir

# Start Ollama IPEX container (if not running)
sudo systemctl start yggdrasil-ollama-ipex

# Check status
sudo systemctl status yggdrasil-odin yggdrasil-mimir
```

### Start services on Hugin

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
cd ~/yggdrasil
cargo run --release --bin odin -- --config configs/odin/node.yaml

# Run MCP server locally
cargo run --release --bin ygg-mcp-server -- --config configs/mcp-server/config.yaml
```

---

## View Logs

```bash
# Munin
ssh your-user@munin "sudo journalctl -u yggdrasil-odin -f"
ssh your-user@munin "sudo journalctl -u yggdrasil-mimir -f"

# Hugin
ssh your-user@hugin "sudo journalctl -u yggdrasil-huginn -f"
ssh your-user@hugin "sudo journalctl -u yggdrasil-muninn -f"
```

---

## Deploy Commands

### Full install (first time on a node)

```bash
cd ~/yggdrasil
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
rsync target/release/odin your-user@<munin-ip>:/home/your-user/odin.new
ssh your-user@<munin-ip> \
  "sudo mv /home/your-user/odin.new /opt/yggdrasil/bin/odin && \
   sudo systemctl restart yggdrasil-odin"
```

### Rollback

```bash
deploy/rollback.sh munin odin  # restores odin.prev binary and restarts
```

### Update MCP server binary (workstation)

```bash
cd ~/yggdrasil
cargo build --release --bin ygg-mcp-server
rsync target/release/ygg-mcp-server ~/.local/bin/ygg-mcp-server
# Restart Claude Code to reload the MCP server
```

---

## Database Admin

### PostgreSQL (Munin, via Docker)

```bash
# Connect to yggdrasil DB
ssh your-user@munin \
  "docker exec -it ygg-postgres psql -U your-user -d yggdrasil"

# Run migration manually
ssh your-user@munin \
  "cd /opt/yggdrasil && ./bin/odin --migrate-only"  # if supported

# Check engram count
ssh your-user@munin \
  "docker exec ygg-postgres psql -U your-user -d yggdrasil -c 'SELECT count(*) FROM engrams;'"
```

### Qdrant (Hades :6333)

```bash
# List collections
curl http://<hades-ip>:6333/collections

# Collection details
curl http://<hades-ip>:6333/collections/engrams_sdr
curl http://<hades-ip>:6333/collections/code_chunks
```

---

## Backup

```bash
# Manual backup (runs on Munin cron at 03:00 daily)
ssh your-user@munin "sudo /opt/yggdrasil/deploy/backup-hades.sh"

# Backup script location (on Munin)
# /opt/yggdrasil/deploy/backup-hades.sh
# - pg_dump yggdrasil → TrueNAS Hades via rsync
# - Qdrant snapshot → Hades

# Check last backup
ssh your-user@munin "ls -la /mnt/hades/backups/ 2>/dev/null || echo 'check Hades mount'"
```

---

## Monitoring

- **Grafana:** http://`<nightjar-ip>`:3000 (dashboard uid: `ygg-observability`)
- **Prometheus:** http://`<nightjar-ip>`:9099 (scrapes `/metrics` from all 4 services)

### Quick health check (all services)

```bash
# Odin
curl -sf http://<munin-ip>:8080/health | python3 -m json.tool

# Mimir
curl -sf http://<munin-ip>:9090/health

# Muninn
curl -sf http://<hugin-ip>:9091/health

# Huginn
curl -sf http://<hugin-ip>:9092/health
```

---

## SSH Shortcuts

Configure `~/.ssh/config` for convenient access (key-based auth required):

```
Host munin
    HostName <munin-ip>
    User your-user

Host hugin
    HostName <hugin-ip>
    User your-user

Host hades
    HostName <hades-ip>
    User your-user

Host nightjar
    HostName <nightjar-ip>
    User your-user
```

Then use:
```bash
ssh munin    # Odin + Mimir
ssh hugin    # Huginn + Muninn + Ollama
ssh hades    # Qdrant + TrueNAS
ssh nightjar # Grafana + Prometheus
```

---

## SSHFS (Hugin → Workstation)

Hugin mounts the workstation's home documents directory at `/mnt/workstation/docs` so Huginn can index local code.

```bash
# Check mount status on Hugin
ssh your-user@hugin "systemctl status mnt-workstation-docs.mount"

# Remount if dropped
ssh your-user@hugin "sudo systemctl restart mnt-workstation-docs.mount"

# Workstation sshd must be running
sudo systemctl status ssh  # on workstation
```

### Claude Code MCP Config (`~/.claude.json`)

```json
{
  "mcpServers": {
    "yggdrasil": {
      "type": "http",
      "url": "http://<munin-ip>:9093/mcp"
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
cd ~/yggdrasil
./deploy/workstation/ClaudeClient_Install
# → Project dir: ~/yggdrasil (auto-detected, no parent .git)
# → project=yggdrasil  workspace=~/yggdrasil
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
