#!/usr/bin/env bash
# scripts/ops/vault-rotate.sh — Rotate the Mimir vault encryption key.
#
# This script:
#   1. Reads all current secrets from the live vault.
#   2. Generates a new 32-byte base64 key.
#   3. Writes the new key to the systemd drop-in on Munin via SSH.
#   4. Restarts yggdrasil-mimir.
#   5. Re-seeds all saved secrets under the new key.
#   6. Verifies round-trip.
#
# Usage:
#   bash scripts/ops/vault-rotate.sh            # interactive (prompts for confirm)
#   DRY_RUN=1 bash scripts/ops/vault-rotate.sh  # plan only, no SSH or vault writes
#
# REQUIRES: sudo on Munin via password (stored in script for fleet consistency).
# Store the new vault key in your password manager under:
#   "Yggdrasil Mimir Vault Key YYYY-MM-DD"

set -euo pipefail

# ─── Colour helpers ───────────────────────────────────────────────────────────
RED=$'\033[0;31m'
GREEN=$'\033[0;32m'
YELLOW=$'\033[1;33m'
CYAN=$'\033[0;36m'
BOLD=$'\033[1m'
RESET=$'\033[0m'

log()  { printf '%s[vault-rotate]%s %s\n' "$CYAN"   "$RESET" "$*"; }
ok()   { printf '  %s✓%s %s\n'           "$GREEN"   "$RESET" "$*"; }
warn() { printf '  %s!%s %s\n'           "$YELLOW"  "$RESET" "$*"; }
err()  { printf '  %s✗%s %s\n'           "$RED"     "$RESET" "$*" >&2; }

banner_pass() {
  printf '\n%s%s================ PASS ================%s\n' "$BOLD" "$GREEN" "$RESET"
}
banner_fail() {
  printf '\n%s%s================ FAIL ================%s\n' "$BOLD" "$RED" "$RESET"
}

# ─── Config ───────────────────────────────────────────────────────────────────
MIMIR_URL="${MIMIR_URL:-http://10.0.65.8:9090}"
MUNIN_HOST="${MUNIN_HOST:-10.0.65.8}"
SSH_USER="${SSH_USER:-jhernandez}"
SUDO_PASS="723559"
DROP_IN_DIR="/etc/systemd/system/yggdrasil-mimir.service.d"
DROP_IN_FILE="${DROP_IN_DIR}/vault.conf"
DRY_RUN="${DRY_RUN:-0}"

# ─── Dry-run helper ───────────────────────────────────────────────────────────
dry_run_or() {
  local desc="$1"; shift
  if [[ "$DRY_RUN" == "1" ]]; then
    warn "[DRY RUN] would: ${desc}"
  else
    "$@"
  fi
}

# ─── Vault helpers ────────────────────────────────────────────────────────────
vault_post() {
  curl -sf --max-time 15 \
    -X POST "${MIMIR_URL}/api/v1/vault" \
    -H "Content-Type: application/json" \
    -d "$1" 2>/dev/null
}

vault_get_value() {
  local key="$1"
  vault_post "{\"action\":\"get\",\"key\":\"${key}\"}" \
    | jq -r '.value // empty' 2>/dev/null || echo ""
}

vault_get_scope() {
  local key="$1"
  vault_post "{\"action\":\"get\",\"key\":\"${key}\"}" \
    | jq -r '.scope // "global"' 2>/dev/null || echo "global"
}

vault_get_tags() {
  local key="$1"
  vault_post "{\"action\":\"get\",\"key\":\"${key}\"}" \
    | jq -c '.tags // []' 2>/dev/null || echo "[]"
}

# ─── Prerequisites ────────────────────────────────────────────────────────────
log "checking prerequisites"
for bin in curl jq ssh openssl; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    err "required binary '$bin' not found on PATH"
    banner_fail
    exit 1
  fi
done
ok "prerequisites satisfied"

# ─── Safety warning + confirmation ───────────────────────────────────────────
printf '\n'
printf '%s%s!!! VAULT KEY ROTATION — READ CAREFULLY !!!%s\n' "$BOLD" "$RED" "$RESET"
printf '\n'
printf '  This script will:\n'
printf '  1. Read ALL current vault secrets (plaintext values in memory).\n'
printf '  2. Generate a NEW 32-byte encryption key.\n'
printf '  3. Update the systemd drop-in on Munin (%s) via SSH.\n' "$MUNIN_HOST"
printf '  4. Restart yggdrasil-mimir (brief downtime).\n'
printf '  5. Re-encrypt all secrets under the new key.\n'
printf '\n'
printf '  After completion: copy the printed NEW_KEY to your password manager\n'
printf '  under "Yggdrasil Mimir Vault Key %s".\n' "$(date +%Y-%m-%d)"
printf '\n'

if [[ "$DRY_RUN" == "1" ]]; then
  warn "DRY_RUN=1: no SSH or vault writes will occur"
else
  read -rp "  Type YES to continue (anything else aborts): " confirm
  if [[ "$confirm" != "YES" ]]; then
    warn "aborted by user"
    exit 0
  fi
fi

# ─── Step 1: Read all current secrets ─────────────────────────────────────────
log "reading current vault keys from ${MIMIR_URL}"

LIST_RESP="$(vault_post '{"action":"list"}' || echo "")"
if [[ -z "$LIST_RESP" ]]; then
  err "vault list returned empty — is Mimir reachable at ${MIMIR_URL}?"
  banner_fail
  exit 1
fi

mapfile -t VAULT_KEYS < <(echo "$LIST_RESP" | jq -r '.secrets[]?.key // empty' 2>/dev/null || true)

if [[ "${#VAULT_KEYS[@]}" -eq 0 ]]; then
  warn "vault is empty (no keys found) — will proceed with key rotation but no re-seeding needed"
else
  ok "found ${#VAULT_KEYS[@]} vault key(s): ${VAULT_KEYS[*]}"
fi

# Collect secret data into parallel arrays
declare -a SECRET_VALUES=()
declare -a SECRET_SCOPES=()
declare -a SECRET_TAGS=()

for key in "${VAULT_KEYS[@]}"; do
  val="$(vault_get_value "$key")"
  scope="$(vault_get_scope "$key")"
  tags="$(vault_get_tags "$key")"

  if [[ -z "$val" ]]; then
    warn "could not read value for key '${key}' — it will be skipped during re-seed"
    val="__UNREADABLE__"
  fi

  SECRET_VALUES+=("$val")
  SECRET_SCOPES+=("$scope")
  SECRET_TAGS+=("$tags")
  ok "read secret: ${key} (scope=${scope})"
done

# ─── Step 2: Generate new key ─────────────────────────────────────────────────
log "generating new 32-byte base64 vault key"
NEW_KEY="$(openssl rand -base64 32)"
KEY_LEN="${#NEW_KEY}"
ok "new key generated (base64 length=${KEY_LEN})"

if [[ "$DRY_RUN" == "1" ]]; then
  warn "[DRY RUN] new key would be: <redacted for dry run>"
fi

# ─── Step 3: Write drop-in on Munin ───────────────────────────────────────────
log "writing new vault key to drop-in on Munin (${MUNIN_HOST})"

DROP_IN_CONTENT="[Service]
Environment=\"MIMIR_VAULT_KEY=${NEW_KEY}\""

dry_run_or \
  "ssh ${SSH_USER}@${MUNIN_HOST} sudo tee ${DROP_IN_FILE}" \
  ssh "${SSH_USER}@${MUNIN_HOST}" bash -s <<REMOTE_SCRIPT
set -euo pipefail
echo '${SUDO_PASS}' | sudo -S mkdir -p '${DROP_IN_DIR}'
printf '%s\n' '${DROP_IN_CONTENT}' | echo '${SUDO_PASS}' | sudo -S tee '${DROP_IN_FILE}' > /dev/null
echo '${SUDO_PASS}' | sudo -S chmod 600 '${DROP_IN_FILE}'
echo '${SUDO_PASS}' | sudo -S chown root:root '${DROP_IN_FILE}'
REMOTE_SCRIPT

if [[ "$DRY_RUN" != "1" ]]; then
  ok "drop-in written to ${DROP_IN_FILE}"
fi

# ─── Step 4: Reload + restart mimir ───────────────────────────────────────────
log "reloading systemd and restarting yggdrasil-mimir on Munin"

dry_run_or \
  "ssh ${SSH_USER}@${MUNIN_HOST} sudo systemctl daemon-reload && sudo systemctl restart yggdrasil-mimir" \
  ssh "${SSH_USER}@${MUNIN_HOST}" bash -s <<REMOTE_RESTART
set -euo pipefail
echo '${SUDO_PASS}' | sudo -S systemctl daemon-reload
echo '${SUDO_PASS}' | sudo -S systemctl restart yggdrasil-mimir
REMOTE_RESTART

if [[ "$DRY_RUN" != "1" ]]; then
  ok "yggdrasil-mimir restarted"
fi

# ─── Step 5: Wait for Mimir health ────────────────────────────────────────────
if [[ "$DRY_RUN" == "1" ]]; then
  warn "[DRY RUN] would wait for ${MIMIR_URL}/health to return 200"
else
  log "waiting for Mimir to become healthy"
  sleep 5

  HEALTHY=0
  for attempt in 1 2 3; do
    if curl -sf --max-time 5 "${MIMIR_URL}/health" >/dev/null 2>&1; then
      HEALTHY=1
      break
    fi
    warn "health check attempt ${attempt}/3 failed — waiting 3s"
    sleep 3
  done

  if [[ "$HEALTHY" -eq 0 ]]; then
    err "Mimir did not become healthy after restart"
    err "Check logs: ssh ${SSH_USER}@${MUNIN_HOST} sudo journalctl -u yggdrasil-mimir -n 50"
    banner_fail
    exit 1
  fi
  ok "Mimir is healthy"
fi

# ─── Step 6: Re-seed all secrets ──────────────────────────────────────────────
if [[ "${#VAULT_KEYS[@]}" -gt 0 ]]; then
  log "re-seeding ${#VAULT_KEYS[@]} secret(s) under new key"

  for i in "${!VAULT_KEYS[@]}"; do
    key="${VAULT_KEYS[$i]}"
    val="${SECRET_VALUES[$i]}"
    scope="${SECRET_SCOPES[$i]}"
    tags="${SECRET_TAGS[$i]}"

    if [[ "$val" == "__UNREADABLE__" ]]; then
      warn "skipping unreadable secret: ${key}"
      continue
    fi

    # Escape value for JSON (basic — replaces double-quotes and backslashes)
    val_escaped="${val//\\/\\\\}"
    val_escaped="${val_escaped//\"/\\\"}"

    PAYLOAD="{\"action\":\"set\",\"key\":\"${key}\",\"value\":\"${val_escaped}\",\"scope\":\"${scope}\",\"tags\":${tags}}"

    dry_run_or \
      "POST vault set ${key}" \
      bash -c "curl -sf --max-time 15 \
        -X POST '${MIMIR_URL}/api/v1/vault' \
        -H 'Content-Type: application/json' \
        -d '${PAYLOAD}' >/dev/null"

    if [[ "$DRY_RUN" != "1" ]]; then
      ok "re-seeded: ${key}"
    fi
  done
fi

# ─── Step 7: Verification ─────────────────────────────────────────────────────
if [[ "$DRY_RUN" == "1" ]]; then
  warn "[DRY RUN] would: verify all keys present via vault list + sample get"
else
  log "verifying vault round-trip"

  VERIFY_RESP="$(vault_post '{"action":"list"}' || echo "")"
  mapfile -t VERIFY_KEYS < <(echo "$VERIFY_RESP" | jq -r '.secrets[]?.key // empty' 2>/dev/null || true)
  ok "vault list after rotation: ${#VERIFY_KEYS[@]} key(s)"

  if [[ "${#VAULT_KEYS[@]}" -gt 0 ]]; then
    SAMPLE_KEY="${VAULT_KEYS[0]}"
    SAMPLE_VAL="$(vault_get_value "$SAMPLE_KEY")"
    if [[ -z "$SAMPLE_VAL" || "$SAMPLE_VAL" == "__UNREADABLE__" ]]; then
      err "sample get for '${SAMPLE_KEY}' returned empty after rotation"
      banner_fail
      exit 1
    fi
    ok "sample get '${SAMPLE_KEY}' returned value (length=${#SAMPLE_VAL})"
  fi
fi

# ─── Done ─────────────────────────────────────────────────────────────────────
printf '\n'
if [[ "$DRY_RUN" != "1" ]]; then
  printf '%s%s  ACTION REQUIRED: Save the new vault key now!%s\n' "$BOLD" "$YELLOW" "$RESET"
  printf '  Store in password manager as "Yggdrasil Mimir Vault Key %s"\n' "$(date +%Y-%m-%d)"
  printf '\n'
  printf '  NEW_KEY: %s\n' "$NEW_KEY"
  printf '\n'
  printf '  Drop-in location on Munin: %s\n' "$DROP_IN_FILE"
  printf '  If the drop-in is lost, restore from password manager and re-run step 4.\n'
fi

banner_pass
