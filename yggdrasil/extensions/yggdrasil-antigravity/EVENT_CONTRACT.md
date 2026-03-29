# Yggdrasil Antigravity IDE Integration — Event Contract

This directory scaffolds the Antigravity IDE integration for Yggdrasil.

## Event Hook Protocol

The Yggdrasil event hook system writes JSONL events to a shared file for
consumption by the memory sidecar pipeline. Antigravity should follow the
same protocol as the VS Code extension (`extensions/yggdrasil-local/`).

### Event File

Write events as newline-delimited JSON to:

```
/tmp/ygg-hooks/memory-events.jsonl
```

### Event Format

Each line is a JSON object with these required fields:

```json
{
  "timestamp": "2026-03-28T10:00:00Z",
  "event_type": "tool_use | file_edit | session_start | session_end",
  "source": "antigravity",
  "workspace_id": "yggdrasil:window-abc123",
  "data": {
    "tool_name": "edit",
    "file_path": "/path/to/file.rs",
    "content_snippet": "first 200 chars..."
  }
}
```

### Required Fields

| Field | Type | Description |
|-------|------|-------------|
| `timestamp` | ISO 8601 | Event time |
| `event_type` | string | Event category |
| `source` | string | Always `"antigravity"` |
| `workspace_id` | string | Unique per IDE window, matches `McpServerConfig.workspace_id` |
| `data` | object | Event-specific payload |

### Event Types

- `session_start` — IDE window opened or reconnected
- `session_end` — IDE window closed
- `tool_use` — An MCP tool was called (include `tool_name` in data)
- `file_edit` — A file was edited (include `file_path` and `content_snippet`)

### MCP Configuration

Antigravity connects to Yggdrasil via the remote MCP server:

```json
{
  "type": "http",
  "url": "http://<munin-ip>:9093/mcp",
  "headers": {
    "X-Client": "antigravity",
    "X-Workspace-Id": "<workspace_id>"
  }
}
```

### Agent Streaming

For real-time agent loop feedback, connect to Odin's SSE endpoint:

```
POST http://<munin-ip>:8080/v1/agent/stream
Content-Type: application/json

{
  "model": "qwen3-coder",
  "messages": [...],
  "tools": [...]
}
```

Returns Server-Sent Events with `event: step` (AgentStepEvent JSON) and
`event: result` (final ChatCompletionResponse JSON).

## Implementation Status

- [x] Event file path convention (shared with VS Code extension)
- [x] workspace_id field in McpServerConfig
- [x] workspace_id field in ContextBridgeParams
- [x] Agent streaming SSE endpoint on Odin
- [x] antigravity_url + ide_type in McpServerConfig
- [ ] Antigravity extension implementation (depends on Antigravity SDK)
