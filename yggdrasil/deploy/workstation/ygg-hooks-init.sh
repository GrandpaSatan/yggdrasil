#!/usr/bin/env bash
# SessionStart hook: initialize the timing log directory for the current session.
# Called by Claude Code at the start of each session.
rm -rf /tmp/ygg-hooks
mkdir -p /tmp/ygg-hooks
echo "# Hook timing log - $(date -Iseconds)" > /tmp/ygg-hooks/recall-timing.log
exit 0
