#!/usr/bin/env bash
# ygg-memory.sh — Unified memory sidecar for Claude Code (Sprint 045)
# Usage: ygg-memory.sh <init|recall|ingest|sleep>
# All modes exit 0 — never blocks Claude Code.
# Emits JSONL events to /tmp/ygg-hooks/memory-events.jsonl for the
# Yggdrasil Local VS Code extension.
set -o pipefail

MUNIN_IP="${MUNIN_IP:-10.0.65.9}"
MIMIR_URL="http://${MUNIN_IP}:9090"
OLLAMA_URL="http://${MUNIN_IP}:11434"
EVENTS_FILE="/tmp/ygg-hooks/memory-events.jsonl"

log() {
    printf "\033[0;36m[mem]\033[0m %s\n" "$*" >&2
    printf "%s [mem] %s\n" "$(date +%H:%M:%S)" "$*" >> /tmp/ygg-hooks/memory.log 2>/dev/null
}
hook_output() {
    local event="$1" ctx="$2"
    printf '{"hookSpecificOutput":{"hookEventName":"%s","additionalContext":%s}}\n' \
        "$event" "$(printf '%s' "$ctx" | jq -Rs .)"
}
emit_event() {
    local event="$1" data="$2"
    mkdir -p /tmp/ygg-hooks 2>/dev/null
    printf '{"ts":"%s","event":"%s","data":%s}\n' \
        "$(date -Iseconds)" "$event" "$data" >> "$EVENTS_FILE" 2>/dev/null
}

# ── check_and_update: versioned auto-update for yggdrasil-local extension ──
# Compares package.json version against installed extension version.
# Handles all cases: fresh install, migration from Rust binary, version bumps.
# Runs rebuild in background — never blocks the hook.
check_and_update() {
    local ext_dir mcp_json source_version installed_version
    ext_dir="$(cd "$(dirname "$0")/../.." && pwd)/extensions/yggdrasil-local"
    mcp_json="$HOME/Documents/Code/.mcp.json"

    # Bail if source doesn't exist or toolchain missing
    [ -f "$ext_dir/package.json" ] || return 0
    command -v node &>/dev/null || return 0
    command -v npm &>/dev/null || return 0

    # Read source version (single source of truth)
    source_version=$(jq -r '.version // "0.0.0"' "$ext_dir/package.json" 2>/dev/null)

    # Read installed version from VS Code extensions directory
    installed_version=$(ls -d "$HOME/.vscode/extensions/yggdrasil.yggdrasil-local-"* 2>/dev/null \
        | sed 's/.*yggdrasil-local-//' | sort -V | tail -1)
    installed_version="${installed_version:-not_installed}"

    # Fast path: already up to date
    if [ "$source_version" = "$installed_version" ]; then
        return 0
    fi

    log "update: $installed_version → $source_version"
    emit_event "update" "{\"from\":\"$installed_version\",\"to\":\"$source_version\",\"status\":\"started\"}"

    # Background rebuild + install
    (
        cd "$ext_dir" || exit 1

        # Install deps if needed
        [ -d "node_modules" ] || npm install --no-audit --no-fund 2>/dev/null

        # Compile TypeScript
        npm run compile 2>/dev/null || exit 1

        # Package and install VS Code extension
        if command -v code &>/dev/null && [ -f "out/mcp/server.js" ]; then
            npx @vscode/vsce package --no-dependencies 2>/dev/null
            local vsix=$(ls -t *.vsix 2>/dev/null | head -1)
            [ -n "$vsix" ] && code --install-extension "$vsix" --force 2>/dev/null
        fi

        # Fix .mcp.json if still pointing to old Rust binary
        if [ -f "$mcp_json" ] && grep -q "ygg-mcp-server" "$mcp_json" 2>/dev/null; then
            local server_js="$ext_dir/out/mcp/server.js"
            local config_path
            config_path=$(jq -r '.mcpServers["yggdrasil-local"].args[-1] // ""' "$mcp_json" 2>/dev/null)

            cp "$mcp_json" "$mcp_json.bak.$(date +%s)" 2>/dev/null
            jq --arg srv "$server_js" --arg cfg "$config_path" '
                .mcpServers["yggdrasil-local"] = {
                    "command": "node",
                    "args": (if $cfg != "" then [$srv, "--config", $cfg] else [$srv] end)
                }
            ' "$mcp_json" > "$mcp_json.tmp" && mv "$mcp_json.tmp" "$mcp_json"
            log "update: .mcp.json migrated to Node.js server"
        fi

        log "update: extension updated to $source_version — restart Claude Code to activate"
        emit_event "update" "{\"from\":\"$installed_version\",\"to\":\"$source_version\",\"status\":\"complete\"}"
    ) &>/dev/null &
}

# ── init: SessionStart — recall prior session context ────────────────
do_init() {
    # Truncate events file for new session
    mkdir -p /tmp/ygg-hooks 2>/dev/null
    : > "$EVENTS_FILE"
    : > /tmp/ygg-hooks/memory.log

    # Auto-update extension if source version != installed version
    check_and_update

    local response count context
    response=$(curl -sf --max-time 2 \
        -H "Content-Type: application/json" \
        -d '{"text":"last session work decisions sprint changes deployed gotchas","limit":5,"include_text":true}' \
        "${MIMIR_URL}/api/v1/recall" 2>/dev/null) || { log "init: mimir unreachable"; emit_event "error" '{"stage":"init","message":"mimir unreachable"}'; exit 0; }

    count=$(echo "$response" | jq '[.events[]? | select(.similarity > 0.6)] | length' 2>/dev/null || echo 0)

    if [ "${count:-0}" -gt 0 ]; then
        context=$(echo "$response" | jq -r '
            [.events[]? | select(.similarity > 0.6) |
             "[" + (.similarity | tostring | .[0:4]) + "] " +
             (.cause // "") + " → " + (.effect // "")
            ] | join("\n")
        ' 2>/dev/null)
        log "init: restored $count engrams"
        emit_event "init" "{\"count\":$count}"
        hook_output "SessionStart" "Prior session context (auto-recalled):\n${context}"
    else
        log "init: no prior context found"
        emit_event "init" '{"count":0}'
    fi
}

# ── recall: PreToolUse — surface relevant memories before edits ──────
do_recall() {
    local stdin_data file_path query response count context filename
    stdin_data=$(cat)
    file_path=$(echo "$stdin_data" | jq -r '.tool_input.file_path // .tool_input.path // "unknown"' 2>/dev/null)
    filename=$(basename "$file_path")
    query="${file_path} $(echo "$stdin_data" | jq -r '(.tool_input.new_string // .tool_input.content // "")' 2>/dev/null | head -c 200)"

    response=$(curl -sf --max-time 0.5 \
        -H "Content-Type: application/json" \
        -d "{\"text\":$(echo "$query" | jq -Rs .),\"limit\":3,\"include_text\":true}" \
        "${MIMIR_URL}/api/v1/recall" 2>/dev/null) || exit 0

    count=$(echo "$response" | jq '[.events[]? | select(.similarity > 0.7)] | length' 2>/dev/null || echo 0)

    if [ "${count:-0}" -gt 0 ]; then
        context=$(echo "$response" | jq -r '
            [.events[]? | select(.similarity > 0.7) |
             "[" + (.similarity | tostring | .[0:4]) + "] " +
             (.cause // "") + " → " + (.effect // "")
            ] | join("\n")
        ' 2>/dev/null)
        log "recall: $count engrams for $filename"
        emit_event "recall" "{\"count\":$count,\"file\":$(echo "$filename" | jq -Rs .)}"
        hook_output "PreToolUse" "Relevant memories:\n${context}"
    fi
}

# ── ingest: PostToolUse — LLM judges if change is worth remembering ──
do_ingest() {
    local stdin_data file_path content filename response
    stdin_data=$(cat)
    file_path=$(echo "$stdin_data" | jq -r '.tool_input.file_path // .tool_input.path // "unknown"' 2>/dev/null)
    content=$(echo "$stdin_data" | jq -r '(.tool_input.new_string // .tool_input.content // "")' 2>/dev/null | head -c 300)
    filename=$(basename "$file_path")

    # Skip trivial changes
    [ ${#content} -lt 50 ] && exit 0

    # Call Mimir smart-ingest (which calls LLM internally)
    response=$(curl -sf --max-time 3 \
        -H "Content-Type: application/json" \
        -d "{\"content\":$(echo "$content" | jq -Rs .),\"file_path\":$(echo "$file_path" | jq -Rs .),\"workstation\":\"$(hostname)\",\"source\":\"edit\"}" \
        "${MIMIR_URL}/api/v1/smart-ingest" 2>/dev/null) || exit 0

    stored=$(echo "$response" | jq -r '.stored // false' 2>/dev/null)
    if [ "$stored" = "true" ]; then
        cause=$(echo "$response" | jq -r '.cause // "change"' 2>/dev/null)
        log "ingest: stored $filename — $cause"
        emit_event "ingest" "{\"stored\":true,\"file\":$(echo "$filename" | jq -Rs .),\"cause\":$(echo "$cause" | jq -Rs .)}"
    fi
}

# ── sleep: Stop — consolidate session memories ───────────────────────
do_sleep() {
    local response
    # Call Mimir consolidation endpoint
    response=$(curl -sf --max-time 10 \
        -H "Content-Type: application/json" \
        -d "{\"workstation\":\"$(hostname)\",\"hours\":12}" \
        "${MIMIR_URL}/api/v1/consolidate" 2>/dev/null) || { log "sleep: mimir unreachable"; exit 0; }

    local summary=$(echo "$response" | jq -r '.summary // "no consolidation needed"' 2>/dev/null)
    log "sleep: $summary"
    emit_event "sleep" "{\"summary\":$(echo "$summary" | jq -Rs .)}"
}

# ── dispatch ─────────────────────────────────────────────────────────
case "${1:-}" in
    init)   do_init ;;
    recall) do_recall ;;
    ingest) do_ingest ;;
    sleep)  do_sleep ;;
    *)      echo "Usage: ygg-memory.sh <init|recall|ingest|sleep>" >&2 ;;
esac
exit 0
