#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# claude-config-sync.sh — Centralize Claude Code config on Munin
# ═══════════════════════════════════════════════════════════════════════
#
# Syncs Claude Code configuration (agents, memory, hooks, settings) to
# a central server and manages local symlinks to a sync cache.
#
# Subcommands:
#   init         First-time setup: move files → cache, symlink, push
#   push         Push local cache to remote
#   pull         Pull remote to local cache
#   sync         Bidirectional sync (push then pull with --update)
#   status       Show sync state of all managed files
#   consolidate  Merge memory from another workstation export
#   bootstrap    Fresh workstation: pull from remote, create symlinks
#   rollback     Restore from a remote backup
#
# Usage:
#   ./claude-config-sync.sh init
#   ./claude-config-sync.sh push [--dry-run]
#   ./claude-config-sync.sh pull [--force]
#   ./claude-config-sync.sh sync
#   ./claude-config-sync.sh status
#   ./claude-config-sync.sh consolidate --from /path/to/export
#   ./claude-config-sync.sh bootstrap
#   ./claude-config-sync.sh rollback [TIMESTAMP]
#
# Environment:
#   MUNIN_IP       Remote host IP (required — set in your env or .env)
#   DEPLOY_USER    SSH user (required — set in your env or .env)
#   WORKSTATION_ID Override hostname-based ID
#
# Idempotent — safe to re-run.
# ═══════════════════════════════════════════════════════════════════════
set -euo pipefail

# ── Constants ───────────────────────────────────────────────────────────
REMOTE_HOST="${MUNIN_IP:?Set MUNIN_IP to the remote server IP}"
REMOTE_USER="${DEPLOY_USER:?Set DEPLOY_USER to the SSH username}"
REMOTE_BASE="/opt/yggdrasil/claude-config"
CLAUDE_DIR="$HOME/.claude"
SYNC_CACHE="$CLAUDE_DIR/.sync-cache"
STATE_DIR="$HOME/.config/yggdrasil"
STATE_FILE="$STATE_DIR/claude-sync-state.json"
BACKUP_DIR_REMOTE="$REMOTE_BASE/.backups"

SSH_OPTS="-o ConnectTimeout=3 -o BatchMode=yes -o StrictHostKeyChecking=accept-new"
RSYNC_OPTS="--archive --compress --checksum --timeout=10"

# Files to sync (relative to ~/.claude/)
SYNC_FILES=("CLAUDE.md" "settings.json")

# Directories to sync (relative to ~/.claude/)
SYNC_DIRS=("agents" "teams")

# ── Colors & Logging ───────────────────────────────────────────────────
RED=$'\033[0;31m'
GREEN=$'\033[0;32m'
YELLOW=$'\033[1;33m'
CYAN=$'\033[0;36m'
BOLD=$'\033[1m'
NC=$'\033[0m'

log()  { printf '%s\n' "${CYAN}[sync]${NC} $1"; }
ok()   { printf '%s\n' "${GREEN}  ✓${NC} $1"; }
warn() { printf '%s\n' "${YELLOW}  !${NC} $1"; }
err()  { printf '%s\n' "${RED}  ✗${NC} $1"; }

# ── Flags ──────────────────────────────────────────────────────────────
DRY_RUN=false
FORCE=false
CONSOLIDATE_FROM=""
WORKSTATION_ID="${WORKSTATION_ID:-$(hostname)}"

# ── Helpers ────────────────────────────────────────────────────────────

usage() {
    cat <<'EOF'
Usage: claude-config-sync.sh <command> [options]

Commands:
  init          First-time setup (move → cache → symlink → push)
  push          Push local changes to remote
  pull          Pull remote changes to local cache
  sync          Bidirectional sync
  status        Show sync state of managed files
  consolidate   Merge memory from another source (--from PATH)
  bootstrap     Set up fresh workstation from remote
  rollback      Restore from remote backup [TIMESTAMP]

Options:
  --dry-run           Show what would happen without making changes
  --force             Overwrite without conflict checks
  --workstation-id ID Override hostname-based workstation identifier
  --from PATH         Source path for consolidate (local dir or user@host:path)
  -h, --help          Show this help
EOF
    exit 0
}

check_remote() {
    if ! ssh $SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" true 2>/dev/null; then
        err "Cannot reach $REMOTE_USER@$REMOTE_HOST"
        err "Check SSH key auth and network connectivity"
        return 1
    fi
}

remote_cmd() {
    ssh $SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" "$@"
}

compute_hash() {
    local file="$1"
    if [[ -f "$file" ]]; then
        sha256sum "$file" | awk '{print $1}'
    elif [[ -d "$file" ]]; then
        find "$file" -type f -print0 | sort -z | xargs -0 sha256sum 2>/dev/null | sha256sum | awk '{print $1}'
    else
        echo "missing"
    fi
}

run_or_dry() {
    if $DRY_RUN; then
        printf '%s\n' "${YELLOW}  [dry-run]${NC} $*"
    else
        "$@"
    fi
}

timestamp() {
    date -u +"%Y-%m-%dT%H:%M:%SZ"
}

save_state() {
    mkdir -p "$STATE_DIR"
    local files_json="{}"

    # Hash all synced files
    for f in "${SYNC_FILES[@]}"; do
        local fp="$SYNC_CACHE/$f"
        if [[ -f "$fp" ]]; then
            local h
            h=$(compute_hash "$fp")
            files_json=$(echo "$files_json" | jq --arg k "$f" --arg v "$h" '. + {($k): $v}')
        fi
    done

    # Hash all synced directories (individual files within)
    for d in "${SYNC_DIRS[@]}"; do
        local dp="$SYNC_CACHE/$d"
        if [[ -d "$dp" ]]; then
            while IFS= read -r -d '' file; do
                local rel="${file#"$SYNC_CACHE/"}"
                local h
                h=$(compute_hash "$file")
                files_json=$(echo "$files_json" | jq --arg k "$rel" --arg v "$h" '. + {($k): $v}')
            done < <(find "$dp" -type f -print0 | sort -z)
        fi
    done

    # Hash project memory files
    if [[ -d "$SYNC_CACHE/projects" ]]; then
        while IFS= read -r -d '' file; do
            local rel="${file#"$SYNC_CACHE/"}"
            local h
            h=$(compute_hash "$file")
            files_json=$(echo "$files_json" | jq --arg k "$rel" --arg v "$h" '. + {($k): $v}')
        done < <(find "$SYNC_CACHE/projects" -type f -print0 | sort -z)
    fi

    jq -n \
        --arg wid "$WORKSTATION_ID" \
        --arg ts "$(timestamp)" \
        --argjson files "$files_json" \
        '{workstation_id: $wid, last_sync: $ts, files: $files}' \
        > "$STATE_FILE"
}

load_state() {
    if [[ -f "$STATE_FILE" ]]; then
        cat "$STATE_FILE"
    else
        echo '{"workstation_id":"","last_sync":"","files":{}}'
    fi
}

# Discover project memory directories (only those with actual .md files)
discover_project_memories() {
    local proj_base="$1"  # either CLAUDE_DIR or SYNC_CACHE
    if [[ ! -d "$proj_base/projects" ]]; then
        return
    fi
    for proj_dir in "$proj_base"/projects/*/; do
        [[ -d "$proj_dir" ]] || continue
        local mem_dir="${proj_dir}memory"
        if [[ -d "$mem_dir" ]] && ls "$mem_dir"/*.md &>/dev/null; then
            local encoded
            encoded=$(basename "$(dirname "$mem_dir")")
            echo "$encoded"
        fi
    done | sort -u
}

# Create a symlink, handling existing symlinks/files gracefully
make_symlink() {
    local target="$1"   # what the symlink points to
    local link="$2"     # where the symlink is created

    if [[ -L "$link" ]]; then
        local current
        current=$(readlink "$link")
        if [[ "$current" == "$target" ]]; then
            return 0  # already correct
        fi
        run_or_dry rm "$link"
    elif [[ -e "$link" ]]; then
        if $DRY_RUN; then
            # In dry-run, the mv didn't happen so the file is still here
            run_or_dry ln -sf "$target" "$link"
            return 0
        fi
        err "$link exists and is not a symlink — skipping (move it first)"
        return 1
    fi

    run_or_dry ln -s "$target" "$link"
}

# ═══════════════════════════════════════════════════════════════════════
# INIT — First-time setup
# ═══════════════════════════════════════════════════════════════════════
cmd_init() {
    log "Initializing config sync for workstation: ${BOLD}$WORKSTATION_ID${NC}"
    echo ""

    # 1. Check remote connectivity
    log "Checking remote connectivity..."
    if ! check_remote; then
        exit 1
    fi
    ok "Remote reachable: $REMOTE_USER@$REMOTE_HOST"

    # 2. Create remote directory structure
    log "Creating remote directory structure..."
    if ! run_or_dry remote_cmd "mkdir -p '$REMOTE_BASE'/{agents,teams,projects,project-configs,.backups}" 2>/dev/null; then
        warn "Cannot create dirs as $REMOTE_USER — trying with sudo..."
        run_or_dry remote_cmd "sudo mkdir -p '$REMOTE_BASE'/{agents,teams,projects,project-configs,.backups} && sudo chown -R '$REMOTE_USER:$REMOTE_USER' '$REMOTE_BASE'"
    fi
    ok "Remote dirs: $REMOTE_BASE/"

    # 3. Create local sync cache
    log "Setting up local sync cache..."
    run_or_dry mkdir -p "$SYNC_CACHE"/{agents,teams,projects}
    ok "Cache: $SYNC_CACHE/"

    # 4. Move files into cache and create symlinks
    log "Moving files to sync cache..."

    # -- Individual files --
    for f in "${SYNC_FILES[@]}"; do
        local src="$CLAUDE_DIR/$f"
        local dst="$SYNC_CACHE/$f"
        if [[ -L "$src" ]]; then
            ok "$f: already symlinked"
            continue
        fi
        if [[ -f "$src" ]]; then
            run_or_dry mv "$src" "$dst"
            make_symlink ".sync-cache/$f" "$src"
            ok "$f: moved + symlinked"
        else
            warn "$f: not found, skipping"
        fi
    done

    # -- Directories --
    for d in "${SYNC_DIRS[@]}"; do
        local src="$CLAUDE_DIR/$d"
        local dst="$SYNC_CACHE/$d"
        if [[ -L "$src" ]]; then
            ok "$d/: already symlinked"
            continue
        fi
        if [[ -d "$src" ]]; then
            # If cache dir already has content (re-run), merge
            if [[ -d "$dst" ]] && [[ "$(ls -A "$dst" 2>/dev/null)" ]]; then
                run_or_dry cp -a "$src"/. "$dst"/
                run_or_dry rm -rf "$src"
            else
                run_or_dry mv "$src" "$dst"
            fi
            make_symlink ".sync-cache/$d" "$src"
            ok "$d/: moved + symlinked"
        else
            run_or_dry mkdir -p "$dst"
            make_symlink ".sync-cache/$d" "$src"
            ok "$d/: created + symlinked"
        fi
    done

    # -- Project memory directories --
    log "Processing project memory directories..."
    local mem_projects
    mem_projects=$(discover_project_memories "$CLAUDE_DIR")

    for encoded in $mem_projects; do
        local src_mem="$CLAUDE_DIR/projects/$encoded/memory"
        local dst_mem="$SYNC_CACHE/projects/$encoded/memory"

        if [[ -L "$src_mem" ]]; then
            ok "projects/$encoded/memory: already symlinked"
            continue
        fi

        run_or_dry mkdir -p "$SYNC_CACHE/projects/$encoded"

        if [[ -d "$src_mem" ]]; then
            run_or_dry mv "$src_mem" "$dst_mem"

            # Compute relative symlink path from project dir back to .sync-cache
            # ~/.claude/projects/ENCODED/memory → ~/.claude/.sync-cache/projects/ENCODED/memory
            # Relative: ../../.sync-cache/projects/ENCODED/memory
            make_symlink "../../.sync-cache/projects/$encoded/memory" "$src_mem"
            ok "projects/$encoded/memory: moved + symlinked"
        fi
    done

    # 5. Initial push
    echo ""
    cmd_push

    # 6. Save state
    save_state
    ok "Sync state saved: $STATE_FILE"

    echo ""
    log "Init complete! Files are now in $SYNC_CACHE and symlinked."
    log "Run '$(basename "$0") sync' to keep in sync, or install the systemd timer."
}

# ═══════════════════════════════════════════════════════════════════════
# PUSH — Upload local cache to remote
# ═══════════════════════════════════════════════════════════════════════
cmd_push() {
    log "Pushing to $REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/ ..."

    if ! check_remote; then
        warn "Remote unreachable — skipping push"
        return 0
    fi

    # Create timestamped backup on remote before overwriting
    local ts
    ts=$(date -u +"%Y%m%dT%H%M%SZ")
    run_or_dry remote_cmd "
        if [ -d '$REMOTE_BASE/agents' ] || [ -f '$REMOTE_BASE/CLAUDE.md' ]; then
            mkdir -p '$BACKUP_DIR_REMOTE/$ts'
            cp -a '$REMOTE_BASE'/CLAUDE.md '$REMOTE_BASE'/settings.json \
                  '$BACKUP_DIR_REMOTE/$ts/' 2>/dev/null || true
            cp -a '$REMOTE_BASE'/agents '$BACKUP_DIR_REMOTE/$ts/agents' 2>/dev/null || true
            cp -a '$REMOTE_BASE'/teams '$BACKUP_DIR_REMOTE/$ts/teams' 2>/dev/null || true
            cp -a '$REMOTE_BASE'/projects '$BACKUP_DIR_REMOTE/$ts/projects' 2>/dev/null || true
        fi
    "

    # Sync individual files
    for f in "${SYNC_FILES[@]}"; do
        local src="$SYNC_CACHE/$f"
        if [[ -f "$src" ]]; then
            run_or_dry rsync $RSYNC_OPTS "$src" \
                "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/$f"
            ok "Pushed $f"
        fi
    done

    # Sync directories
    for d in "${SYNC_DIRS[@]}"; do
        local src="$SYNC_CACHE/$d/"
        if [[ -d "$src" ]]; then
            run_or_dry rsync $RSYNC_OPTS --delete "$src" \
                "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/$d/"
            ok "Pushed $d/"
        fi
    done

    # Sync project memories
    if [[ -d "$SYNC_CACHE/projects" ]]; then
        local mem_projects
        mem_projects=$(discover_project_memories "$SYNC_CACHE")
        for encoded in $mem_projects; do
            local src="$SYNC_CACHE/projects/$encoded/memory/"
            run_or_dry remote_cmd "mkdir -p '$REMOTE_BASE/projects/$encoded/memory'"
            run_or_dry rsync $RSYNC_OPTS --delete "$src" \
                "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/projects/$encoded/memory/"
            ok "Pushed projects/$encoded/memory/"
        done
    fi

    # Update remote metadata
    run_or_dry remote_cmd "
        echo '{\"last_push_by\": \"$WORKSTATION_ID\", \"timestamp\": \"$(timestamp)\"}' \
            > '$REMOTE_BASE/.sync-meta.json'
    "

    save_state
    ok "Push complete"
}

# ═══════════════════════════════════════════════════════════════════════
# PULL — Download remote to local cache
# ═══════════════════════════════════════════════════════════════════════
cmd_pull() {
    log "Pulling from $REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/ ..."

    if ! check_remote; then
        warn "Remote unreachable — skipping pull"
        return 0
    fi

    # Ensure cache structure exists
    mkdir -p "$SYNC_CACHE"/{agents,teams,projects}

    local rsync_extra=""
    if $FORCE; then
        rsync_extra="--delete"
    fi

    # Pull individual files
    for f in "${SYNC_FILES[@]}"; do
        if run_or_dry rsync $RSYNC_OPTS --backup --suffix=".bak" \
            "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/$f" "$SYNC_CACHE/$f" 2>/dev/null; then
            ok "Pulled $f"
        else
            warn "$f: not found on remote"
        fi
    done

    # Pull directories
    for d in "${SYNC_DIRS[@]}"; do
        if run_or_dry rsync $RSYNC_OPTS --backup --suffix=".bak" $rsync_extra \
            "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/$d/" "$SYNC_CACHE/$d/" 2>/dev/null; then
            ok "Pulled $d/"
        else
            warn "$d/: not found on remote"
        fi
    done

    # Pull project memories
    local remote_projects
    remote_projects=$(remote_cmd "ls '$REMOTE_BASE/projects/' 2>/dev/null" || true)
    for encoded in $remote_projects; do
        if remote_cmd "test -d '$REMOTE_BASE/projects/$encoded/memory'" 2>/dev/null; then
            mkdir -p "$SYNC_CACHE/projects/$encoded"
            run_or_dry rsync $RSYNC_OPTS --backup --suffix=".bak" \
                "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/projects/$encoded/memory/" \
                "$SYNC_CACHE/projects/$encoded/memory/"
            ok "Pulled projects/$encoded/memory/"
        fi
    done

    save_state
    ok "Pull complete"
}

# ═══════════════════════════════════════════════════════════════════════
# SYNC — Bidirectional (push local changes, pull remote changes)
# ═══════════════════════════════════════════════════════════════════════
cmd_sync() {
    log "Bidirectional sync..."

    if ! check_remote; then
        warn "Remote unreachable — skipping sync"
        return 0
    fi

    # Push first (local wins for files changed locally)
    # Use --update so we only overwrite remote if local is newer
    log "Phase 1: Pushing local changes..."
    for f in "${SYNC_FILES[@]}"; do
        local src="$SYNC_CACHE/$f"
        if [[ -f "$src" ]]; then
            run_or_dry rsync $RSYNC_OPTS --update "$src" \
                "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/$f" 2>/dev/null && \
                ok "Synced $f (push)" || true
        fi
    done

    for d in "${SYNC_DIRS[@]}"; do
        local src="$SYNC_CACHE/$d/"
        if [[ -d "$src" ]]; then
            run_or_dry rsync $RSYNC_OPTS --update "$src" \
                "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/$d/" 2>/dev/null && \
                ok "Synced $d/ (push)" || true
        fi
    done

    # Project memories push
    if [[ -d "$SYNC_CACHE/projects" ]]; then
        local mem_projects
        mem_projects=$(discover_project_memories "$SYNC_CACHE")
        for encoded in $mem_projects; do
            local src="$SYNC_CACHE/projects/$encoded/memory/"
            run_or_dry remote_cmd "mkdir -p '$REMOTE_BASE/projects/$encoded/memory'"
            run_or_dry rsync $RSYNC_OPTS --update "$src" \
                "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/projects/$encoded/memory/" 2>/dev/null && \
                ok "Synced projects/$encoded/memory/ (push)" || true
        done
    fi

    # Pull second (remote wins for files changed only remotely)
    log "Phase 2: Pulling remote changes..."
    for f in "${SYNC_FILES[@]}"; do
        run_or_dry rsync $RSYNC_OPTS --update --backup --suffix=".bak" \
            "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/$f" "$SYNC_CACHE/$f" 2>/dev/null && \
            ok "Synced $f (pull)" || true
    done

    for d in "${SYNC_DIRS[@]}"; do
        run_or_dry rsync $RSYNC_OPTS --update --backup --suffix=".bak" \
            "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/$d/" "$SYNC_CACHE/$d/" 2>/dev/null && \
            ok "Synced $d/ (pull)" || true
    done

    # Project memories pull
    local remote_projects
    remote_projects=$(remote_cmd "ls '$REMOTE_BASE/projects/' 2>/dev/null" || true)
    for encoded in $remote_projects; do
        if remote_cmd "test -d '$REMOTE_BASE/projects/$encoded/memory'" 2>/dev/null; then
            mkdir -p "$SYNC_CACHE/projects/$encoded"
            run_or_dry rsync $RSYNC_OPTS --update --backup --suffix=".bak" \
                "$REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/projects/$encoded/memory/" \
                "$SYNC_CACHE/projects/$encoded/memory/" 2>/dev/null && \
                ok "Synced projects/$encoded/memory/ (pull)" || true
        fi
    done

    # Update remote metadata
    run_or_dry remote_cmd "
        echo '{\"last_sync_by\": \"$WORKSTATION_ID\", \"timestamp\": \"$(timestamp)\"}' \
            > '$REMOTE_BASE/.sync-meta.json'
    "

    save_state
    ok "Sync complete"
}

# ═══════════════════════════════════════════════════════════════════════
# STATUS — Show sync state
# ═══════════════════════════════════════════════════════════════════════
cmd_status() {
    log "Sync status for workstation: ${BOLD}$WORKSTATION_ID${NC}"
    echo ""

    local state
    state=$(load_state)
    local last_sync
    last_sync=$(echo "$state" | jq -r '.last_sync // "never"')
    echo -e "  Last sync: ${BOLD}$last_sync${NC}"
    echo -e "  Cache dir: $SYNC_CACHE"
    echo -e "  Remote:    $REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE"
    echo ""

    # Check symlink status
    printf "  ${BOLD}%-30s %-12s %-10s${NC}\n" "FILE" "SYMLINK" "STATE"
    printf "  %-30s %-12s %-10s\n" "──────────────────────────────" "────────────" "──────────"

    for f in "${SYNC_FILES[@]}"; do
        local link="$CLAUDE_DIR/$f"
        local cache="$SYNC_CACHE/$f"
        local sym_status="missing"
        local sync_state="unknown"

        if [[ -L "$link" ]]; then
            sym_status="${GREEN}linked${NC}"
        elif [[ -f "$link" ]]; then
            sym_status="${YELLOW}file${NC}"
        else
            sym_status="${RED}absent${NC}"
        fi

        if [[ -f "$cache" ]]; then
            local current_hash
            current_hash=$(compute_hash "$cache")
            local saved_hash
            saved_hash=$(echo "$state" | jq -r --arg k "$f" '.files[$k] // "none"')
            if [[ "$current_hash" == "$saved_hash" ]]; then
                sync_state="${GREEN}in-sync${NC}"
            else
                sync_state="${YELLOW}changed${NC}"
            fi
        else
            sync_state="${RED}missing${NC}"
        fi

        printf "  %-30s %-24s %-22s\n" "$f" "$sym_status" "$sync_state"
    done

    for d in "${SYNC_DIRS[@]}"; do
        local link="$CLAUDE_DIR/$d"
        local cache="$SYNC_CACHE/$d"
        local sym_status="missing"
        local file_count=0

        if [[ -L "$link" ]]; then
            sym_status="${GREEN}linked${NC}"
        elif [[ -d "$link" ]]; then
            sym_status="${YELLOW}dir${NC}"
        else
            sym_status="${RED}absent${NC}"
        fi

        if [[ -d "$cache" ]]; then
            file_count=$(find "$cache" -type f | wc -l)
        fi

        printf "  %-30s %-24s %s files\n" "$d/" "$sym_status" "$file_count"
    done

    # Project memories
    if [[ -d "$SYNC_CACHE/projects" ]]; then
        local mem_projects
        mem_projects=$(discover_project_memories "$SYNC_CACHE")
        for encoded in $mem_projects; do
            local link="$CLAUDE_DIR/projects/$encoded/memory"
            local cache="$SYNC_CACHE/projects/$encoded/memory"
            local sym_status="missing"
            local file_count=0

            if [[ -L "$link" ]]; then
                sym_status="${GREEN}linked${NC}"
            elif [[ -d "$link" ]]; then
                sym_status="${YELLOW}dir${NC}"
            fi

            if [[ -d "$cache" ]]; then
                file_count=$(find "$cache" -type f | wc -l)
            fi

            # Truncate long encoded names
            local display="${encoded:0:28}"
            printf "  %-30s %-24s %s files\n" "proj:$display" "$sym_status" "$file_count"
        done
    fi

    echo ""

    # Remote connectivity
    if check_remote 2>/dev/null; then
        local remote_meta
        remote_meta=$(remote_cmd "cat '$REMOTE_BASE/.sync-meta.json' 2>/dev/null" || echo '{}')
        local last_push_by
        last_push_by=$(echo "$remote_meta" | jq -r '.last_push_by // .last_sync_by // "unknown"')
        local remote_ts
        remote_ts=$(echo "$remote_meta" | jq -r '.timestamp // "unknown"')
        echo -e "  Remote last updated by: ${BOLD}$last_push_by${NC} at $remote_ts"
    else
        warn "Remote unreachable — cannot check remote state"
    fi
}

# ═══════════════════════════════════════════════════════════════════════
# CONSOLIDATE — Merge memory from another workstation
# ═══════════════════════════════════════════════════════════════════════
cmd_consolidate() {
    if [[ -z "$CONSOLIDATE_FROM" ]]; then
        err "Usage: $(basename "$0") consolidate --from /path/to/export"
        exit 1
    fi

    log "Consolidating memory from: ${BOLD}$CONSOLIDATE_FROM${NC}"
    echo ""

    local source_dir="$CONSOLIDATE_FROM"

    # If source is remote (user@host:path), rsync it to a temp dir first
    if [[ "$source_dir" == *:* ]]; then
        local tmp_dir
        tmp_dir=$(mktemp -d)
        log "Fetching remote export to $tmp_dir ..."
        rsync $RSYNC_OPTS "$source_dir/" "$tmp_dir/"
        source_dir="$tmp_dir"
        trap "rm -rf '$tmp_dir'" EXIT
    fi

    if [[ ! -d "$source_dir" ]]; then
        err "Source directory not found: $source_dir"
        exit 1
    fi

    local merged=0
    local copied=0
    local skipped=0

    # Consolidate global files (CLAUDE.md, settings.json)
    for f in "${SYNC_FILES[@]}"; do
        local src="$source_dir/$f"
        local dst="$SYNC_CACHE/$f"
        if [[ ! -f "$src" ]]; then
            continue
        fi
        if [[ ! -f "$dst" ]]; then
            cp "$src" "$dst"
            ok "$f: copied from source"
            ((copied++))
            continue
        fi
        local src_hash dst_hash
        src_hash=$(compute_hash "$src")
        dst_hash=$(compute_hash "$dst")
        if [[ "$src_hash" == "$dst_hash" ]]; then
            ok "$f: identical — skipped"
            ((skipped++))
        else
            warn "$f: CONFLICT — source differs from local"
            cp "$src" "${dst}.incoming"
            warn "  Saved source version as ${dst}.incoming — resolve manually"
            ((merged++))
        fi
    done

    # Consolidate agent definitions
    if [[ -d "$source_dir/agents" ]]; then
        for agent_file in "$source_dir"/agents/*.md; do
            [[ -f "$agent_file" ]] || continue
            local name
            name=$(basename "$agent_file")
            local dst="$SYNC_CACHE/agents/$name"
            if [[ ! -f "$dst" ]]; then
                cp "$agent_file" "$dst"
                ok "agents/$name: copied from source"
                ((copied++))
            else
                local src_hash dst_hash
                src_hash=$(compute_hash "$agent_file")
                dst_hash=$(compute_hash "$dst")
                if [[ "$src_hash" == "$dst_hash" ]]; then
                    ((skipped++))
                else
                    cp "$agent_file" "${dst}.incoming"
                    warn "agents/$name: CONFLICT — saved as .incoming"
                    ((merged++))
                fi
            fi
        done
    fi

    # Consolidate memory directories
    log "Merging memory files..."
    local mem_dirs=()
    if [[ -d "$source_dir/projects" ]]; then
        while IFS= read -r -d '' mem_dir; do
            mem_dirs+=("$mem_dir")
        done < <(find "$source_dir/projects" -type d -name "memory" -print0)
    fi
    # Also check if source has memory/ at top level (flat export)
    if [[ -d "$source_dir/memory" ]]; then
        mem_dirs+=("$source_dir/memory")
    fi

    for mem_dir in "${mem_dirs[@]}"; do
        # Determine the target project path
        local rel="${mem_dir#"$source_dir/"}"
        local dst_mem="$SYNC_CACHE/$rel"
        mkdir -p "$dst_mem"

        for md_file in "$mem_dir"/*.md; do
            [[ -f "$md_file" ]] || continue
            local name
            name=$(basename "$md_file")
            local dst_file="$dst_mem/$name"

            if [[ ! -f "$dst_file" ]]; then
                cp "$md_file" "$dst_file"
                ok "$rel/$name: copied from source"
                ((copied++))
                continue
            fi

            local src_hash dst_hash
            src_hash=$(compute_hash "$md_file")
            dst_hash=$(compute_hash "$dst_file")

            if [[ "$src_hash" == "$dst_hash" ]]; then
                ((skipped++))
                continue
            fi

            # Auto-merge by section headers for .md files
            if [[ "$name" == "MEMORY.md" ]]; then
                # MEMORY.md is a line-based index — merge unique lines
                local tmp_merged
                tmp_merged=$(mktemp)
                sort -u "$dst_file" "$md_file" > "$tmp_merged"
                cp "$dst_file" "${dst_file}.pre-merge"
                mv "$tmp_merged" "$dst_file"
                ok "$rel/$name: merged (line-based dedup)"
                ((merged++))
            else
                # Other memory files — section-based merge
                # Append non-duplicate content with merge marker
                local tmp_merged
                tmp_merged=$(mktemp)
                cp "$dst_file" "$tmp_merged"

                # Check if the source has content not in destination
                local has_diff=false
                if ! diff -q "$dst_file" "$md_file" &>/dev/null; then
                    has_diff=true
                fi

                if $has_diff; then
                    {
                        echo ""
                        echo "<!-- merged from $CONSOLIDATE_FROM on $(timestamp) -->"
                        # Extract lines from source not present in destination
                        comm -23 <(sort "$md_file") <(sort "$dst_file")
                    } >> "$tmp_merged"
                    cp "$dst_file" "${dst_file}.pre-merge"
                    mv "$tmp_merged" "$dst_file"
                    ok "$rel/$name: merged with conflict markers"
                    ((merged++))
                else
                    rm "$tmp_merged"
                    ((skipped++))
                fi
            fi
        done
    done

    echo ""
    log "Consolidation summary: ${GREEN}$copied copied${NC}, ${YELLOW}$merged merged${NC}, $skipped skipped"

    if [[ $((copied + merged)) -gt 0 ]]; then
        echo ""
        log "Pushing consolidated result to remote..."
        cmd_push
    fi
}

# ═══════════════════════════════════════════════════════════════════════
# BOOTSTRAP — Fresh workstation setup from remote
# ═══════════════════════════════════════════════════════════════════════
cmd_bootstrap() {
    log "Bootstrapping from $REMOTE_USER@$REMOTE_HOST:$REMOTE_BASE/"
    echo ""

    # 1. Check remote
    if ! check_remote; then
        exit 1
    fi

    # Verify remote has config
    if ! remote_cmd "test -d '$REMOTE_BASE/agents'" 2>/dev/null; then
        err "No config found on remote. Run 'init' on another workstation first."
        exit 1
    fi
    ok "Remote config found"

    # 2. Create local dirs
    mkdir -p "$CLAUDE_DIR"
    mkdir -p "$SYNC_CACHE"/{agents,teams,projects}
    mkdir -p "$STATE_DIR"

    # 3. Pull everything
    log "Pulling config from remote..."
    cmd_pull

    # 4. Create symlinks
    log "Creating symlinks..."

    for f in "${SYNC_FILES[@]}"; do
        local cache="$SYNC_CACHE/$f"
        local link="$CLAUDE_DIR/$f"
        if [[ -f "$cache" ]]; then
            # Back up existing local file if present and not a symlink
            if [[ -f "$link" && ! -L "$link" ]]; then
                mv "$link" "${link}.pre-bootstrap"
                warn "$f: backed up existing to ${f}.pre-bootstrap"
            fi
            make_symlink ".sync-cache/$f" "$link"
            ok "$f: symlinked"
        fi
    done

    for d in "${SYNC_DIRS[@]}"; do
        local cache="$SYNC_CACHE/$d"
        local link="$CLAUDE_DIR/$d"
        if [[ -d "$cache" ]]; then
            if [[ -d "$link" && ! -L "$link" ]]; then
                mv "$link" "${link}.pre-bootstrap"
                warn "$d/: backed up existing to ${d}.pre-bootstrap"
            fi
            make_symlink ".sync-cache/$d" "$link"
            ok "$d/: symlinked"
        fi
    done

    # Project memory symlinks
    if [[ -d "$SYNC_CACHE/projects" ]]; then
        local mem_projects
        mem_projects=$(discover_project_memories "$SYNC_CACHE")
        for encoded in $mem_projects; do
            local proj_dir="$CLAUDE_DIR/projects/$encoded"
            local link="$proj_dir/memory"
            local cache="$SYNC_CACHE/projects/$encoded/memory"

            mkdir -p "$proj_dir"

            if [[ -d "$link" && ! -L "$link" ]]; then
                mv "$link" "${link}.pre-bootstrap"
                warn "projects/$encoded/memory: backed up existing"
            fi

            make_symlink "../../.sync-cache/projects/$encoded/memory" "$link"
            ok "projects/$encoded/memory: symlinked"
        done
    fi

    save_state

    echo ""
    log "Bootstrap complete!"
    log "Config synced from remote and symlinked locally."
    log "Run '$(basename "$0") sync' periodically or install the systemd timer."
}

# ═══════════════════════════════════════════════════════════════════════
# ROLLBACK — Restore from remote backup
# ═══════════════════════════════════════════════════════════════════════
cmd_rollback() {
    local target_ts="${1:-}"

    if ! check_remote; then
        exit 1
    fi

    # List available backups
    local backups
    backups=$(remote_cmd "ls -1 '$BACKUP_DIR_REMOTE/' 2>/dev/null" || true)

    if [[ -z "$backups" ]]; then
        warn "No backups found on remote"
        return 0
    fi

    if [[ -z "$target_ts" ]]; then
        log "Available backups on remote:"
        echo ""
        echo "$backups" | while read -r ts; do
            echo "  $ts"
        done
        echo ""
        log "Usage: $(basename "$0") rollback <TIMESTAMP>"
        return 0
    fi

    # Verify the requested backup exists
    if ! remote_cmd "test -d '$BACKUP_DIR_REMOTE/$target_ts'" 2>/dev/null; then
        err "Backup not found: $target_ts"
        return 1
    fi

    log "Rolling back to: ${BOLD}$target_ts${NC}"

    # Pull backup to local cache
    for f in "${SYNC_FILES[@]}"; do
        if run_or_dry rsync $RSYNC_OPTS \
            "$REMOTE_USER@$REMOTE_HOST:$BACKUP_DIR_REMOTE/$target_ts/$f" \
            "$SYNC_CACHE/$f" 2>/dev/null; then
            ok "Restored $f"
        fi
    done

    for d in "${SYNC_DIRS[@]}"; do
        if run_or_dry rsync $RSYNC_OPTS \
            "$REMOTE_USER@$REMOTE_HOST:$BACKUP_DIR_REMOTE/$target_ts/$d/" \
            "$SYNC_CACHE/$d/" 2>/dev/null; then
            ok "Restored $d/"
        fi
    done

    # Also restore to remote current
    log "Pushing rolled-back state to remote current..."
    cmd_push

    ok "Rollback to $target_ts complete"
}

# ═══════════════════════════════════════════════════════════════════════
# Argument Parsing
# ═══════════════════════════════════════════════════════════════════════
CMD=""
ROLLBACK_TS=""

while [[ $# -gt 0 ]]; do
    case $1 in
        init|push|pull|sync|status|consolidate|bootstrap|rollback)
            CMD="$1"
            shift
            # Capture rollback timestamp if present
            if [[ "$CMD" == "rollback" && $# -gt 0 && "$1" != --* ]]; then
                ROLLBACK_TS="$1"
                shift
            fi
            ;;
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --force)
            FORCE=true
            shift
            ;;
        --workstation-id)
            WORKSTATION_ID="$2"
            shift 2
            ;;
        --from)
            CONSOLIDATE_FROM="$2"
            shift 2
            ;;
        -h|--help)
            usage
            ;;
        *)
            err "Unknown option: $1"
            usage
            ;;
    esac
done

if [[ -z "$CMD" ]]; then
    usage
fi

# ═══════════════════════════════════════════════════════════════════════
# Dispatch
# ═══════════════════════════════════════════════════════════════════════
case "$CMD" in
    init)        cmd_init ;;
    push)        cmd_push ;;
    pull)        cmd_pull ;;
    sync)        cmd_sync ;;
    status)      cmd_status ;;
    consolidate) cmd_consolidate ;;
    bootstrap)   cmd_bootstrap ;;
    rollback)    cmd_rollback "$ROLLBACK_TS" ;;
esac
