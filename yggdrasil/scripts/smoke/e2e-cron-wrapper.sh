#!/usr/bin/env bash
# Sprint 064 P6 — daily E2E cron wrapper.
#
# Runs scripts/smoke/e2e-live.sh, captures the tail of its output, and on
# non-zero exit POSTs a notification to Home Assistant's mobile companion app
# (mobile_app_pixel_10_pro_fold). Always exits 0 so the systemd unit doesn't
# enter a failed state — failure notification is the user-facing signal.
#
# Required env (read from /opt/yggdrasil/.env via the systemd unit):
#   HA_URL    e.g. http://10.0.65.14:8123
#   HA_TOKEN  long-lived access token
#
# Optional env:
#   YGG_E2E_SCRIPT    override the smoke script (default: ./scripts/smoke/e2e-live.sh)
#   YGG_E2E_LOG_TAIL  number of lines of output to include in the notification (default 40)

set -uo pipefail

SCRIPT="${YGG_E2E_SCRIPT:-/opt/yggdrasil/scripts/smoke/e2e-live.sh}"
TAIL_LINES="${YGG_E2E_LOG_TAIL:-40}"
HA_URL="${HA_URL:-}"
HA_TOKEN="${HA_TOKEN:-}"
NOTIFY_TARGET="${YGG_E2E_NOTIFY_TARGET:-mobile_app_pixel_10_pro_fold}"

LOG_DIR="/var/log/yggdrasil"
mkdir -p "$LOG_DIR" 2>/dev/null || LOG_DIR="/tmp"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
LOG_FILE="$LOG_DIR/e2e-${TS}.log"

echo "[$(date -Is)] starting e2e cron wrapper, script=$SCRIPT, log=$LOG_FILE"

# Sprint 064 P8 — increment odin_e2e_hits_total so Prometheus can see the
# timer firing. Best-effort: never block the smoke run on this.
ODIN_URL="${ODIN_URL:-http://127.0.0.1:8080}"
curl -sS -o /dev/null --max-time 5 -X POST "${ODIN_URL%/}/api/v1/e2e/hit" || \
    echo "WARN: e2e hit ping failed (non-fatal)"

if [[ ! -x "$SCRIPT" ]]; then
    echo "ERROR: smoke script $SCRIPT not found or not executable"
    exit 0
fi

# Run the smoke. Capture both stdout+stderr to the log.
START_EPOCH=$(date +%s)
"$SCRIPT" >"$LOG_FILE" 2>&1
EXIT=$?
END_EPOCH=$(date +%s)
DURATION=$((END_EPOCH - START_EPOCH))

echo "[$(date -Is)] e2e finished exit=$EXIT duration=${DURATION}s"

if [[ "$EXIT" -eq 0 ]]; then
    echo "e2e PASS — no notification sent"
    exit 0
fi

# ── Failure path: notify HA ────────────────────────────────────
if [[ -z "$HA_URL" || -z "$HA_TOKEN" ]]; then
    echo "ERROR: e2e failed but HA_URL or HA_TOKEN is unset; cannot notify"
    exit 0
fi

TAIL=$(tail -n "$TAIL_LINES" "$LOG_FILE" 2>/dev/null || echo "(log unavailable)")

# Compose JSON payload safely (jq if available, else sed-escape fallback).
if command -v jq >/dev/null 2>&1; then
    PAYLOAD=$(jq -nc \
        --arg title "Yggdrasil E2E FAILED" \
        --arg msg "exit=$EXIT duration=${DURATION}s\n---\n$TAIL" \
        --arg target "$NOTIFY_TARGET" \
        '{title: $title, message: $msg, data: {tag: "ygg-e2e", channel: "yggdrasil-alerts"}}')
else
    ESCAPED=$(printf '%s' "$TAIL" | sed 's/\\/\\\\/g; s/"/\\"/g; s/\t/\\t/g; s/$/\\n/' | tr -d '\n' | sed 's/\\n$//')
    PAYLOAD="{\"title\":\"Yggdrasil E2E FAILED\",\"message\":\"exit=$EXIT duration=${DURATION}s\\n---\\n${ESCAPED}\",\"data\":{\"tag\":\"ygg-e2e\",\"channel\":\"yggdrasil-alerts\"}}"
fi

NOTIFY_URL="${HA_URL%/}/api/services/notify/${NOTIFY_TARGET}"
HTTP_CODE=$(curl -sS -o /tmp/e2e-ha-resp.txt -w '%{http_code}' \
    -X POST "$NOTIFY_URL" \
    -H "Authorization: Bearer $HA_TOKEN" \
    -H 'Content-Type: application/json' \
    --max-time 10 \
    -d "$PAYLOAD" 2>&1) || HTTP_CODE="000"

echo "[$(date -Is)] HA notify POST $NOTIFY_URL → HTTP $HTTP_CODE"
if [[ "$HTTP_CODE" != "200" ]]; then
    echo "WARN: HA notify did not return 200; response: $(cat /tmp/e2e-ha-resp.txt 2>/dev/null | head -c 200)"
fi

exit 0
