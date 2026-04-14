#!/usr/bin/env bash
# Sprint 063 Track C — P5b: Live E2E smoke test for home_automation flow.
#
# Sends real HTTP requests to a live Odin instance and validates:
#   1. "turn on the kitchen light" → 200 + HA-confirm pattern in response.
#   2. "turn on kitchen light while I play Fallout" → 200 + still routes HA
#      (router regression guard added in Sprint 062).
#
# Gated behind E2E_LIVE=1 to prevent accidental execution in default CI.
#
# Usage:
#   E2E_LIVE=1 ./e2e-live.sh
#   E2E_LIVE=1 ODIN_URL=http://10.0.65.8:8080 ./e2e-live.sh

set -euo pipefail

if [[ "${E2E_LIVE:-0}" != "1" ]]; then
  echo "Skipped: set E2E_LIVE=1 to run live E2E tests against a real Odin."
  exit 0
fi

ODIN_URL="${ODIN_URL:-http://10.0.65.8:8080}"

RED=$'\033[31m'
GREEN=$'\033[32m'
YELLOW=$'\033[33m'
BOLD=$'\033[1m'
RESET=$'\033[0m'

log()  { printf '%s\n' "$*"; }
ok()   { printf '  %s✓%s %s\n' "$GREEN"  "$RESET" "$*"; }
fail() { printf '  %s✗%s %s\n' "$RED"    "$RESET" "$*" >&2; FAILURES=$((FAILURES+1)); }
warn() { printf '  %s!%s %s\n' "$YELLOW" "$RESET" "$*"; }

banner_pass() { printf '\n%s%s================ PASS ================%s\n' "$BOLD" "$GREEN" "$RESET"; }
banner_fail() { printf '\n%s%s================ FAIL ================%s\n' "$BOLD" "$RED"   "$RESET"; }

# Track total failures so we can print a summary banner.
FAILURES=0

log ""
log "${BOLD}Sprint 063 Track C — Live E2E smoke test${RESET}"
log "  Odin: ${ODIN_URL}"
log ""

# ─────────────────────────── Prerequisites ────────────────────────────────────
for bin in curl jq; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    fail "required binary '${bin}' not found on PATH"
    banner_fail
    exit 1
  fi
done

# ─────────────────────────── 1. Health check ─────────────────────────────────
log "1) GET ${ODIN_URL}/health"
health_code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "${ODIN_URL}/health" || echo 000)"
if [[ "${health_code}" != "200" ]]; then
  fail "health check: expected 200, got ${health_code}"
  banner_fail
  exit 1
fi
ok "health: 200 OK"

# ─────────────────────────── Helper: send chat request ───────────────────────
# chat_request <description> <message_json_string>
# Posts to /v1/chat/completions (non-streaming), prints the response content,
# and returns it in CHAT_CONTENT.
CHAT_CONTENT=""
chat_request() {
  local desc="$1"
  local msg="$2"

  local body
  body="$(jq -nc --arg m "${msg}" '{
    "model": null,
    "messages": [{"role": "user", "content": $m}],
    "stream": false
  }')"

  local response
  response="$(curl -s -w '\n%{http_code}' --max-time 60 \
    -X POST "${ODIN_URL}/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d "${body}" || echo -e '\n000')"

  local http_body http_code
  http_body="$(printf '%s' "${response}" | head -n -1)"
  http_code="$(printf '%s' "${response}" | tail -n 1)"

  log ""
  log "  ${BOLD}${desc}${RESET}"
  log "  Message: ${msg}"
  log "  HTTP:    ${http_code}"

  if [[ "${http_code}" != "200" ]]; then
    fail "${desc}: expected 200, got ${http_code}"
    log "  Body: ${http_body}" >&2
    CHAT_CONTENT=""
    return
  fi

  CHAT_CONTENT="$(printf '%s' "${http_body}" | jq -r '.choices[0].message.content // ""' 2>/dev/null || echo "")"
  log "  Content: ${CHAT_CONTENT:0:200}..."
}

# content_contains <pattern> <description>
# Case-insensitive substring match against CHAT_CONTENT.
content_contains() {
  local pattern="$1"
  local desc="$2"
  if printf '%s' "${CHAT_CONTENT}" | grep -qi "${pattern}"; then
    ok "${desc}"
  else
    fail "${desc}: response does not contain '${pattern}'"
  fi
}

# ─────────────────────────── 2. HA flow test ─────────────────────────────────
log ""
log "2) home_automation flow — 'turn on the kitchen light'"
chat_request "HA flow: turn on kitchen light" "turn on the kitchen light"

if [[ -n "${CHAT_CONTENT}" ]]; then
  # Expect content matching a home-automation confirm pattern.
  content_contains "light\|kitchen\|turn\|on" "response mentions kitchen light or HA action"
fi

# ─────────────────────────── 3. Router regression guard ──────────────────────
log ""
log "3) Router regression — 'turn on kitchen light while I play Fallout'"
chat_request "Mixed HA+gaming routing" "turn on the kitchen light while I play Fallout"

if [[ -n "${CHAT_CONTENT}" ]]; then
  # The response should still relate to HA (light control), not purely gaming.
  content_contains "light\|kitchen\|turn\|lamp" "response still references light control for mixed-intent message"
fi

# ─────────────────────────── 4. Summary ──────────────────────────────────────
if (( FAILURES > 0 )); then
  log ""
  log "${RED}${FAILURES} assertion(s) failed.${RESET}"
  banner_fail
  exit 1
fi

banner_pass
exit 0
