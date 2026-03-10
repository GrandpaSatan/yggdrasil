#!/usr/bin/env bash
# Poll a health endpoint until it returns HTTP 200 or timeout expires.
# Usage: wait-for-health.sh <url> <timeout_seconds>
# Example: wait-for-health.sh http://localhost:9090/health 30
#
# Used by systemd ExecStartPre in yggdrasil-odin.service to gate Odin startup
# on Mimir being healthy.
set -euo pipefail

URL=$1
TIMEOUT=${2:-30}
ELAPSED=0

while [ "$ELAPSED" -lt "$TIMEOUT" ]; do
    if curl -sf "$URL" > /dev/null 2>&1; then
        exit 0
    fi
    sleep 1
    ELAPSED=$((ELAPSED + 1))
done

echo "Health check failed: $URL did not respond within ${TIMEOUT}s" >&2
exit 1
