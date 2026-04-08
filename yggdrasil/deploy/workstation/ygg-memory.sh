#!/usr/bin/env bash
# ygg-memory.sh — Unified memory sidecar for Claude Code (Sprint 045)
# Usage: ygg-memory.sh <init|recall|ingest|sleep>
# All modes exit 0 — never blocks Claude Code.
# Emits JSONL events to /tmp/ygg-hooks/memory-events.jsonl for the
# Yggdrasil Local VS Code extension.
set -o pipefail

MUNIN_IP="${MUNIN_IP:-10.0.65.8}"
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
    local ext_dir source_version installed_version
    ext_dir="$(cd "$(dirname "$0")/../.." && pwd)/extensions/yggdrasil-local"

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
        if command -v code &>/dev/null && [ -f "out/extension.js" ]; then
            npx @vscode/vsce package --no-dependencies 2>/dev/null
            local vsix=$(ls -t *.vsix 2>/dev/null | head -1)
            [ -n "$vsix" ] && code --install-extension "$vsix" --force 2>/dev/null
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

    # Sprint 055: No generic recall at session start.
    # Context-aware recall is handled by Claude.md Phase 1 protocol
    # (query_memory_tool with the actual task topic on first prompt).
    log "init: session started (recall deferred to first prompt)"
    emit_event "init" '{"count":0}'
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

# ── ingest: PostToolUse — Saga LLM judges if change is worth remembering ──
do_ingest() {
    local stdin_data file_path content filename response diff_content header branch
    stdin_data=$(cat)
    file_path=$(echo "$stdin_data" | jq -r '.tool_input.file_path // .tool_input.path // "unknown"' 2>/dev/null)
    filename=$(basename "$file_path")

    # Sprint 055: Capture git diff hunks instead of fixed char truncation.
    # This gives the LLM the actual change with 3 lines of context.
    diff_content=""
    if [ -f "$file_path" ] && command -v git &>/dev/null; then
        # Try unstaged diff first, then staged
        diff_content=$(git diff -U3 -- "$file_path" 2>/dev/null | head -c 2000)
        if [ -z "$diff_content" ]; then
            diff_content=$(git diff --cached -U3 -- "$file_path" 2>/dev/null | head -c 2000)
        fi
    fi

    # Fallback to raw content if git diff unavailable (new file, not in repo, etc.)
    if [ -z "$diff_content" ]; then
        diff_content=$(echo "$stdin_data" | jq -r '(.tool_input.new_string // .tool_input.content // "")' 2>/dev/null | head -c 2000)
    fi

    # Skip trivial changes
    [ ${#diff_content} -lt 50 ] && exit 0

    # Sprint 055: Prepend file metadata so the LLM knows context
    branch=$(git branch --show-current 2>/dev/null || echo "unknown")
    header="File: ${file_path} | Branch: ${branch}"
    content="${header}
${diff_content}"

    # Call Mimir smart-ingest (Saga model judges STORE vs SKIP)
    response=$(curl -sf --max-time 5 \
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

# ── error_recall: PostToolUse(Bash) — surface past errors on failure ──
do_error_recall() {
    local stdin_data exit_code output response count context
    stdin_data=$(cat)

    # Extract exit code from hook payload
    exit_code=$(echo "$stdin_data" | jq -r '.tool_result.exit_code // .tool_output.exit_code // "0"' 2>/dev/null)
    [ "$exit_code" = "0" ] || [ -z "$exit_code" ] && exit 0

    # Grab the error output (stderr or stdout, last 500 chars)
    output=$(echo "$stdin_data" | jq -r '(.tool_result.stderr // .tool_result.stdout // .tool_output.content // "")' 2>/dev/null | tail -c 500)
    [ ${#output} -lt 20 ] && exit 0

    # Query memory with the error text
    response=$(curl -sf --max-time 1 \
        -H "Content-Type: application/json" \
        -d "{\"text\":$(echo "$output" | jq -Rs .),\"limit\":3,\"include_text\":true}" \
        "${MIMIR_URL}/api/v1/recall" 2>/dev/null) || exit 0

    count=$(echo "$response" | jq '[.events[]? | select(.similarity > 0.7)] | length' 2>/dev/null || echo 0)

    if [ "${count:-0}" -gt 0 ]; then
        context=$(echo "$response" | jq -r '
            [.events[]? | select(.similarity > 0.7) |
             "[" + (.similarity | tostring | .[0:4]) + "] " +
             (.cause // "") + " → " + (.effect // "")
            ] | join("\n")
        ' 2>/dev/null)
        log "error_recall: $count engrams for failed command"
        emit_event "error_recall" "{\"count\":$count}"
        hook_output "PostToolUse" "Past encounters with similar errors:\n${context}"
    fi
}

# ── dispatch ─────────────────────────────────────────────────────────
case "${1:-}" in
    init)         do_init ;;
    recall)       do_recall ;;
    ingest)       do_ingest ;;
    error_recall) do_error_recall ;;
    sleep)        do_sleep ;;
    *)            echo "Usage: ygg-memory.sh <init|recall|ingest|sleep|error_recall>" >&2 ;;
esac
exit 0
