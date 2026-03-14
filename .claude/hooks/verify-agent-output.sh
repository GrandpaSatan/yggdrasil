#!/bin/bash
# Post-agent verification hook
# Runs cargo check + clippy after any executor agent modifies Rust code.
# Feeds compilation errors back into the conversation as context.

set -euo pipefail

cd "$CLAUDE_PROJECT_DIR/yggdrasil" 2>/dev/null || exit 0

# Check if any Rust files were recently modified (last 3 minutes)
RECENT_RS=$(find crates -name '*.rs' -newer /tmp/ygg-hooks/agent_start_marker 2>/dev/null | head -5)

if [ -z "$RECENT_RS" ]; then
  # No Rust files changed — skip verification
  exit 0
fi

# Run cargo check
CHECK_OUTPUT=$(cargo check --workspace 2>&1) || {
  ERRORS=$(echo "$CHECK_OUTPUT" | grep -E "^error" | head -10)
  echo "IMPORTANT: Agent-generated code has compilation errors. Fix these before proceeding:" >&2
  echo "$ERRORS" >&2
  echo "" >&2
  echo "Changed files:" >&2
  echo "$RECENT_RS" >&2
  exit 2
}

# Run cargo clippy (warnings only, don't block)
CLIPPY_OUTPUT=$(cargo clippy --workspace 2>&1) || true
CLIPPY_WARNINGS=$(echo "$CLIPPY_OUTPUT" | grep -c "^warning\[" 2>/dev/null || echo "0")

if [ "$CLIPPY_WARNINGS" -gt 0 ]; then
  WARNINGS=$(echo "$CLIPPY_OUTPUT" | grep -E "^warning\[" | head -5)
  jq -n --arg warnings "$WARNINGS" --arg count "$CLIPPY_WARNINGS" '{
    "hookSpecificOutput": {
      "hookEventName": "SubagentStop",
      "additionalContext": ("Agent code compiled successfully but has " + $count + " clippy warnings. Consider fixing:\n" + $warnings)
    }
  }'
  exit 0
fi

jq -n '{
  "hookSpecificOutput": {
    "hookEventName": "SubagentStop",
    "additionalContext": "Agent code verified: cargo check + clippy clean."
  }
}'
exit 0
