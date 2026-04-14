#!/usr/bin/env bash
# Sprint 062 — Flows smoke test.
#
# Validates that:
#   1. Odin /health responds 200.
#   2. Each new flow (home_automation + 4 dream_* flows) can be PUT via the
#      hot-swap endpoint and Odin responds {"ok":true}.
#   3. GET /api/flows lists all four dream_* flows + home_automation.
#
# Odin expects a SINGLE FlowConfig (not the {"flows": [...]} wrapper) on the
# PUT endpoint, so this script extracts individual flows from the bundled
# config-template files before uploading.
#
# Usage:
#   ./flows-smoke.sh                      # uses default http://10.0.65.8:8080 (Munin/Odin)
#   ODIN_URL=http://odin:8080 ./flows-smoke.sh

set -euo pipefail

ODIN_URL="${ODIN_URL:-http://10.0.65.8:8080}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEMPLATES="${REPO_ROOT}/deploy/config-templates"

RED=$'\033[31m'
GREEN=$'\033[32m'
YELLOW=$'\033[33m'
BOLD=$'\033[1m'
RESET=$'\033[0m'

log()  { printf '%s\n' "$*"; }
ok()   { printf '  %s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
fail() { printf '  %s✗%s %s\n' "$RED"   "$RESET" "$*" >&2; }
warn() { printf '  %s!%s %s\n' "$YELLOW" "$RESET" "$*"; }

banner_pass() {
  printf '\n%s%s================ PASS ================%s\n' "$BOLD" "$GREEN" "$RESET"
}
banner_fail() {
  printf '\n%s%s================ FAIL ================%s\n' "$BOLD" "$RED" "$RESET"
}

# Check prerequisites.
for bin in curl jq; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    fail "required binary '$bin' not found on PATH"
    banner_fail
    exit 1
  fi
done

log ""
log "${BOLD}Sprint 062 flows smoke test${RESET}"
log "  Odin:      ${ODIN_URL}"
log "  Templates: ${TEMPLATES}"
log ""

# ─────────────────────────── 1. /health ───────────────────────────
log "1) GET ${ODIN_URL}/health"
health_code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "${ODIN_URL}/health" || echo 000)"
if [[ "${health_code}" != "200" ]]; then
  fail "expected 200, got ${health_code}"
  banner_fail
  exit 1
fi
ok "200 OK"

# ───────────────────── 2. PUT each new flow ───────────────────────
declare -a FILES=(
  "home-automation-flow.json"
  "dream-flows.json"
)

EXPECTED_FLOWS=(
  "home_automation"
  "dream_consolidation"
  "dream_exploration"
  "dream_speculation"
  "dream_self_improvement"
)

put_failures=0
log ""
log "2) PUT each flow to ${ODIN_URL}/api/flows/<name>"
for file in "${FILES[@]}"; do
  src="${TEMPLATES}/${file}"
  if [[ ! -f "${src}" ]]; then
    fail "missing template: ${src}"
    put_failures=$((put_failures + 1))
    continue
  fi

  # Iterate every flow defined in the file.
  names="$(jq -r '.flows[].name' "${src}")"
  while IFS= read -r flow_name; do
    [[ -z "${flow_name}" ]] && continue

    payload="$(jq -c --arg n "${flow_name}" '.flows[] | select(.name == $n)' "${src}")"
    if [[ -z "${payload}" || "${payload}" == "null" ]]; then
      fail "could not extract flow '${flow_name}' from ${file}"
      put_failures=$((put_failures + 1))
      continue
    fi

    response="$(curl -s -w '\n%{http_code}' --max-time 10 \
      -X PUT "${ODIN_URL}/api/flows/${flow_name}" \
      -H 'Content-Type: application/json' \
      -d "${payload}")"
    body="$(printf '%s' "${response}" | head -n -1)"
    code="$(printf '%s' "${response}" | tail -n 1)"

    if [[ "${code}" != "200" ]]; then
      fail "PUT ${flow_name}: HTTP ${code} — ${body}"
      put_failures=$((put_failures + 1))
      continue
    fi

    if ! printf '%s' "${body}" | jq -e '.ok == true' >/dev/null 2>&1; then
      fail "PUT ${flow_name}: expected {\"ok\":true}, got: ${body}"
      put_failures=$((put_failures + 1))
      continue
    fi
    ok "PUT ${flow_name} → 200 {\"ok\":true}"
  done <<< "${names}"
done

if (( put_failures > 0 )); then
  banner_fail
  exit 1
fi

# ──────────────── 3. GET /api/flows — assert presence ─────────────
log ""
log "3) GET ${ODIN_URL}/api/flows (expect 4 dream_* + home_automation)"
listing="$(curl -s --max-time 5 "${ODIN_URL}/api/flows")"
if [[ -z "${listing}" ]]; then
  fail "empty response from /api/flows"
  banner_fail
  exit 1
fi

matches="$(printf '%s' "${listing}" \
  | jq -r '.[] | .name' \
  | grep -E '^(home_automation|dream_consolidation|dream_exploration|dream_speculation|dream_self_improvement)$' \
  || true)"
match_count="$(printf '%s\n' "${matches}" | grep -c . || true)"

if (( match_count < 5 )); then
  fail "expected 5 matching flow names, found ${match_count}:"
  printf '%s\n' "${matches}" | sed 's/^/    /' >&2
  banner_fail
  exit 1
fi

printf '%s\n' "${matches}" | sort -u | while IFS= read -r n; do
  [[ -n "${n}" ]] && ok "listed: ${n}"
done

# ─────────────────────────── banner ───────────────────────────────
banner_pass
exit 0
