#!/usr/bin/env bash
# SessionStart hook: recall last session context from Mimir on session start/resume.
# Runs AFTER ygg-hooks-init.sh (which creates /tmp/ygg-hooks/env).
# NEVER exits non-zero — hook failures must not block session start.

[ -f /tmp/ygg-hooks/env ] && . /tmp/ygg-hooks/env
MIMIR_URL="${MIMIR_URL:-http://localhost:9090}"

# Query Mimir for recent session context
response=$(curl --silent --max-time 2 \
    -H "Content-Type: application/json" \
    -d '{"text": "last session work implemented decisions notes remember future tasks sprint", "limit": 5, "include_text": true}' \
    "${MIMIR_URL}/api/v1/recall" 2>/dev/null) || true

# Count high-similarity results
count=$(echo "$response" | jq '[.events[]? | select(.similarity > 0.6)] | length' 2>/dev/null || echo 0)

if [ "${count:-0}" -gt 0 ]; then
    # Build summary for notification (first 2 causes, truncated)
    summary=$(echo "$response" | jq -r '
        [.events[]? | select(.similarity > 0.6) |
         (.cause // (.trigger.label // ""))
        ] | .[0:2] | join("\n")
    ' 2>/dev/null | head -c 200 || echo "")

    notify-send -t 4000 -i dialog-information "[session] context restored" "$count engrams recalled" 2>/dev/null || true

    # Build additionalContext for session injection
    context=$(echo "$response" | jq -r '
        [.events[]? | select(.similarity > 0.6) |
         "[" + (.similarity | tostring | .[0:4]) + "] " +
         (.cause // (.trigger.label // "")) + " -> " +
         (.effect // "")
        ] | join("\n")
    ' 2>/dev/null || echo "")

    if [ -n "$context" ]; then
        printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":%s}}\n' "$(echo "Prior session context (auto-recalled):\n${context}" | jq -Rs .)"
    fi
else
    notify-send -t 2000 -i dialog-information "[session] started" "No prior context found" 2>/dev/null || true
fi

exit 0
