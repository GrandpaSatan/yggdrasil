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
| `GET`  | `/v1/voice` | Voice WebSocket endpoint. Upgrade to WebSocket; stream PCM s16le audio (16 kHz mono) as binary frames. Server sends JSON control frames and TTS audio back. See [Voice WebSocket Pipeline](ARCHITECTURE.md#data-flow-voice-websocket-pipeline-odin) for full protocol. |
| `GET`  | `/voice` | Embedded browser voice UI (HTML page) |
| `POST` | `/api/v1/embed` | Proxy to Mimir: embed text. Body: `{text}` |
| `GET`  | `/api/v1/engrams/{id}` | Proxy to Mimir: get engram by ID |
| `POST` | `/api/v1/gaming` | Cloud gaming VM orchestration. Body: `{action, vm_name?, gpu?}` |
| `POST` | `/api/v1/web_search` | Web search via Brave API. Body: `{query, count?}` |
| `POST` | `/api/v1/voice/alert` | Inject voice alert from Sentinel. Body: `{message, priority?}` |
| `GET`  | `/api/v1/voice/enroll` | List wake word enrollments |
| `POST` | `/api/v1/voice/enroll/{user_id}` | Enroll wake word for user |
| `DELETE`| `/api/v1/voice/enroll/{user_id}` | Remove wake word enrollment |
| `GET`  | `/health` | Health check (always HTTP 200, status in body) |
| `GET`  | `/metrics` | Prometheus text metrics |

**Session continuity:** Pass `session_id` (string UUID) to maintain multi-turn context. Odin stores history server-side. Pass `project_id` (e.g. `"yggdrasil"`) to enable cross-window context injection from previous sessions.

**Streaming:** `stream: true` (default) returns SSE chunks. `stream: false` returns JSON response. Response includes `x-session-id` header with the resolved session ID.

---

### Mimir — Engram Memory (Munin :9090)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/store` | Store new engram. Body: `{cause, effect, tags?: [string]}` |
| `POST` | `/api/v1/recall` | SDR dual-system recall. Body: `{text, limit?, include_text?: bool}` → returns `{events: [EngramEvent]}`. When `include_text: true`, each event includes `cause` and `effect` fields. |
| `POST` | `/api/v1/auto-ingest` | Autonomous SDR-classified ingest. Body: `{content, source, event_type, workstation, file_path?, project?}` → returns `{stored, engram_id?, matched_template?, similarity?, skipped_reason?}` |
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
| `POST` | `/api/v1/vault` | Encrypted secret vault. Body: `{action: "set"|"get"|"list"|"delete", key_name, scope?, value?, tags?}` |
| `GET`  | `/api/v1/engrams/{id}` | Get engram by ID |
| `POST` | `/api/v1/embed` | Embed text and return vector. Body: `{text}` |
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

The MCP layer is split into two servers (Sprint 027, updated Sprint 050):

1. **`yggdrasil`** (remote, StreamableHTTP) — 32 network tools + 3 resources. Always-on, shared across IDE windows.
2. **`yggdrasil-local`** (local, VS Code extension + stdio) — 2 local tools + memory dashboard. Auto-updates via version check on session start.

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
| `config_version_tool` | Check/bump version info for server, client, config |
| `config_sync_tool` | Cross-workstation config file sync (push/pull/status) |
| `gaming_tool` | Cloud gaming VM management on Thor (Proxmox) |
| `vault_tool` | Encrypted secret vault (get/set/list/delete) |
| `deploy_tool` | Build and deploy Yggdrasil service binaries |
| `network_topology_tool` | Query Yggdrasil mesh network topology |
| `delegate_tool` | Unified LLM delegation with full context and agentic tool use |
| `web_search_tool` | Search the web via Brave Search API |

| Resource URI | Description |
|--------------|-------------|
| `yggdrasil://models` | Available models markdown table |
| `yggdrasil://memory/stats` | Mimir engram statistics |
| `yggdrasil://context/session` | Prefetched active sprint context |

#### Local Server — `yggdrasil-local` (VS Code Extension)

**Extension:** `yggdrasil.yggdrasil-local` (installed via auto-update or `install-memory.sh`)
**MCP Server:** `extensions/yggdrasil-local/out/mcp/server.js` (Node.js, stdio transport)
**Config:** `~/.config/yggdrasil/local-mcp.yaml`
**Claude Code config:** `command: "node"`, args point to `server.js` + `--config` flag

| Tool | Description |
|------|-------------|
| `sync_docs_tool` | Sprint lifecycle: setup workspace, update USAGE.md on start, archive on end |
| `screenshot_tool` | Headless Chromium page capture via Puppeteer |

**Visual Features (VS Code extension):**
| Feature | Description |
|---------|-------------|
| Status Bar | `$(database) Ygg: N recalled · N stored` — click to open dashboard |
| Output Channel | "Yggdrasil Memory" — timestamped event log |
| Notifications | Configurable toasts on ingest/error (settings: `yggdrasil.notifications.*`) |
| Dashboard | Webview panel (Ctrl+Shift+M) — session stats, event timeline |

**Auto-Update:** On each session start, `ygg-memory.sh` compares `package.json` version against installed extension version. On mismatch: background rebuild + reinstall. Bump `package.json` version, push, all workstations update automatically.

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

# Local MCP server is now the VS Code extension (yggdrasil-local)
# Install/update: ./deploy/workstation/install-memory.sh
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

### Update local MCP server (workstation)

```bash
cd ~/yggdrasil
# Option 1: Run the installer (builds extension + installs hooks)
./deploy/workstation/install-memory.sh

# Option 2: Auto-update — just bump extensions/yggdrasil-local/package.json version.
# All workstations update automatically on next Claude Code session start.
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
      "command": "node",
      "args": ["/path/to/extensions/yggdrasil-local/out/mcp/server.js", "--config", "~/.config/yggdrasil/local-mcp.yaml"]
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

## Autonomous Memory Pipeline (Sprint 044)

Memory recall and ingestion are handled automatically via Claude Code hook scripts. No explicit `query_memory_tool` or `store_memory_tool` calls are needed for routine file-context operations. Those tools remain available for topology queries, sprint history, and deliberate knowledge storage.

### Environment Variable

| Variable | Default | Description |
|----------|---------|-------------|
| `MIMIR_URL` | `http://localhost:9090` | Base URL for Mimir. Set this if Mimir runs on a different host or port. Used by all hook scripts. |

### `POST /api/v1/auto-ingest`

SDR-classified autonomous ingest. Content is embedded, fingerprinted into a 256-bit SDR, and matched against 6 pre-seeded insight templates. If the best template match exceeds the configured threshold (default 0.3 Hamming similarity), a cause/effect engram is auto-generated and stored.

**Request:**

```json
{
  "content": "Edited crates/mimir/src/handlers.rs: replaced old error path with proper Result",
  "source": "Edit",
  "event_type": "post_tool",
  "workstation": "my-workstation",
  "file_path": "crates/mimir/src/handlers.rs",
  "project": "yggdrasil"
}
```

**Response (stored):**

```json
{
  "stored": true,
  "engram_id": "a1b2c3d4-...",
  "matched_template": "bug_fix",
  "similarity": 0.42,
  "skipped_reason": null
}
```

**Response (skipped):**

```json
{
  "stored": false,
  "engram_id": null,
  "matched_template": null,
  "similarity": 0.15,
  "skipped_reason": "below_threshold"
}
```

Other `skipped_reason` values: `"cooldown"`, `"duplicate"`, `"empty_content"`.

**curl example:**

```bash
curl -s -X POST http://localhost:9090/api/v1/auto-ingest \
  -H "Content-Type: application/json" \
  -d '{
    "content": "Fixed DashMap clone bug: use Arc<DashMap> for shared Axum state",
    "source": "Edit",
    "event_type": "post_tool",
    "workstation": "my-workstation"
  }'
```

### `POST /api/v1/recall` — `include_text` Parameter

The existing recall endpoint accepts an optional `include_text: true` parameter. When set, each returned event includes the full `cause` and `effect` text fields (normally omitted per Sprint 015 zero-injection policy).

```bash
curl -s -X POST http://localhost:9090/api/v1/recall \
  -H "Content-Type: application/json" \
  -d '{"text": "DashMap shared state Axum", "limit": 3, "include_text": true}'
```

### Hook Scripts

Hook scripts live at `deploy/workstation/` in the repository. They are wired into Claude Code via `~/.claude/settings.json`.

| Script | Hook Type | Trigger | Behavior |
|--------|-----------|---------|----------|
| `ygg-hooks-init.sh` | SessionStart | Every new Claude Code session | Creates `/tmp/ygg-hooks/` directory and initializes timing log |
| `ygg-memory-recall.sh` | PreToolUse | `Edit\|Write` | Queries Mimir `/api/v1/recall` with file context, returns `additionalContext` with relevant memories. Hard timeout 500ms. |
| `ygg-memory-ingest.sh` | PostToolUse | `Edit\|Write\|Bash` | Sends tool I/O to Mimir `/api/v1/auto-ingest` in the background (fire-and-forget). Never blocks. |

**Visual indicators** (printed to stderr, visible in terminal):
- `[mem] <- recalled N engrams` -- recall hook found relevant memories
- `[mem] -> stored: {template}` -- ingest hook matched a template
- `[mem] -> skipped` -- ingest hook content was below threshold or deduplicated
- No output on failure (graceful degradation, silent no-op)

### Hook Installation

The hook scripts are configured in `~/.claude/settings.json` under the `hooks` key. The `deploy/workstation/ClaudeClient_Install` script handles this automatically. For manual setup:

1. Ensure scripts are executable:
   ```bash
   chmod +x deploy/workstation/ygg-hooks-init.sh
   chmod +x deploy/workstation/ygg-memory-recall.sh
   chmod +x deploy/workstation/ygg-memory-ingest.sh
   ```

2. Add to `~/.claude/settings.json` (abbreviated -- see sprint-044 Phase 3 for full config):
   ```json
   {
     "hooks": {
       "SessionStart": [{"hooks": [{"type": "command", "command": "/absolute/path/to/ygg-hooks-init.sh"}]}],
       "PreToolUse": [{"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "/absolute/path/to/ygg-memory-recall.sh"}]}],
       "PostToolUse": [
         {"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "/absolute/path/to/ygg-memory-ingest.sh"}]},
         {"matcher": "Bash", "hooks": [{"type": "command", "command": "/absolute/path/to/ygg-memory-ingest.sh"}]}
       ]
     }
   }
   ```

3. Optionally set `MIMIR_URL` if Mimir is not at the default address:
   ```bash
   export MIMIR_URL="http://localhost:9090"
   ```

**Prerequisites:** `curl` and `jq` must be installed on the workstation.

### Seed Insight Templates

The `seed-insight-templates.sh` script populates Mimir with the 6 insight template engrams used for SDR template matching. Run once after deploying the auto-ingest endpoint, or re-run to update templates.

```bash
# Seed all 6 templates (idempotent)
MIMIR_URL=http://localhost:9090 deploy/workstation/seed-insight-templates.sh

# Templates seeded: bug_fix, architecture_decision, sprint_lifecycle,
# user_feedback, deployment_change, gotcha
```

Mimir loads template SDRs at startup from engrams tagged `insight_template`. After seeding, restart Mimir or wait for the next restart for templates to take effect.

---

## Antigravity Integration

The Antigravity integration enables cross-IDE context sharing via the `context_bridge_tool`. This requires setting up an Antigravity server and configuring the `context_bridge_tool` to communicate with it.

To enable this functionality, ensure:
1. The Antigravity server is running
2. The `context_bridge_tool` is properly configured in the MCP server
3. Claude Code is configured to use the `yggdrasil` MCP server with the appropriate URL

The `context_bridge_tool` allows sharing context between different IDE windows by bridging them through the Antigravity infrastructure.
