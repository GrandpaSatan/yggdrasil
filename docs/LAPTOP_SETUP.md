# Yggdrasil — Laptop Claude Code Setup

How to connect Claude Code on any machine to the Yggdrasil memory system.

**Prerequisites:** The machine must be on VLAN 65 (10.0.65.0/24) or have a route to it.

---

## 1. Project-level MCP config

Create `.mcp.json` in the **root of your Yggdrasil workspace** (same directory as this repo):

```json
{
  "mcpServers": {
    "yggdrasil": {
      "type": "http",
      "url": "http://10.0.65.8:9093/mcp",
      "headers": {
        "X-Client": "claude-code"
      }
    }
  }
}
```

This connects to `ygg-mcp-remote` on Munin. It provides all memory, code search, HA, and generation tools (`query_memory_tool`, `store_memory_tool`, `search_code_tool`, `generate_tool`, `ha_*`, etc.).

> **Key:** `mcpServers` (not `servers`). Claude Code reads `.mcp.json` from the project root.

---

## 2. Memory hooks (optional but recommended)

The `ygg-memory.sh` script auto-recalls context on session start and ingests memories on code edits. Add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/yggdrasil/deploy/workstation/ygg-memory.sh init",
            "timeout": 5000
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/yggdrasil/deploy/workstation/ygg-memory.sh recall",
            "timeout": 1000
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/yggdrasil/deploy/workstation/ygg-memory.sh ingest",
            "timeout": 5000
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/yggdrasil/deploy/workstation/ygg-memory.sh sleep",
            "timeout": 15000
          }
        ]
      }
    ]
  }
}
```

Replace `/path/to/yggdrasil/` with the actual path to your Yggdrasil repo.

The script uses `MUNIN_IP` env var (defaults to `10.0.65.8`). Override if needed:
```bash
export MUNIN_IP=10.0.65.8
```

**Dependencies:** `curl`, `jq`

---

## 3. VS Code extension (optional)

If using Claude Code inside VS Code, install the Yggdrasil Local extension for status bar + dashboard:

```bash
cd yggdrasil/extensions/yggdrasil-local
npm install && npm run compile
npx @vscode/vsce package --no-dependencies
code --install-extension yggdrasil-local-*.vsix
```

This provides `sync_docs_tool` and `screenshot_tool` locally. All other tools come from the remote MCP server.

---

## 4. Verify

After restarting Claude Code:

1. Check MCP tools are loaded — you should see `mcp__yggdrasil__query_memory_tool` in the tool list
2. Test: ask Claude to run `query_memory_tool` with text `"yggdrasil system topology"`
3. You should get engrams back with node IPs, service ports, etc.

If tools don't appear:
- Verify `.mcp.json` is at the project root (not inside `.vscode/`)
- Verify Munin is reachable: `curl -s http://10.0.65.8:9093/mcp -H "Accept: application/json, text/event-stream" -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}},"id":1}'`
- Check the key is `mcpServers` (not `servers`)

---

## Service endpoints (for reference)

| Service | Host | Port | Purpose |
|---------|------|------|---------|
| Odin | 10.0.65.8 | 8080 | LLM orchestrator, chat API |
| Mimir | 10.0.65.8 | 9090 | Engram memory (PG + Qdrant) |
| MCP Remote | 10.0.65.8 | 9093 | MCP tool server (this is what .mcp.json connects to) |
| llama-server | 10.0.65.8 | 8081 | LFM2.5-1.2B on Munin iGPU |
| llama-server | 10.0.65.9 | 8081 | LFM2.5-1.2B on Hugin iGPU |
| llama-server | 10.0.65.9 | 8082 | LFM2.5-1.2B on Hugin eGPU |
| llama-embed | 10.0.65.8 | 8083 | all-MiniLM-L6-v2 embeddings |
| llama-embed | 10.0.65.9 | 8083 | all-MiniLM-L6-v2 embeddings |
| Huginn | 10.0.65.9 | 9092 | Code indexer |
| Muninn | 10.0.65.9 | 9091 | Code retrieval |
| Qdrant | 10.0.55.2 | 6334 | Vector DB (on Hades) |
| PostgreSQL | 10.0.55.2 | 5432 | Relational DB (on Hades) |

---

Last updated: 2026-04-06
