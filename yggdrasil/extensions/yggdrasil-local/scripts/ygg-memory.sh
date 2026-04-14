#!/usr/bin/env bash
# ygg-memory.sh — Cognitive Sidecar for Claude Code (Sprint 058)
# Bundled inside the yggdrasil-local VS Code extension.
# Deployed to ~/.yggdrasil/ygg-memory.sh by the extension on activation.
#
# Usage: ygg-memory.sh <init|sidecar|post|sleep>
# All modes exit 0 — never blocks Claude Code.
# Emits JSONL events to /tmp/ygg-hooks/memory-events.jsonl
set -o pipefail

MUNIN_IP="${MUNIN_IP:-10.0.65.8}"
HUGIN_IP="${HUGIN_IP:-10.0.65.9}"
MIMIR_URL="http://${MUNIN_IP}:9090"
HUGIN_OLLAMA="http://${HUGIN_IP}:11434"
RWKV_MODEL="mollysama/rwkv-7-g1e:2.9b"
EVENTS_FILE="/tmp/ygg-hooks/memory-events.jsonl"

# Per-project session file (hash of $PWD)
PROJECT_HASH=$(echo "$PWD" | md5sum | cut -c1-8)
SESSION_FILE="/tmp/ygg-hooks/session-${PROJECT_HASH}.jsonl"

log() {
    printf "\033[0;36m[mem]\033[0m %s\n" "$*" >&2
    printf "%s [mem] %s\n" "$(date +%H:%M:%S)" "$*" >> /tmp/ygg-hooks/memory.log 2>/dev/null
}
hook_output() {
    local ctx="$1"
    printf '{"additionalContext": %s}\n' "$(printf '%s' "$ctx" | jq -Rs .)"
}
emit_event() {
    local event="$1" data="$2"
    mkdir -p /tmp/ygg-hooks 2>/dev/null
    printf '{"ts":"%s","event":"%s","data":%s}\n' \
        "$(date -Iseconds)" "$event" "$data" >> "$EVENTS_FILE" 2>/dev/null
}
session_append() {
    local tool="$1" summary="$2"
    printf '{"ts":"%s","tool":"%s","summary":%s}\n' \
        "$(date +%H:%M:%S)" "$tool" "$(echo "$summary" | jq -Rs .)" >> "$SESSION_FILE" 2>/dev/null
}

# ── init: SessionStart — prepare per-project session ───────────────────
do_init() {
    mkdir -p /tmp/ygg-hooks 2>/dev/null
    : > "$EVENTS_FILE"
    : > /tmp/ygg-hooks/memory.log

    # Create per-project session file (truncate for new session)
    : > "$SESSION_FILE"

    log "init: session started (project: ${PROJECT_HASH}, sidecar: RWKV-7)"
    emit_event "init" "{\"count\":0,\"project_hash\":\"${PROJECT_HASH}\"}"
}

# ── sidecar: PreToolUse — RWKV-7 classifies + Mimir injects context ───
do_sidecar() {
    local stdin_data tool_name tool_summary session_context rwkv_response
    local category queries store_worthy context_parts final_context
    stdin_data=$(cat)

    tool_name=$(echo "$stdin_data" | jq -r '.tool_name // "unknown"' 2>/dev/null)

    # Build a short summary of the current tool use
    case "$tool_name" in
        Edit|Write)
            tool_summary=$(echo "$stdin_data" | jq -r '
                (.tool_input.file_path // .tool_input.path // "unknown") + " — " +
                (.tool_input.new_string // .tool_input.content // "" | .[0:150])
            ' 2>/dev/null)
            ;;
        Read)
            tool_summary=$(echo "$stdin_data" | jq -r '
                "reading " + (.tool_input.file_path // "unknown")
            ' 2>/dev/null)
            ;;
        Bash)
            tool_summary=$(echo "$stdin_data" | jq -r '
                (.tool_input.command // "" | .[0:200])
            ' 2>/dev/null)
            ;;
        Grep)
            tool_summary=$(echo "$stdin_data" | jq -r '
                "grep " + (.tool_input.pattern // "") + " in " + (.tool_input.path // ".")
            ' 2>/dev/null)
            ;;
        Agent)
            tool_summary=$(echo "$stdin_data" | jq -r '
                "agent: " + (.tool_input.description // .tool_input.prompt // "" | .[0:150])
            ' 2>/dev/null)
            ;;
        *)
            tool_summary="$tool_name"
            ;;
    esac

    # Append to session log
    session_append "$tool_name" "$tool_summary"

    # Read last ~50 events from session log for context
    session_context=$(tail -50 "$SESSION_FILE" 2>/dev/null | jq -rs '
        [.[] | .tool + ": " + .summary] | join("\n")
    ' 2>/dev/null || echo "")

    # Call RWKV-7 for classification
    local rwkv_prompt="Session history:
${session_context}

Current tool: ${tool_name}
Current action: ${tool_summary}

Pick exactly ONE category from: infra, coding, deployment, debugging, training, homelab.
Write 1-2 short search queries to find relevant past knowledge for this task.
Decide if this action is important enough to save as a memory (true/false).
Reply with ONLY a JSON object like: {\"category\": \"infra\", \"queries\": [\"odin service status munin\"], \"store_worthy\": false}"

    # Sprint 061: timeouts are GENEROUS by design — never tune them to p95.
    # RWKV-7 p95 is ~3.4s but cold-starts, prompt-size variance, Ollama queue
    # waits, and weight swaps can blow any tight budget. 25s gives real headroom.
    # Progress feedback lives in the chat UI (thinking fold), not in tight
    # timeouts. See feedback engram "stop tuning timeouts to measured p95".
    rwkv_response=$(curl -sf --max-time 25 \
        -H "Content-Type: application/json" \
        -d "$(jq -n \
            --arg model "$RWKV_MODEL" \
            --arg prompt "$rwkv_prompt" \
            '{model: $model, messages: [
                {role: "system", content: "You are a session monitor for a software engineer'\''s AI coding assistant. Classify the current task and suggest memory queries. Respond ONLY with valid JSON, no explanation."},
                {role: "user", content: $prompt}
            ], stream: false, options: {temperature: 0.1, num_predict: 128}, think: false}')" \
        "${HUGIN_OLLAMA}/api/chat" 2>/dev/null) || {
        # RWKV unreachable — fall back to simple Mimir recall
        log "sidecar: RWKV-7 unreachable, falling back to recall"
        local fallback_query="$tool_summary"
        local fallback_resp=$(curl -sf --max-time 0.5 \
            -H "Content-Type: application/json" \
            -d "{\"text\":$(echo "$fallback_query" | jq -Rs .),\"limit\":3,\"include_text\":true}" \
            "${MIMIR_URL}/api/v1/recall" 2>/dev/null) || exit 0

        local fallback_count=$(echo "$fallback_resp" | jq '[.events[]? | select(.similarity > 0.7)] | length' 2>/dev/null || echo 0)
        if [ "${fallback_count:-0}" -gt 0 ]; then
            local fallback_ctx=$(echo "$fallback_resp" | jq -r '
                [.events[]? | select(.similarity > 0.7) |
                 "[" + (.similarity | tostring | .[0:4]) + "] " +
                 (.cause // "") + " → " + (.effect // "")
                ] | join("\n")
            ' 2>/dev/null)
            log "sidecar: fallback recalled $fallback_count engrams"
            emit_event "recall" "{\"count\":$fallback_count,\"mode\":\"fallback\",\"query\":$(printf '%s' "$fallback_query" | head -c 120 | jq -Rs .)}"
            hook_output "Relevant memories:\n${fallback_ctx}"
        fi
        exit 0
    }

    # Parse RWKV-7 response — strip think tags, extract JSON
    local raw_text=$(echo "$rwkv_response" | jq -r '.message.content // ""' 2>/dev/null)
    # Strip <think>...</think> tags if present
    raw_text=$(echo "$raw_text" | sed 's/<think>.*<\/think>//g' 2>/dev/null)
    # Extract JSON object
    local json_block=$(echo "$raw_text" | grep -oP '\{[^{}]*\}' | head -1 2>/dev/null)

    if [ -z "$json_block" ]; then
        log "sidecar: RWKV-7 returned no JSON, skipping"
        exit 0
    fi

    category=$(echo "$json_block" | jq -r '.category // "coding"' 2>/dev/null)
    store_worthy=$(echo "$json_block" | jq -r '.store_worthy // false' 2>/dev/null)

    # Set store_worthy flag for PostToolUse
    if [ "$store_worthy" = "true" ]; then
        touch /tmp/ygg-hooks/store_worthy 2>/dev/null
    fi

    # Extract queries from RWKV-7 response
    local query_count=$(echo "$json_block" | jq '.queries | length' 2>/dev/null || echo 0)

    if [ "${query_count:-0}" -eq 0 ]; then
        log "sidecar: category=$category, no queries"
        emit_event "sidecar" "{\"category\":\"$category\",\"queries\":0}"
        exit 0
    fi

    # Call Mimir recall for each query (parallel)
    context_parts=""
    local pids=()
    local tmpdir=$(mktemp -d /tmp/ygg-sidecar-XXXX 2>/dev/null)

    for i in $(seq 0 $((query_count - 1))); do
        local query=$(echo "$json_block" | jq -r ".queries[$i]" 2>/dev/null)
        [ -z "$query" ] || [ "$query" = "null" ] && continue

        (
            local resp=$(curl -sf --max-time 0.5 \
                -H "Content-Type: application/json" \
                -d "{\"text\":$(echo "$query" | jq -Rs .),\"limit\":3,\"include_text\":true}" \
                "${MIMIR_URL}/api/v1/recall" 2>/dev/null) || exit 0

            echo "$resp" | jq -r '
                [.events[]? | select(.similarity > 0.7) |
                 "[" + (.similarity | tostring | .[0:4]) + "] " +
                 (.cause // "") + " → " + (.effect // "")
                ] | join("\n")
            ' 2>/dev/null > "$tmpdir/q$i.txt"
        ) &
        pids+=($!)
    done

    # Wait for all parallel recalls
    for pid in "${pids[@]}"; do
        wait "$pid" 2>/dev/null
    done

    # Assemble context from all query results
    final_context=""
    for f in "$tmpdir"/q*.txt; do
        [ -f "$f" ] || continue
        local chunk=$(cat "$f" 2>/dev/null)
        [ -n "$chunk" ] && final_context="${final_context}${chunk}\n"
    done
    rm -rf "$tmpdir" 2>/dev/null

    if [ -n "$final_context" ]; then
        local engram_count=$(echo -e "$final_context" | grep -c '\[0\.' 2>/dev/null || echo 0)
        local all_queries=$(echo "$json_block" | jq -r '.queries | join(", ")' 2>/dev/null)
        log "sidecar: category=$category, injected $engram_count engrams"
        emit_event "sidecar" "{\"category\":\"$category\",\"engrams\":$engram_count,\"store_worthy\":$store_worthy}"
        emit_event "recall" "{\"count\":$engram_count,\"query\":$(printf '%s' "$all_queries" | head -c 120 | jq -Rs .)}"
        hook_output "Relevant memories:\n${final_context}"
    else
        log "sidecar: category=$category, no relevant memories"
        emit_event "sidecar" "{\"category\":\"$category\",\"engrams\":0}"
    fi
}

# ── post: PostToolUse — log session + conditional smart-ingest ─────────
# Sprint 065 A·P2: emit post_entered event FIRST so we can distinguish
# "hook never fired" (no post_entered) from "hook fired but gate said no"
# (post_entered + post_skipped). Eliminates the silent-failure ambiguity
# that caused the 2026-04-07 "auto-ingest not triggering" investigation.
do_post() {
    local stdin_data tool_name tool_summary
    stdin_data=$(cat)

    tool_name=$(echo "$stdin_data" | jq -r '.tool_name // "unknown"' 2>/dev/null)
    emit_event "post_entered" "{\"tool\":$(printf '%s' "$tool_name" | jq -Rs .)}"

    # Build summary based on tool type
    case "$tool_name" in
        Edit|Write)
            local file_path=$(echo "$stdin_data" | jq -r '.tool_input.file_path // .tool_input.path // "unknown"' 2>/dev/null)
            local filename=$(basename "$file_path")
            tool_summary="${file_path}: $(echo "$stdin_data" | jq -r '(.tool_input.new_string // .tool_input.content // "") | .[0:200]' 2>/dev/null)"
            ;;
        Bash)
            local cmd=$(echo "$stdin_data" | jq -r '.tool_input.command // "" | .[0:150]' 2>/dev/null)
            local stderr_text=$(echo "$stdin_data" | jq -r '.tool_response.stderr // "" | .[0:200]' 2>/dev/null)
            local stdout_text=$(echo "$stdin_data" | jq -r '.tool_response.stdout // "" | .[0:200]' 2>/dev/null)
            tool_summary="$ ${cmd} → ${stdout_text}"
            ;;
        *)
            tool_summary="$tool_name completed"
            ;;
    esac

    # Always append to session log
    session_append "$tool_name" "$tool_summary"

    # ── Error recall for Bash failures ──
    if [ "$tool_name" = "Bash" ] && [ ${#stderr_text} -gt 20 ]; then
        local error_resp=$(curl -sf --max-time 0.5 \
            -H "Content-Type: application/json" \
            -d "{\"text\":$(echo "$stderr_text" | tail -c 500 | jq -Rs .),\"limit\":3,\"include_text\":true}" \
            "${MIMIR_URL}/api/v1/recall" 2>/dev/null) || true

        local error_count=$(echo "$error_resp" | jq '[.events[]? | select(.similarity > 0.7)] | length' 2>/dev/null || echo 0)
        if [ "${error_count:-0}" -gt 0 ]; then
            local error_ctx=$(echo "$error_resp" | jq -r '
                [.events[]? | select(.similarity > 0.7) |
                 "[" + (.similarity | tostring | .[0:4]) + "] " +
                 (.cause // "") + " → " + (.effect // "")
                ] | join("\n")
            ' 2>/dev/null)
            log "post: $error_count engrams for error"
            emit_event "error_recall" "{\"count\":$error_count}"
            hook_output "Past encounters with similar errors:\n${error_ctx}"
        fi
    fi

    # ── Conditional smart-ingest (store_worthy flag from sidecar) ──
    # Sprint 065 A·P2: verify the store_worthy marker is fresh. A stale marker
    # (>60s old) means the sidecar fired in a prior tool-use and never cleared
    # it — don't ingest on its behalf, just clean it up and log.
    if [ -f /tmp/ygg-hooks/store_worthy ]; then
        local marker_age=$(( $(date +%s) - $(stat -c %Y /tmp/ygg-hooks/store_worthy 2>/dev/null || echo 0) ))
        if [ "$marker_age" -gt 60 ]; then
            rm -f /tmp/ygg-hooks/store_worthy 2>/dev/null
            emit_event "post_skipped" "{\"reason\":\"stale_marker\",\"age_secs\":$marker_age,\"tool\":$(printf '%s' "$tool_name" | jq -Rs .)}"
            log "post: store_worthy marker was stale (${marker_age}s), discarded"
            exit 0
        fi

        rm -f /tmp/ygg-hooks/store_worthy 2>/dev/null

        # Only for Edit/Write/Bash with substantial content
        local content=""
        case "$tool_name" in
            Edit|Write)
                local file_path=$(echo "$stdin_data" | jq -r '.tool_input.file_path // .tool_input.path // "unknown"' 2>/dev/null)
                # Try git diff first
                if [ -f "$file_path" ] && command -v git &>/dev/null; then
                    content=$(cd "$(dirname "$file_path")" 2>/dev/null && git diff -U3 -- "$file_path" 2>/dev/null | head -c 2000)
                    [ -z "$content" ] && content=$(cd "$(dirname "$file_path")" 2>/dev/null && git diff --cached -U3 -- "$file_path" 2>/dev/null | head -c 2000)
                fi
                [ -z "$content" ] && content=$(echo "$stdin_data" | jq -r '(.tool_input.new_string // .tool_input.content // "") | .[0:2000]' 2>/dev/null)

                local branch=$(git branch --show-current 2>/dev/null || echo "unknown")
                content="File: ${file_path} | Branch: ${branch}\n${content}"
                ;;
            Bash)
                content="$ ${cmd}\nstdout: ${stdout_text}\nstderr: ${stderr_text}"
                ;;
        esac

        # Sprint 065 A·P2: replace silent `exit 0` with explicit event so we can
        # distinguish "content too short to ingest" from "hook never fired".
        if [ ${#content} -lt 50 ]; then
            emit_event "post_skipped" "{\"reason\":\"content_too_short\",\"len\":${#content},\"tool\":$(printf '%s' "$tool_name" | jq -Rs .)}"
            exit 0
        fi

        local response=$(curl -sf --max-time 5 \
            -H "Content-Type: application/json" \
            -d "{\"content\":$(echo "$content" | jq -Rs .),\"file_path\":$(echo "${file_path:-bash}" | jq -Rs .),\"workstation\":\"$(hostname)\",\"source\":\"sidecar\"}" \
            "${MIMIR_URL}/api/v1/smart-ingest" 2>/dev/null)

        # Sprint 065 A·P2: surface Mimir unreachable explicitly instead of exit 0.
        if [ -z "$response" ]; then
            emit_event "post_skipped" "{\"reason\":\"mimir_unreachable\",\"tool\":$(printf '%s' "$tool_name" | jq -Rs .)}"
            log "post: mimir smart-ingest unreachable"
            exit 0
        fi

        local stored=$(echo "$response" | jq -r '.stored // false' 2>/dev/null)
        if [ "$stored" = "true" ]; then
            local cause=$(echo "$response" | jq -r '.cause // "change"' 2>/dev/null)
            log "post: stored — $cause"
            emit_event "ingest" "{\"stored\":true,\"file\":$(echo "${filename:-bash}" | jq -Rs .),\"cause\":$(echo "$cause" | jq -Rs .)}"
        else
            local skip_reason=$(echo "$response" | jq -r '.skipped_reason // "gate_rejected"' 2>/dev/null)
            emit_event "post_skipped" "{\"reason\":$(printf '%s' "$skip_reason" | jq -Rs .),\"tool\":$(printf '%s' "$tool_name" | jq -Rs .)}"
        fi
    else
        emit_event "post_skipped" "{\"reason\":\"no_store_worthy_marker\",\"tool\":$(printf '%s' "$tool_name" | jq -Rs .)}"
    fi
}

# ── sleep: Stop — consolidate session memories ───────────────────────
do_sleep() {
    local response
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
    init)              do_init ;;
    sidecar|recall)    do_sidecar ;;
    post|ingest)       do_post ;;
    sleep)             do_sleep ;;
    *)
        echo "Usage: ygg-memory.sh <init|sidecar|post|sleep>" >&2
        echo "Managed by the yggdrasil-local VS Code extension." >&2
        ;;
esac
exit 0
