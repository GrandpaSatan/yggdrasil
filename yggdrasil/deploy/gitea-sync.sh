#!/usr/bin/env bash
# gitea-sync.sh — Discovers all repos from Gitea API, clones new ones,
# pulls updates for existing ones. Runs on Hugin via systemd timer.
set -euo pipefail

GITEA_URL="${GITEA_URL:-http://${GITEA_IP:-localhost}:3000}"
GITEA_TOKEN="${GITEA_TOKEN}"
REPO_DIR="${REPO_DIR:-/home/${USER}/repos}"
SSH_PORT="${GITEA_SSH_PORT:-22}"
SKIP_REPOS="${SKIP_REPOS:-Crucible}"
LOG_TAG="gitea-sync"

log() { logger -t "$LOG_TAG" "$*"; echo "[$(date '+%H:%M:%S')] $*"; }

if [[ -z "$GITEA_TOKEN" ]]; then
    log "ERROR: GITEA_TOKEN not set"
    exit 1
fi

mkdir -p "$REPO_DIR"

# Fetch all repos from Gitea API (paginated)
page=1
repos=()
while true; do
    response=$(curl -sf -H "Authorization: token $GITEA_TOKEN" \
        "$GITEA_URL/api/v1/user/repos?limit=50&page=$page" 2>/dev/null) || {
        log "ERROR: Failed to fetch repos from Gitea API (page $page)"
        exit 1
    }

    count=$(echo "$response" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null)
    if [[ "$count" -eq 0 ]]; then
        break
    fi

    # Extract repo names and SSH URLs, fix port if needed
    while IFS='|' read -r name ssh_url; do
        # Replace :2222/ with :$SSH_PORT/ in SSH URL if port differs
        ssh_url=$(echo "$ssh_url" | sed "s|:2222/|:${SSH_PORT}/|g")
        repos+=("$name|$ssh_url")
    done < <(echo "$response" | python3 -c "
import sys, json
for r in json.load(sys.stdin):
    print(f\"{r['name']}|{r['ssh_url']}\")
")

    page=$((page + 1))
done

log "Found ${#repos[@]} repos on Gitea"

cloned=0
updated=0
failed=0

for entry in "${repos[@]}"; do
    name="${entry%%|*}"
    ssh_url="${entry#*|}"
    target="$REPO_DIR/$name"

    # Skip excluded repos
    if echo ",$SKIP_REPOS," | grep -qi ",$name,"; then
        log "Skipping $name (in SKIP_REPOS)"
        continue
    fi

    if [[ -d "$target/.git" ]]; then
        # Existing repo — fetch and reset to remote default branch
        log "Updating $name..."
        if git -C "$target" fetch origin 2>&1; then
            # Detect remote HEAD branch
            remote_head=$(git -C "$target" symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|refs/remotes/origin/||' || echo "")
            if [[ -z "$remote_head" ]]; then
                # Try to set it from remote
                git -C "$target" remote set-head origin --auto 2>/dev/null || true
                remote_head=$(git -C "$target" symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|refs/remotes/origin/||' || echo "main")
            fi
            git -C "$target" checkout "$remote_head" 2>/dev/null || true
            git -C "$target" reset --hard "origin/$remote_head" 2>&1
            updated=$((updated + 1))
            log "Updated $name to origin/$remote_head"
        else
            log "ERROR: Failed to fetch $name"
            failed=$((failed + 1))
        fi
    else
        # New repo — clone
        log "Cloning $name from $ssh_url..."
        if GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -p $SSH_PORT" \
           git clone "$ssh_url" "$target" 2>&1 | tail -1; then
            cloned=$((cloned + 1))
        else
            log "ERROR: Failed to clone $name"
            failed=$((failed + 1))
        fi
    fi
done

log "Done: $cloned cloned, $updated updated, $failed failed (${#repos[@]} total)"
