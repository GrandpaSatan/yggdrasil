#!/usr/bin/env bash
# PreToolUse hook: recall relevant engrams before Edit/Write operations (v2).
# Called by Claude Code with CLAUDE_TOOL_INPUT env var set to the tool JSON.
# NEVER exits non-zero — hook failures must not block tool execution.

[ -f /tmp/ygg-hooks/env ] && . /tmp/ygg-hooks/env
MIMIR_URL="${MIMIR_URL:-http://localhost:9090}"

# Extract file path and content snippet from tool input
file_path=$(echo "${CLAUDE_TOOL_INPUT:-{}}" | jq -r '.file_path // .path // "unknown"' 2>/dev/null || echo "unknown")
content_snippet=$(echo "${CLAUDE_TOOL_INPUT:-{}}" | jq -r '(.new_string // .content // "")' 2>/dev/null | head -c 200 || echo "")
filename=$(basename "$file_path")

# Build query text
query_text="${file_path} ${content_snippet}"

# Record start time for timing log
start_ns=$(date +%s%N 2>/dev/null || echo 0)

# Call Mimir recall endpoint with 500ms hard timeout
response=$(curl --silent --max-time 0.5 \
    -H "Content-Type: application/json" \
    -d "{\"text\": $(echo "$query_text" | jq -Rs .), \"limit\": 3, \"include_text\": true}" \
    "${MIMIR_URL}/api/v1/recall" 2>/dev/null) || true

end_ns=$(date +%s%N 2>/dev/null || echo 0)
elapsed_ms=$(( (end_ns - start_ns) / 1000000 )) 2>/dev/null || elapsed_ms=0

# Count high-similarity events
count=$(echo "$response" | jq '[.events[]? | select(.similarity > 0.7)] | length' 2>/dev/null || echo 0)

if [ "${count:-0}" -gt 0 ]; then
    printf "\033[0;36m[mem]\033[0m <- recalled %s engrams for %s (%sms)\n" "$count" "$filename" "$elapsed_ms" >&2
    notify-send -t 2000 -i dialog-information "[mem] recalled" "$count engrams for $filename" 2>/dev/null || true

    # Build additionalContext from engram cause/effect text
    context=$(echo "$response" | jq -r '
        [.events[]? | select(.similarity > 0.7) |
         "[" + (.similarity | tostring | .[0:4]) + "] " +
         (.cause // (.trigger.label // "")) + " -> " +
         (.effect // "")
        ] | join("\n")
    ' 2>/dev/null || echo "")

    if [ -n "$context" ]; then
        printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":%s}}\n' "$(echo "Relevant memories (auto-recalled):\n${context}" | jq -Rs .)"
    fi
else
    printf "\033[0;90m[mem]\033[0m <- no recall for %s (%sms)\n" "$filename" "$elapsed_ms" >&2
fi

echo "$(date -Iseconds) recall ${filename} ${elapsed_ms}ms" >> /tmp/ygg-hooks/recall-timing.log 2>/dev/null || true

exit 0
