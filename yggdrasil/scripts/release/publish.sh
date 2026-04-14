#!/usr/bin/env bash
# scripts/release/publish.sh — Package and publish the yggdrasil-local VSIX to Gitea
# and optionally to GitHub.
#
# Usage:
#   bash scripts/release/publish.sh 0.12.0
#   ALLOW_TAG=1 bash scripts/release/publish.sh 0.12.0      # create tag if missing
#   DRY_RUN=1   bash scripts/release/publish.sh 0.12.0-test # plan only, no actions
#
# Secrets are read from the Mimir vault (live at MIMIR_URL). If github_token is
# absent from the vault, GitHub publishing is skipped with a warning.
#
# Requires: curl, jq, git, node/npx (@vscode/vsce). gh CLI optional for GitHub.

set -euo pipefail

# ─── Colour helpers ───────────────────────────────────────────────────────────
RED=$'\033[0;31m'
GREEN=$'\033[0;32m'
YELLOW=$'\033[1;33m'
CYAN=$'\033[0;36m'
BOLD=$'\033[1m'
RESET=$'\033[0m'

log()  { printf '%s[publish]%s %s\n' "$CYAN"   "$RESET" "$*"; }
ok()   { printf '  %s✓%s %s\n'      "$GREEN"   "$RESET" "$*"; }
warn() { printf '  %s!%s %s\n'      "$YELLOW"  "$RESET" "$*"; }
err()  { printf '  %s✗%s %s\n'      "$RED"     "$RESET" "$*" >&2; }

banner_pass() {
  printf '\n%s%s================ PASS ================%s\n' "$BOLD" "$GREEN" "$RESET"
}
banner_fail() {
  printf '\n%s%s================ FAIL ================%s\n' "$BOLD" "$RED" "$RESET"
}

# ─── Args / env ───────────────────────────────────────────────────────────────
VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
  err "Usage: bash publish.sh <version>  (e.g. 0.12.0)"
  banner_fail
  exit 1
fi

MIMIR_URL="${MIMIR_URL:-http://10.0.65.8:9090}"
DRY_RUN="${DRY_RUN:-0}"
ALLOW_TAG="${ALLOW_TAG:-0}"

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EXT_DIR="${REPO_ROOT}/extensions/yggdrasil-local"
DIST_DIR="${EXT_DIR}/dist"
VSIX_NAME="yggdrasil-local-${VERSION}.vsix"
VSIX_PATH="${DIST_DIR}/${VSIX_NAME}"

# ─── Dry-run guard ────────────────────────────────────────────────────────────
# dry_run_or <description> <cmd> [args...]
# In DRY_RUN mode: prints what would happen. Otherwise: executes.
dry_run_or() {
  local desc="$1"; shift
  if [[ "$DRY_RUN" == "1" ]]; then
    warn "[DRY RUN] would: ${desc}"
  else
    "$@"
  fi
}

# ─── Vault secret reader ──────────────────────────────────────────────────────
vault_get() {
  local key="$1"
  local response
  response="$(curl -sf --max-time 10 \
    -X POST "${MIMIR_URL}/api/v1/vault" \
    -H "Content-Type: application/json" \
    -d "{\"action\":\"get\",\"key\":\"${key}\"}" 2>/dev/null || echo "")"

  if [[ -z "$response" ]]; then
    echo ""
    return
  fi

  # Returns empty string if key absent (.value may be null)
  echo "$response" | jq -r '.value // empty' 2>/dev/null || echo ""
}

# ─── Prerequisite checks ──────────────────────────────────────────────────────
log "checking prerequisites"
for bin in curl jq git node npx; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    err "required binary '$bin' not found on PATH"
    banner_fail
    exit 1
  fi
done
ok "prerequisites satisfied"

# ─── Vault reads ──────────────────────────────────────────────────────────────
log "reading secrets from Mimir vault (${MIMIR_URL})"
GITEA_USER="$(vault_get gitea_user)"
GITEA_PASSWORD="$(vault_get gitea_password)"
GITEA_URL="$(vault_get gitea_url)"
GITHUB_TOKEN="$(vault_get github_token)"

if [[ -z "$GITEA_USER" || -z "$GITEA_PASSWORD" || -z "$GITEA_URL" ]]; then
  err "vault missing one or more required secrets: gitea_user, gitea_password, gitea_url"
  err "seed them via: curl -X POST ${MIMIR_URL}/api/v1/vault -d '{\"action\":\"set\",\"key\":\"gitea_user\",\"value\":\"jesus\"}'"
  banner_fail
  exit 1
fi
ok "gitea_user=${GITEA_USER}, gitea_url=${GITEA_URL}"

SKIP_GITHUB=0
if [[ -z "$GITHUB_TOKEN" ]]; then
  warn "github_token not found in vault — GitHub release will be skipped"
  SKIP_GITHUB=1
else
  ok "github_token present — GitHub release enabled"
fi

# ─── Git tag validation ───────────────────────────────────────────────────────
TAG="v${VERSION}"
log "checking git tag ${TAG}"
if git -C "$REPO_ROOT" rev-parse "$TAG" >/dev/null 2>&1; then
  ok "tag ${TAG} exists"
elif [[ "$ALLOW_TAG" == "1" ]]; then
  log "ALLOW_TAG=1: creating tag ${TAG}"
  dry_run_or "git tag ${TAG}" git -C "$REPO_ROOT" tag "$TAG"
  ok "created tag ${TAG}"
else
  err "tag ${TAG} does not exist. Create it with: git tag ${TAG}"
  err "or re-run with ALLOW_TAG=1 to create it automatically"
  banner_fail
  exit 1
fi

# ─── VSIX packaging ───────────────────────────────────────────────────────────
log "packaging VSIX → ${VSIX_PATH}"
mkdir -p "$DIST_DIR"

if [[ "$DRY_RUN" == "1" ]]; then
  warn "[DRY RUN] would: cd ${EXT_DIR} && npx @vscode/vsce package --no-dependencies --out ${VSIX_PATH}"
else
  (cd "$EXT_DIR" && npx @vscode/vsce package --no-dependencies --out "$VSIX_PATH")
  if [[ ! -f "$VSIX_PATH" ]]; then
    err "VSIX not produced at ${VSIX_PATH}"
    banner_fail
    exit 1
  fi
fi

if [[ "$DRY_RUN" == "1" ]]; then
  warn "[DRY RUN] sha256/size: (would compute from ${VSIX_PATH})"
  VSIX_SHA256="<sha256-dry-run>"
  VSIX_SIZE="<size-dry-run>"
else
  VSIX_SHA256="$(sha256sum "$VSIX_PATH" | awk '{print $1}')"
  VSIX_SIZE="$(stat -c%s "$VSIX_PATH")"
  ok "VSIX sha256: ${VSIX_SHA256}"
  ok "VSIX size:   ${VSIX_SIZE} bytes"
fi

# ─── Gitea release creation ───────────────────────────────────────────────────
log "creating Gitea release ${TAG} on ${GITEA_URL}"

GITEA_RELEASE_URL=""
RELEASE_ID=""

if [[ "$DRY_RUN" == "1" ]]; then
  warn "[DRY RUN] would: POST ${GITEA_URL}/api/v1/repos/jesus/Yggdrasil/releases with Basic Auth"
  warn "[DRY RUN] would: upload ${VSIX_NAME} as release asset"
  GITEA_RELEASE_URL="${GITEA_URL}/jesus/Yggdrasil/releases/tag/${TAG}"
else
  RELEASE_RESP="$(curl -sf --max-time 30 \
    -X POST "${GITEA_URL}/api/v1/repos/jesus/Yggdrasil/releases" \
    -u "${GITEA_USER}:${GITEA_PASSWORD}" \
    -H "Content-Type: application/json" \
    -d "{\"tag_name\":\"${TAG}\",\"name\":\"${TAG}\",\"body\":\"Release ${TAG}\"}" 2>/dev/null)"

  RELEASE_ID="$(echo "$RELEASE_RESP" | jq -r '.id // empty')"
  if [[ -z "$RELEASE_ID" ]]; then
    err "Gitea release creation failed. Response: ${RELEASE_RESP}"
    banner_fail
    exit 1
  fi
  ok "Gitea release created (id=${RELEASE_ID})"

  # Upload VSIX asset
  log "uploading ${VSIX_NAME} to Gitea release ${RELEASE_ID}"
  UPLOAD_RESP="$(curl -sf --max-time 120 \
    -X POST "${GITEA_URL}/api/v1/repos/jesus/Yggdrasil/releases/${RELEASE_ID}/assets?name=${VSIX_NAME}" \
    -u "${GITEA_USER}:${GITEA_PASSWORD}" \
    -H "Content-Type: multipart/form-data" \
    -F "attachment=@${VSIX_PATH};type=application/octet-stream" 2>/dev/null)"

  ASSET_ID="$(echo "$UPLOAD_RESP" | jq -r '.id // empty')"
  if [[ -z "$ASSET_ID" ]]; then
    err "asset upload failed. Response: ${UPLOAD_RESP}"
    banner_fail
    exit 1
  fi
  ok "VSIX asset uploaded (asset_id=${ASSET_ID})"

  GITEA_RELEASE_URL="${GITEA_URL}/jesus/Yggdrasil/releases/tag/${TAG}"
  ok "Gitea release URL: ${GITEA_RELEASE_URL}"
fi

# ─── GitHub release ───────────────────────────────────────────────────────────
GITHUB_RELEASE_URL=""

if [[ "$SKIP_GITHUB" == "1" ]]; then
  warn "skipping GitHub release (no github_token in vault)"
else
  log "creating GitHub release ${TAG}"

  if [[ "$DRY_RUN" == "1" ]]; then
    if command -v gh >/dev/null 2>&1; then
      warn "[DRY RUN] would: GITHUB_TOKEN=<token> gh release create ${TAG} ${VSIX_PATH} --title '${TAG}' --notes 'Release ${TAG}'"
    else
      warn "[DRY RUN] would: curl POST https://api.github.com/repos/GrandpaSatan/Yggdrasil/releases (gh not installed)"
    fi
    GITHUB_RELEASE_URL="https://github.com/GrandpaSatan/Yggdrasil/releases/tag/${TAG}"
  elif command -v gh >/dev/null 2>&1; then
    # Preferred: gh CLI
    GITHUB_TOKEN="$GITHUB_TOKEN" gh release create "$TAG" "$VSIX_PATH" \
      --title "$TAG" \
      --notes "Release ${TAG}" \
      --repo GrandpaSatan/Yggdrasil
    ok "GitHub release created via gh CLI"
    GITHUB_RELEASE_URL="https://github.com/GrandpaSatan/Yggdrasil/releases/tag/${TAG}"
  else
    # Fallback: raw curl
    warn "gh CLI not installed — using curl fallback for GitHub"

    GH_RESP="$(curl -sf --max-time 30 \
      -X POST "https://api.github.com/repos/GrandpaSatan/Yggdrasil/releases" \
      -H "Authorization: Bearer ${GITHUB_TOKEN}" \
      -H "Content-Type: application/json" \
      -H "X-GitHub-Api-Version: 2022-11-28" \
      -d "{\"tag_name\":\"${TAG}\",\"name\":\"${TAG}\",\"body\":\"Release ${TAG}\"}" 2>/dev/null)"

    GH_RELEASE_ID="$(echo "$GH_RESP" | jq -r '.id // empty')"
    GH_UPLOAD_URL="$(echo "$GH_RESP" | jq -r '.upload_url // empty' | sed 's/{?name,label}//')"

    if [[ -z "$GH_RELEASE_ID" || -z "$GH_UPLOAD_URL" ]]; then
      err "GitHub release creation failed. Response: ${GH_RESP}"
      banner_fail
      exit 1
    fi
    ok "GitHub release created (id=${GH_RELEASE_ID})"

    # Upload asset
    GH_ASSET_RESP="$(curl -sf --max-time 120 \
      -X POST "${GH_UPLOAD_URL}?name=${VSIX_NAME}" \
      -H "Authorization: Bearer ${GITHUB_TOKEN}" \
      -H "Content-Type: application/octet-stream" \
      --data-binary "@${VSIX_PATH}" 2>/dev/null)"

    GH_ASSET_ID="$(echo "$GH_ASSET_RESP" | jq -r '.id // empty')"
    if [[ -z "$GH_ASSET_ID" ]]; then
      err "GitHub asset upload failed. Response: ${GH_ASSET_RESP}"
      banner_fail
      exit 1
    fi
    ok "VSIX uploaded to GitHub (asset_id=${GH_ASSET_ID})"

    GITHUB_RELEASE_URL="https://github.com/GrandpaSatan/Yggdrasil/releases/tag/${TAG}"
    ok "GitHub release URL: ${GITHUB_RELEASE_URL}"
  fi
fi

# ─── Summary ──────────────────────────────────────────────────────────────────
printf '\n'
log "Release ${TAG} complete"
[[ -n "$GITEA_RELEASE_URL" ]]  && ok "Gitea:  ${GITEA_RELEASE_URL}"
[[ -n "$GITHUB_RELEASE_URL" ]] && ok "GitHub: ${GITHUB_RELEASE_URL}"
if [[ "$DRY_RUN" != "1" ]]; then
  ok "sha256: ${VSIX_SHA256}"
  ok "size:   ${VSIX_SIZE} bytes"
fi

banner_pass
