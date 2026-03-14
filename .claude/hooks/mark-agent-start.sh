#!/bin/bash
# Pre-agent marker — creates a timestamp file so the post-agent hook
# knows which files were modified during the agent's execution.
mkdir -p /tmp/ygg-hooks
touch /tmp/ygg-hooks/agent_start_marker
exit 0
