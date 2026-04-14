#!/usr/bin/env bash
# Sprint 065 B·P7 — regression smoke sweep across critical flows.
#
# After dual-serve soak + Odin config cutover, hit each flow and assert
# a non-error response. Used as the gate for B·P9 (Ollama shutdown).

set -u -o pipefail

ODIN_URL="${ODIN_URL:-http://10.0.65.8:8080}"
fail=0
pass=0

post_chat() {
    local flow_name="$1" message="$2"
    local body resp http_code
    body=$(jq -n --arg msg "$message" --arg flow "$flow_name" '{
        messages: [{"role":"user","content":$msg}],
        stream: false,
        metadata: {"flow_name": $flow}
    }')
    resp=$(curl -s -o /tmp/flow-smoke-resp.json -w "%{http_code}" \
        -H "Content-Type: application/json" \
        --max-time 90 \
        -d "$body" \
        "$ODIN_URL/v1/chat/completions" 2>/dev/null || echo "000")

    if [ "$resp" = "200" ]; then
        pass=$((pass+1))
        echo "  PASS $flow_name ($resp)"
    else
        fail=$((fail+1))
        echo "  FAIL $flow_name ($resp)"
        head -c 400 /tmp/flow-smoke-resp.json 2>/dev/null | sed 's/^/      /'
        echo
    fi
}

echo "Flow smoke sweep against $ODIN_URL"
echo

post_chat "coding_swarm"     "Write a minimal tokio TcpListener bind example."
post_chat "home_automation"  "turn on the living room light"
post_chat "perceive"         "describe what's in this frame (text only, no image)"
post_chat "research"         "latest on vLLM ROCm support for gfx1150?"
post_chat "dream_exploration" "Reflect on the last hour."

echo
echo "Result: $pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1
