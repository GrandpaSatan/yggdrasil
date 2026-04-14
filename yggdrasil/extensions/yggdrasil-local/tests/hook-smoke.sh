#!/usr/bin/env bash
# hook-smoke.sh — Sprint 065 A·P2 — smoke test for ygg-memory.sh PostToolUse.
#
# Simulates a PostToolUse hook invocation via stdin JSON and asserts that the
# expected events land in /tmp/ygg-hooks/memory-events.jsonl. Does NOT require
# Mimir, RWKV, or any external service — it only asserts the local event log
# transitions correctly.
#
# Usage: bash extensions/yggdrasil-local/tests/hook-smoke.sh
# Exit:   0 on PASS, 1 on FAIL.
set -eu -o pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOK="$SCRIPT_DIR/../scripts/ygg-memory.sh"
EVENTS_FILE="/tmp/ygg-hooks/memory-events.jsonl"
STORE_WORTHY_MARKER="/tmp/ygg-hooks/store_worthy"

if [ ! -x "$HOOK" ] && [ ! -f "$HOOK" ]; then
    echo "FAIL: hook script not found at $HOOK" >&2
    exit 1
fi

fail_count=0
pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1"; fail_count=$((fail_count+1)); }

# Reset event log for clean test state.
reset_events() {
    mkdir -p /tmp/ygg-hooks
    : > "$EVENTS_FILE"
    rm -f "$STORE_WORTHY_MARKER"
}

# Assert last matching event has field/value.
assert_event_present() {
    local event_name="$1" description="$2"
    if grep -q "\"event\":\"$event_name\"" "$EVENTS_FILE" 2>/dev/null; then
        pass "$description (event=$event_name present)"
    else
        fail "$description (event=$event_name NOT in $EVENTS_FILE)"
    fi
}

# Test 1: post with no store_worthy marker → post_entered + post_skipped(no_store_worthy_marker)
echo "Test 1: post without store_worthy marker"
reset_events
echo '{"tool_name":"Edit","tool_input":{"file_path":"/tmp/x.rs","new_string":"short"}}' | bash "$HOOK" post >/dev/null 2>&1 || true
assert_event_present "post_entered" "entered event emitted"
if grep -q '"reason":"no_store_worthy_marker"' "$EVENTS_FILE"; then
    pass "skipped with reason=no_store_worthy_marker"
else
    fail "expected post_skipped with no_store_worthy_marker reason"
fi

# Test 2: post with fresh store_worthy marker + short content → post_skipped(content_too_short)
echo "Test 2: post with fresh marker + short content"
reset_events
touch "$STORE_WORTHY_MARKER"
echo '{"tool_name":"Edit","tool_input":{"file_path":"/tmp/x.rs","new_string":"x"}}' | bash "$HOOK" post >/dev/null 2>&1 || true
assert_event_present "post_entered" "entered event emitted"
if grep -q '"reason":"content_too_short"' "$EVENTS_FILE"; then
    pass "skipped with reason=content_too_short"
else
    fail "expected post_skipped with content_too_short reason"
fi
# Marker should have been consumed.
if [ ! -f "$STORE_WORTHY_MARKER" ]; then
    pass "store_worthy marker consumed on fresh-marker path"
else
    fail "store_worthy marker NOT consumed"
fi

# Test 3: stale store_worthy marker (>60s) → post_skipped(stale_marker)
echo "Test 3: stale store_worthy marker"
reset_events
touch "$STORE_WORTHY_MARKER"
# Backdate the marker by 120 seconds.
touch -d "120 seconds ago" "$STORE_WORTHY_MARKER"
echo '{"tool_name":"Edit","tool_input":{"file_path":"/tmp/x.rs","new_string":"doesnt matter"}}' | bash "$HOOK" post >/dev/null 2>&1 || true
assert_event_present "post_entered" "entered event emitted"
if grep -q '"reason":"stale_marker"' "$EVENTS_FILE"; then
    pass "skipped with reason=stale_marker"
else
    fail "expected post_skipped with stale_marker reason"
fi
if [ ! -f "$STORE_WORTHY_MARKER" ]; then
    pass "stale marker cleaned up"
else
    fail "stale marker NOT cleaned up"
fi

# Test 4: tool_name surfaces in every skipped event.
echo "Test 4: tool_name surfaces in skipped events"
reset_events
echo '{"tool_name":"Bash","tool_input":{"command":"ls"}}' | bash "$HOOK" post >/dev/null 2>&1 || true
if grep -q '"tool":"Bash"' "$EVENTS_FILE"; then
    pass "tool=Bash present in events"
else
    fail "tool=Bash NOT in events"
fi

echo ""
if [ "$fail_count" -gt 0 ]; then
    echo "HOOK SMOKE: $fail_count FAILURES"
    exit 1
fi
echo "HOOK SMOKE: all passed"
