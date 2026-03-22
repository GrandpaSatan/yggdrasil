#Requires -Version 5.1
<#
.SYNOPSIS
    claude-config-sync.ps1 — Centralize Claude Code config on Munin (Windows)

.DESCRIPTION
    Syncs Claude Code configuration (agents, memory, hooks, settings) to a
    central server via SSH/SCP and manages local symlinks to a sync cache.

    Companion to claude-config-sync.sh (Linux). Same remote layout, same
    sync protocol, cross-platform compatible.

.PARAMETER Command
    Subcommand: init, push, pull, sync, status, consolidate, bootstrap,
    rollback, export

.PARAMETER DryRun
    Show what would happen without making changes.

.PARAMETER Force
    Overwrite without conflict checks.

.PARAMETER WorkstationId
    Override hostname-based workstation identifier.

.PARAMETER From
    Source path for consolidate (local dir or user@host:path).

.PARAMETER Timestamp
    Backup timestamp for rollback.

.EXAMPLE
    .\claude-config-sync.ps1 init
    # First-time setup: move files, create symlinks, push to Munin

.EXAMPLE
    .\claude-config-sync.ps1 sync
    # Bidirectional sync

.EXAMPLE
    .\claude-config-sync.ps1 export -OutputPath C:\temp\claude-export
    # Export config for consolidation on another workstation

.EXAMPLE
    .\claude-config-sync.ps1 bootstrap
    # Set up fresh workstation from remote
#>

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet("init", "push", "pull", "sync", "status", "consolidate",
                 "bootstrap", "rollback", "export", "help")]
    [string]$Command = "help",

    [switch]$DryRun,
    [switch]$Force,
    [string]$WorkstationId,
    [string]$From,
    [string]$Timestamp,
    [string]$OutputPath
)

$ErrorActionPreference = "Stop"

# ── Constants ───────────────────────────────────────────────────────────
if (-not $env:MUNIN_IP) { Write-Host "  x MUNIN_IP not set. Export it or add to .env" -ForegroundColor Red; exit 1 }
if (-not $env:DEPLOY_USER) { Write-Host "  x DEPLOY_USER not set. Export it or add to .env" -ForegroundColor Red; exit 1 }
$REMOTE_HOST    = $env:MUNIN_IP
$REMOTE_USER    = $env:DEPLOY_USER
$REMOTE_BASE    = "/opt/yggdrasil/claude-config"
$HOME_DIR       = $env:USERPROFILE
$CLAUDE_DIR     = Join-Path $HOME_DIR ".claude"
$SYNC_CACHE     = Join-Path $CLAUDE_DIR ".sync-cache"
$STATE_DIR      = Join-Path $HOME_DIR ".config\yggdrasil"
$STATE_FILE     = Join-Path $STATE_DIR "claude-sync-state.json"
$BACKUP_REMOTE  = "$REMOTE_BASE/.backups"

$SSH_OPTS = @("-o", "ConnectTimeout=3", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new")

# Files to sync (relative to ~/.claude/)
$SYNC_FILES = @("CLAUDE.md", "settings.json")
# Directories to sync
$SYNC_DIRS  = @("agents", "teams")

if (-not $WorkstationId) {
    $WorkstationId = $env:COMPUTERNAME
}

# ── Logging ─────────────────────────────────────────────────────────────
function Log  ($msg) { Write-Host "[sync] $msg" -ForegroundColor Cyan }
function Ok   ($msg) { Write-Host "  + $msg" -ForegroundColor Green }
function Warn ($msg) { Write-Host "  ! $msg" -ForegroundColor Yellow }
function Err  ($msg) { Write-Host "  x $msg" -ForegroundColor Red }

# ── Helpers ─────────────────────────────────────────────────────────────

function Test-RemoteSSH {
    try {
        $result = & ssh @SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" "true" 2>&1
        return $LASTEXITCODE -eq 0
    } catch {
        return $false
    }
}

function Invoke-Remote {
    param([string]$Cmd)
    & ssh @SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" $Cmd
    if ($LASTEXITCODE -ne 0) {
        throw "Remote command failed: $Cmd"
    }
}

function Get-FileHashString {
    param([string]$Path)
    if (Test-Path $Path -PathType Leaf) {
        return (Get-FileHash -Path $Path -Algorithm SHA256).Hash.ToLower()
    }
    return "missing"
}

function Get-DirHashString {
    param([string]$Path)
    if (-not (Test-Path $Path -PathType Container)) { return "missing" }
    $hashes = Get-ChildItem -Path $Path -File -Recurse | Sort-Object FullName | ForEach-Object {
        (Get-FileHash -Path $_.FullName -Algorithm SHA256).Hash
    }
    if ($hashes.Count -eq 0) { return "empty" }
    $combined = $hashes -join ""
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($combined)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $hash = $sha.ComputeHash($bytes)
    return ($hash | ForEach-Object { $_.ToString("x2") }) -join ""
}

function Invoke-RunOrDry {
    param([string]$Description, [scriptblock]$Action)
    if ($DryRun) {
        Write-Host "  [dry-run] $Description" -ForegroundColor Yellow
    } else {
        & $Action
    }
}

function Get-Timestamp {
    return (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
}

function Save-SyncState {
    if (-not (Test-Path $STATE_DIR)) {
        New-Item -ItemType Directory -Path $STATE_DIR -Force | Out-Null
    }
    $files = @{}

    foreach ($f in $SYNC_FILES) {
        $fp = Join-Path $SYNC_CACHE $f
        if (Test-Path $fp) {
            $files[$f] = Get-FileHashString $fp
        }
    }

    foreach ($d in $SYNC_DIRS) {
        $dp = Join-Path $SYNC_CACHE $d
        if (Test-Path $dp) {
            Get-ChildItem -Path $dp -File -Recurse | ForEach-Object {
                $rel = $_.FullName.Substring($SYNC_CACHE.Length + 1) -replace '\\', '/'
                $files[$rel] = Get-FileHashString $_.FullName
            }
        }
    }

    $projDir = Join-Path $SYNC_CACHE "projects"
    if (Test-Path $projDir) {
        Get-ChildItem -Path $projDir -File -Recurse | ForEach-Object {
            $rel = $_.FullName.Substring($SYNC_CACHE.Length + 1) -replace '\\', '/'
            $files[$rel] = Get-FileHashString $_.FullName
        }
    }

    $state = @{
        workstation_id = $WorkstationId
        last_sync      = Get-Timestamp
        files          = $files
    }

    $state | ConvertTo-Json -Depth 5 | Set-Content -Path $STATE_FILE -Encoding UTF8
}

function Get-SyncState {
    if (Test-Path $STATE_FILE) {
        return Get-Content -Path $STATE_FILE -Raw | ConvertFrom-Json
    }
    return @{ workstation_id = ""; last_sync = ""; files = @{} }
}

function Get-ProjectMemories {
    param([string]$BasePath)
    $results = @()
    $projPath = Join-Path $BasePath "projects"
    if (-not (Test-Path $projPath)) { return $results }

    Get-ChildItem -Path $projPath -Directory | ForEach-Object {
        $memDir = Join-Path $_.FullName "memory"
        if ((Test-Path $memDir) -and (Get-ChildItem -Path $memDir -Filter "*.md" -ErrorAction SilentlyContinue)) {
            $results += $_.Name
        }
    }
    return $results | Sort-Object -Unique
}

function New-Symlink {
    param([string]$Target, [string]$Link)

    if (Test-Path $Link) {
        $item = Get-Item $Link -Force
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            # Already a symlink
            $current = $item.Target
            if ($current -eq $Target) { return }
            Invoke-RunOrDry "Remove existing symlink $Link" {
                Remove-Item $Link -Force
            }
        } else {
            Err "$Link exists and is not a symlink - move it first"
            return
        }
    }

    Invoke-RunOrDry "Create symlink $Link -> $Target" {
        New-Item -ItemType SymbolicLink -Path $Link -Target $Target -Force | Out-Null
    }
}

# SCP-based sync (Windows doesn't have rsync natively)
function Push-FileToRemote {
    param([string]$LocalPath, [string]$RemotePath)
    if (-not (Test-Path $LocalPath)) { return }
    & scp -q @SSH_OPTS $LocalPath "${REMOTE_USER}@${REMOTE_HOST}:${RemotePath}" 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { Warn "Failed to push $LocalPath" }
}

function Pull-FileFromRemote {
    param([string]$RemotePath, [string]$LocalPath)
    $dir = Split-Path -Parent $LocalPath
    if (-not (Test-Path $dir)) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
    }
    & scp -q @SSH_OPTS "${REMOTE_USER}@${REMOTE_HOST}:${RemotePath}" $LocalPath 2>&1 | Out-Null
    return $LASTEXITCODE -eq 0
}

function Push-DirToRemote {
    param([string]$LocalDir, [string]$RemoteDir)
    if (-not (Test-Path $LocalDir)) { return }
    Invoke-Remote "mkdir -p '$RemoteDir'"
    Get-ChildItem -Path $LocalDir -File -Recurse | ForEach-Object {
        $rel = $_.FullName.Substring($LocalDir.Length + 1) -replace '\\', '/'
        $remoteFile = "$RemoteDir/$rel"
        $remoteParent = $remoteFile -replace '/[^/]+$', ''
        Invoke-Remote "mkdir -p '$remoteParent'" 2>$null
        Push-FileToRemote $_.FullName $remoteFile
    }
}

function Pull-DirFromRemote {
    param([string]$RemoteDir, [string]$LocalDir)
    if (-not (Test-Path $LocalDir)) {
        New-Item -ItemType Directory -Path $LocalDir -Force | Out-Null
    }
    # List remote files and pull each
    try {
        $files = & ssh @SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" "find '$RemoteDir' -type f -printf '%P\n' 2>/dev/null" 2>&1
        if ($LASTEXITCODE -eq 0 -and $files) {
            foreach ($rel in ($files -split "`n" | Where-Object { $_ })) {
                $localFile = Join-Path $LocalDir ($rel -replace '/', '\')
                Pull-FileFromRemote "$RemoteDir/$rel" $localFile
            }
        }
    } catch {
        Warn "Could not list remote dir $RemoteDir"
    }
}

# ═══════════════════════════════════════════════════════════════════════
# INIT
# ═══════════════════════════════════════════════════════════════════════
function Invoke-Init {
    Log "Initializing config sync for workstation: $WorkstationId"
    Write-Host ""

    Log "Checking remote connectivity..."
    if (-not (Test-RemoteSSH)) {
        Err "Cannot reach $REMOTE_USER@$REMOTE_HOST"
        Err "Ensure OpenSSH is installed and SSH key auth is configured"
        exit 1
    }
    Ok "Remote reachable"

    Log "Creating remote directory structure..."
    Invoke-RunOrDry "Create remote dirs" {
        Invoke-Remote "mkdir -p '$REMOTE_BASE'/{agents,teams,projects,project-configs,.backups}"
    }
    Ok "Remote dirs: $REMOTE_BASE/"

    Log "Setting up local sync cache..."
    foreach ($sub in @("agents", "teams", "projects")) {
        $p = Join-Path $SYNC_CACHE $sub
        if (-not (Test-Path $p)) {
            New-Item -ItemType Directory -Path $p -Force | Out-Null
        }
    }
    Ok "Cache: $SYNC_CACHE"

    Log "Moving files to sync cache..."

    # Individual files
    foreach ($f in $SYNC_FILES) {
        $src = Join-Path $CLAUDE_DIR $f
        $dst = Join-Path $SYNC_CACHE $f

        if ((Test-Path $src) -and ((Get-Item $src -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            Ok "${f}: already symlinked"
            continue
        }
        if (Test-Path $src -PathType Leaf) {
            Invoke-RunOrDry "Move $f to cache" {
                Move-Item -Path $src -Destination $dst -Force
            }
            New-Symlink -Target (Join-Path ".sync-cache" $f) -Link $src
            Ok "${f}: moved + symlinked"
        } else {
            Warn "${f}: not found, skipping"
        }
    }

    # Directories
    foreach ($d in $SYNC_DIRS) {
        $src = Join-Path $CLAUDE_DIR $d
        $dst = Join-Path $SYNC_CACHE $d

        if ((Test-Path $src) -and ((Get-Item $src -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            Ok "${d}/: already symlinked"
            continue
        }
        if (Test-Path $src -PathType Container) {
            if ((Test-Path $dst) -and (Get-ChildItem $dst -ErrorAction SilentlyContinue)) {
                Invoke-RunOrDry "Merge $d into cache" {
                    Copy-Item -Path "$src\*" -Destination $dst -Recurse -Force
                    Remove-Item -Path $src -Recurse -Force
                }
            } else {
                Invoke-RunOrDry "Move $d to cache" {
                    if (Test-Path $dst) { Remove-Item $dst -Recurse -Force }
                    Move-Item -Path $src -Destination $dst -Force
                }
            }
            New-Symlink -Target (Join-Path ".sync-cache" $d) -Link $src
            Ok "${d}/: moved + symlinked"
        } else {
            if (-not (Test-Path $dst)) {
                New-Item -ItemType Directory -Path $dst -Force | Out-Null
            }
            New-Symlink -Target (Join-Path ".sync-cache" $d) -Link $src
            Ok "${d}/: created + symlinked"
        }
    }

    # Project memory directories
    Log "Processing project memory directories..."
    $memories = Get-ProjectMemories $CLAUDE_DIR
    foreach ($encoded in $memories) {
        $srcMem = Join-Path $CLAUDE_DIR "projects\$encoded\memory"
        $dstMem = Join-Path $SYNC_CACHE "projects\$encoded\memory"

        if ((Test-Path $srcMem) -and ((Get-Item $srcMem -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            Ok "projects/$encoded/memory: already symlinked"
            continue
        }

        $dstParent = Join-Path $SYNC_CACHE "projects\$encoded"
        if (-not (Test-Path $dstParent)) {
            New-Item -ItemType Directory -Path $dstParent -Force | Out-Null
        }

        if (Test-Path $srcMem -PathType Container) {
            Invoke-RunOrDry "Move project memory $encoded" {
                Move-Item -Path $srcMem -Destination $dstMem -Force
            }
            # Relative path: ../../.sync-cache/projects/ENCODED/memory
            New-Symlink -Target "..\..\..sync-cache\projects\$encoded\memory" -Link $srcMem
            Ok "projects/$encoded/memory: moved + symlinked"
        }
    }

    Write-Host ""
    Invoke-Push
    Save-SyncState
    Ok "Sync state saved"

    Write-Host ""
    Log "Init complete! Files are in $SYNC_CACHE and symlinked."
}

# ═══════════════════════════════════════════════════════════════════════
# PUSH
# ═══════════════════════════════════════════════════════════════════════
function Invoke-Push {
    Log "Pushing to ${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_BASE}/ ..."

    if (-not (Test-RemoteSSH)) {
        Warn "Remote unreachable - skipping push"
        return
    }

    # Backup on remote
    $ts = (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmssZ")
    try {
        Invoke-Remote "if [ -d '$REMOTE_BASE/agents' ] || [ -f '$REMOTE_BASE/CLAUDE.md' ]; then mkdir -p '$BACKUP_REMOTE/$ts' && cp -a '$REMOTE_BASE'/CLAUDE.md '$REMOTE_BASE'/settings.json '$BACKUP_REMOTE/$ts/' 2>/dev/null; cp -a '$REMOTE_BASE'/agents '$BACKUP_REMOTE/$ts/agents' 2>/dev/null; cp -a '$REMOTE_BASE'/projects '$BACKUP_REMOTE/$ts/projects' 2>/dev/null; fi"
    } catch {
        Warn "Could not create remote backup (first push?)"
    }

    # Push files
    foreach ($f in $SYNC_FILES) {
        $src = Join-Path $SYNC_CACHE $f
        if (Test-Path $src) {
            Invoke-RunOrDry "Push $f" { Push-FileToRemote $src "$REMOTE_BASE/$f" }
            Ok "Pushed $f"
        }
    }

    # Push directories
    foreach ($d in $SYNC_DIRS) {
        $src = Join-Path $SYNC_CACHE $d
        if (Test-Path $src) {
            Invoke-RunOrDry "Push $d/" { Push-DirToRemote $src "$REMOTE_BASE/$d" }
            Ok "Pushed $d/"
        }
    }

    # Push project memories
    $memories = Get-ProjectMemories $SYNC_CACHE
    foreach ($encoded in $memories) {
        $src = Join-Path $SYNC_CACHE "projects\$encoded\memory"
        Invoke-RunOrDry "Push projects/$encoded/memory/" {
            Push-DirToRemote $src "$REMOTE_BASE/projects/$encoded/memory"
        }
        Ok "Pushed projects/$encoded/memory/"
    }

    # Update remote metadata
    try {
        Invoke-Remote "echo '{`"last_push_by`": `"$WorkstationId`", `"timestamp`": `"$(Get-Timestamp)`"}' > '$REMOTE_BASE/.sync-meta.json'"
    } catch { }

    Save-SyncState
    Ok "Push complete"
}

# ═══════════════════════════════════════════════════════════════════════
# PULL
# ═══════════════════════════════════════════════════════════════════════
function Invoke-Pull {
    Log "Pulling from ${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_BASE}/ ..."

    if (-not (Test-RemoteSSH)) {
        Warn "Remote unreachable - skipping pull"
        return
    }

    foreach ($sub in @("agents", "teams", "projects")) {
        $p = Join-Path $SYNC_CACHE $sub
        if (-not (Test-Path $p)) {
            New-Item -ItemType Directory -Path $p -Force | Out-Null
        }
    }

    # Pull files
    foreach ($f in $SYNC_FILES) {
        $dst = Join-Path $SYNC_CACHE $f
        if (Test-Path $dst) {
            # Backup before overwrite
            Copy-Item $dst "${dst}.bak" -Force -ErrorAction SilentlyContinue
        }
        if (Pull-FileFromRemote "$REMOTE_BASE/$f" $dst) {
            Ok "Pulled $f"
        } else {
            Warn "${f}: not found on remote"
        }
    }

    # Pull directories
    foreach ($d in $SYNC_DIRS) {
        $dst = Join-Path $SYNC_CACHE $d
        Invoke-RunOrDry "Pull $d/" { Pull-DirFromRemote "$REMOTE_BASE/$d" $dst }
        Ok "Pulled $d/"
    }

    # Pull project memories
    try {
        $remoteProjList = & ssh @SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" "ls '$REMOTE_BASE/projects/' 2>/dev/null" 2>&1
        if ($LASTEXITCODE -eq 0 -and $remoteProjList) {
            foreach ($encoded in ($remoteProjList -split "`n" | Where-Object { $_ })) {
                $hasMemory = & ssh @SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" "test -d '$REMOTE_BASE/projects/$encoded/memory' && echo yes" 2>&1
                if ($hasMemory -eq "yes") {
                    $dst = Join-Path $SYNC_CACHE "projects\$encoded\memory"
                    Pull-DirFromRemote "$REMOTE_BASE/projects/$encoded/memory" $dst
                    Ok "Pulled projects/$encoded/memory/"
                }
            }
        }
    } catch {
        Warn "Could not list remote projects"
    }

    Save-SyncState
    Ok "Pull complete"
}

# ═══════════════════════════════════════════════════════════════════════
# SYNC
# ═══════════════════════════════════════════════════════════════════════
function Invoke-Sync {
    Log "Bidirectional sync..."

    if (-not (Test-RemoteSSH)) {
        Warn "Remote unreachable - skipping sync"
        return
    }

    Log "Phase 1: Pushing local changes..."
    Invoke-Push

    Log "Phase 2: Pulling remote changes..."
    Invoke-Pull

    Save-SyncState
    Ok "Sync complete"
}

# ═══════════════════════════════════════════════════════════════════════
# STATUS
# ═══════════════════════════════════════════════════════════════════════
function Invoke-Status {
    Log "Sync status for workstation: $WorkstationId"
    Write-Host ""

    $state = Get-SyncState
    $lastSync = if ($state.last_sync) { $state.last_sync } else { "never" }
    Write-Host "  Last sync: $lastSync"
    Write-Host "  Cache dir: $SYNC_CACHE"
    Write-Host "  Remote:    ${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_BASE}"
    Write-Host ""

    Write-Host ("  {0,-30} {1,-12} {2,-10}" -f "FILE", "SYMLINK", "STATE")
    Write-Host ("  {0,-30} {1,-12} {2,-10}" -f ("-" * 30), ("-" * 12), ("-" * 10))

    foreach ($f in $SYNC_FILES) {
        $link = Join-Path $CLAUDE_DIR $f
        $cache = Join-Path $SYNC_CACHE $f
        $symStatus = "missing"
        $syncState = "unknown"

        if ((Test-Path $link) -and ((Get-Item $link -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            $symStatus = "linked"
        } elseif (Test-Path $link -PathType Leaf) {
            $symStatus = "file"
        } else {
            $symStatus = "absent"
        }

        if (Test-Path $cache) {
            $currentHash = Get-FileHashString $cache
            $savedHash = if ($state.files.$f) { $state.files.$f } else { "none" }
            $syncState = if ($currentHash -eq $savedHash) { "in-sync" } else { "changed" }
        } else {
            $syncState = "missing"
        }

        Write-Host ("  {0,-30} {1,-12} {2,-10}" -f $f, $symStatus, $syncState)
    }

    foreach ($d in $SYNC_DIRS) {
        $link = Join-Path $CLAUDE_DIR $d
        $cache = Join-Path $SYNC_CACHE $d
        $symStatus = "missing"
        $fileCount = 0

        if ((Test-Path $link) -and ((Get-Item $link -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            $symStatus = "linked"
        } elseif (Test-Path $link -PathType Container) {
            $symStatus = "dir"
        }

        if (Test-Path $cache) {
            $fileCount = (Get-ChildItem -Path $cache -File -Recurse -ErrorAction SilentlyContinue).Count
        }

        Write-Host ("  {0,-30} {1,-12} {2} files" -f "$d/", $symStatus, $fileCount)
    }

    # Project memories
    $memories = Get-ProjectMemories $SYNC_CACHE
    foreach ($encoded in $memories) {
        $link = Join-Path $CLAUDE_DIR "projects\$encoded\memory"
        $cache = Join-Path $SYNC_CACHE "projects\$encoded\memory"
        $symStatus = "missing"
        $fileCount = 0

        if ((Test-Path $link) -and ((Get-Item $link -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            $symStatus = "linked"
        } elseif (Test-Path $link -PathType Container) {
            $symStatus = "dir"
        }

        if (Test-Path $cache) {
            $fileCount = (Get-ChildItem -Path $cache -File -Recurse -ErrorAction SilentlyContinue).Count
        }

        $display = if ($encoded.Length -gt 28) { $encoded.Substring(0, 28) } else { $encoded }
        Write-Host ("  {0,-30} {1,-12} {2} files" -f "proj:$display", $symStatus, $fileCount)
    }

    Write-Host ""
    if (Test-RemoteSSH) {
        try {
            $meta = & ssh @SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" "cat '$REMOTE_BASE/.sync-meta.json' 2>/dev/null" 2>&1
            if ($meta) {
                $metaObj = $meta | ConvertFrom-Json
                $lastBy = if ($metaObj.last_push_by) { $metaObj.last_push_by } elseif ($metaObj.last_sync_by) { $metaObj.last_sync_by } else { "unknown" }
                Write-Host "  Remote last updated by: $lastBy at $($metaObj.timestamp)"
            }
        } catch { }
    } else {
        Warn "Remote unreachable - cannot check remote state"
    }
}

# ═══════════════════════════════════════════════════════════════════════
# CONSOLIDATE
# ═══════════════════════════════════════════════════════════════════════
function Invoke-Consolidate {
    if (-not $From) {
        Err "Usage: claude-config-sync.ps1 consolidate -From C:\path\to\export"
        exit 1
    }

    Log "Consolidating from: $From"
    Write-Host ""

    $sourceDir = $From
    if (-not (Test-Path $sourceDir)) {
        Err "Source not found: $sourceDir"
        exit 1
    }

    $copied = 0; $merged = 0; $skipped = 0

    # Files
    foreach ($f in $SYNC_FILES) {
        $src = Join-Path $sourceDir $f
        $dst = Join-Path $SYNC_CACHE $f
        if (-not (Test-Path $src)) { continue }
        if (-not (Test-Path $dst)) {
            Copy-Item $src $dst -Force
            Ok "${f}: copied from source"
            $copied++
            continue
        }
        $srcH = Get-FileHashString $src
        $dstH = Get-FileHashString $dst
        if ($srcH -eq $dstH) {
            Ok "${f}: identical - skipped"
            $skipped++
        } else {
            Copy-Item $src "${dst}.incoming" -Force
            Warn "${f}: CONFLICT - saved as .incoming"
            $merged++
        }
    }

    # Agent defs
    $agentSrc = Join-Path $sourceDir "agents"
    if (Test-Path $agentSrc) {
        Get-ChildItem -Path $agentSrc -Filter "*.md" | ForEach-Object {
            $dst = Join-Path $SYNC_CACHE "agents\$($_.Name)"
            if (-not (Test-Path $dst)) {
                Copy-Item $_.FullName $dst
                Ok "agents/$($_.Name): copied"
                $script:copied++
            } else {
                $srcH = Get-FileHashString $_.FullName
                $dstH = Get-FileHashString $dst
                if ($srcH -eq $dstH) { $script:skipped++ }
                else {
                    Copy-Item $_.FullName "${dst}.incoming"
                    Warn "agents/$($_.Name): CONFLICT - saved as .incoming"
                    $script:merged++
                }
            }
        }
    }

    # Memory files
    Log "Merging memory files..."
    $memDirs = @()
    $projSrc = Join-Path $sourceDir "projects"
    if (Test-Path $projSrc) {
        Get-ChildItem -Path $projSrc -Directory -Recurse | Where-Object { $_.Name -eq "memory" } | ForEach-Object {
            $memDirs += $_.FullName
        }
    }
    $topMem = Join-Path $sourceDir "memory"
    if (Test-Path $topMem) { $memDirs += $topMem }

    foreach ($memDir in $memDirs) {
        $rel = $memDir.Substring($sourceDir.Length + 1) -replace '\\', '/'
        $dstMem = Join-Path $SYNC_CACHE $rel
        if (-not (Test-Path $dstMem)) {
            New-Item -ItemType Directory -Path $dstMem -Force | Out-Null
        }

        Get-ChildItem -Path $memDir -Filter "*.md" | ForEach-Object {
            $dstFile = Join-Path $dstMem $_.Name
            if (-not (Test-Path $dstFile)) {
                Copy-Item $_.FullName $dstFile
                Ok "$rel/$($_.Name): copied"
                $script:copied++
                return
            }
            $srcH = Get-FileHashString $_.FullName
            $dstH = Get-FileHashString $dstFile
            if ($srcH -eq $dstH) { $script:skipped++; return }

            if ($_.Name -eq "MEMORY.md") {
                # Line-based dedup merge
                $srcLines = Get-Content $_.FullName
                $dstLines = Get-Content $dstFile
                $allLines = ($srcLines + $dstLines) | Sort-Object -Unique
                Copy-Item $dstFile "${dstFile}.pre-merge" -Force
                Set-Content -Path $dstFile -Value $allLines -Encoding UTF8
                Ok "$rel/$($_.Name): merged (line dedup)"
                $script:merged++
            } else {
                Copy-Item $dstFile "${dstFile}.pre-merge" -Force
                $marker = "<!-- merged from $From on $(Get-Timestamp) -->"
                $srcContent = Get-Content $_.FullName -Raw
                Add-Content -Path $dstFile -Value "`n$marker`n$srcContent" -Encoding UTF8
                Ok "$rel/$($_.Name): merged with markers"
                $script:merged++
            }
        }
    }

    Write-Host ""
    Log "Summary: $copied copied, $merged merged, $skipped skipped"

    if (($copied + $merged) -gt 0) {
        Write-Host ""
        Log "Pushing consolidated result..."
        Invoke-Push
    }
}

# ═══════════════════════════════════════════════════════════════════════
# BOOTSTRAP
# ═══════════════════════════════════════════════════════════════════════
function Invoke-Bootstrap {
    Log "Bootstrapping from ${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_BASE}/"
    Write-Host ""

    if (-not (Test-RemoteSSH)) {
        Err "Cannot reach remote. Ensure OpenSSH + key auth are configured."
        exit 1
    }

    $hasConfig = & ssh @SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" "test -d '$REMOTE_BASE/agents' && echo yes" 2>&1
    if ($hasConfig -ne "yes") {
        Err "No config on remote. Run 'init' on another workstation first."
        exit 1
    }
    Ok "Remote config found"

    # Create dirs
    foreach ($sub in @("", "agents", "teams", "projects")) {
        $p = if ($sub) { Join-Path $SYNC_CACHE $sub } else { $SYNC_CACHE }
        if (-not (Test-Path $p)) {
            New-Item -ItemType Directory -Path $p -Force | Out-Null
        }
    }
    if (-not (Test-Path $STATE_DIR)) {
        New-Item -ItemType Directory -Path $STATE_DIR -Force | Out-Null
    }
    if (-not (Test-Path $CLAUDE_DIR)) {
        New-Item -ItemType Directory -Path $CLAUDE_DIR -Force | Out-Null
    }

    Invoke-Pull

    Log "Creating symlinks..."

    foreach ($f in $SYNC_FILES) {
        $cache = Join-Path $SYNC_CACHE $f
        $link = Join-Path $CLAUDE_DIR $f
        if (Test-Path $cache) {
            if ((Test-Path $link -PathType Leaf) -and -not ((Get-Item $link -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                Move-Item $link "${link}.pre-bootstrap" -Force
                Warn "${f}: backed up existing"
            }
            New-Symlink -Target (Join-Path ".sync-cache" $f) -Link $link
            Ok "${f}: symlinked"
        }
    }

    foreach ($d in $SYNC_DIRS) {
        $cache = Join-Path $SYNC_CACHE $d
        $link = Join-Path $CLAUDE_DIR $d
        if (Test-Path $cache) {
            if ((Test-Path $link -PathType Container) -and -not ((Get-Item $link -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                Move-Item $link "${link}.pre-bootstrap" -Force
                Warn "${d}/: backed up existing"
            }
            New-Symlink -Target (Join-Path ".sync-cache" $d) -Link $link
            Ok "${d}/: symlinked"
        }
    }

    # Project memory symlinks
    $memories = Get-ProjectMemories $SYNC_CACHE
    foreach ($encoded in $memories) {
        $projDir = Join-Path $CLAUDE_DIR "projects\$encoded"
        $link = Join-Path $projDir "memory"
        $cache = Join-Path $SYNC_CACHE "projects\$encoded\memory"

        if (-not (Test-Path $projDir)) {
            New-Item -ItemType Directory -Path $projDir -Force | Out-Null
        }

        if ((Test-Path $link -PathType Container) -and -not ((Get-Item $link -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            Move-Item $link "${link}.pre-bootstrap" -Force
            Warn "projects/$encoded/memory: backed up existing"
        }

        New-Symlink -Target "..\..\..sync-cache\projects\$encoded\memory" -Link $link
        Ok "projects/$encoded/memory: symlinked"
    }

    Save-SyncState

    Write-Host ""
    Log "Bootstrap complete!"
    Log "Run '.\claude-config-sync.ps1 sync' periodically to stay synced."
}

# ═══════════════════════════════════════════════════════════════════════
# ROLLBACK
# ═══════════════════════════════════════════════════════════════════════
function Invoke-Rollback {
    if (-not (Test-RemoteSSH)) {
        Err "Cannot reach remote"
        exit 1
    }

    $backups = & ssh @SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" "ls -1 '$BACKUP_REMOTE/' 2>/dev/null" 2>&1
    if (-not $backups) {
        Warn "No backups found on remote"
        return
    }

    if (-not $Timestamp) {
        Log "Available backups:"
        Write-Host ""
        foreach ($b in ($backups -split "`n" | Where-Object { $_ })) {
            Write-Host "  $b"
        }
        Write-Host ""
        Log "Usage: .\claude-config-sync.ps1 rollback -Timestamp <TS>"
        return
    }

    $exists = & ssh @SSH_OPTS "$REMOTE_USER@$REMOTE_HOST" "test -d '$BACKUP_REMOTE/$Timestamp' && echo yes" 2>&1
    if ($exists -ne "yes") {
        Err "Backup not found: $Timestamp"
        return
    }

    Log "Rolling back to: $Timestamp"

    foreach ($f in $SYNC_FILES) {
        $dst = Join-Path $SYNC_CACHE $f
        if (Pull-FileFromRemote "$BACKUP_REMOTE/$Timestamp/$f" $dst) {
            Ok "Restored $f"
        }
    }

    foreach ($d in $SYNC_DIRS) {
        $dst = Join-Path $SYNC_CACHE $d
        Pull-DirFromRemote "$BACKUP_REMOTE/$Timestamp/$d" $dst
        Ok "Restored $d/"
    }

    Invoke-Push
    Ok "Rollback to $Timestamp complete"
}

# ═══════════════════════════════════════════════════════════════════════
# EXPORT — Package config for consolidation on another workstation
# ═══════════════════════════════════════════════════════════════════════
function Invoke-Export {
    $exportDir = if ($OutputPath) { $OutputPath } else { Join-Path $env:TEMP "claude-config-export" }

    Log "Exporting Claude Code config to: $exportDir"

    if (Test-Path $exportDir) {
        Remove-Item $exportDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $exportDir -Force | Out-Null

    # Determine source: sync cache if exists, otherwise raw ~/.claude/
    $source = if (Test-Path $SYNC_CACHE) { $SYNC_CACHE } else { $CLAUDE_DIR }

    foreach ($f in $SYNC_FILES) {
        $src = Join-Path $source $f
        if (Test-Path $src) {
            Copy-Item $src (Join-Path $exportDir $f) -Force
            Ok "Exported $f"
        }
    }

    foreach ($d in $SYNC_DIRS) {
        $src = Join-Path $source $d
        if (Test-Path $src) {
            Copy-Item $src (Join-Path $exportDir $d) -Recurse -Force
            Ok "Exported $d/"
        }
    }

    # Export project memories
    $projSrc = Join-Path $source "projects"
    if (-not (Test-Path $projSrc)) { $projSrc = Join-Path $CLAUDE_DIR "projects" }
    if (Test-Path $projSrc) {
        Get-ChildItem -Path $projSrc -Directory | ForEach-Object {
            $memDir = Join-Path $_.FullName "memory"
            if ((Test-Path $memDir) -and (Get-ChildItem $memDir -Filter "*.md" -ErrorAction SilentlyContinue)) {
                $dstDir = Join-Path $exportDir "projects\$($_.Name)\memory"
                New-Item -ItemType Directory -Path $dstDir -Force | Out-Null
                Copy-Item "$memDir\*.md" $dstDir -Force
                Ok "Exported projects/$($_.Name)/memory/"
            }
        }
    }

    Write-Host ""
    Log "Export complete: $exportDir"
    Log "Transfer this directory to another workstation and run:"
    Log "  ./claude-config-sync.sh consolidate --from /path/to/export"
}

# ═══════════════════════════════════════════════════════════════════════
# HELP
# ═══════════════════════════════════════════════════════════════════════
function Show-Help {
    Write-Host @"

claude-config-sync.ps1 — Centralize Claude Code config on Munin (Windows)

Usage: .\claude-config-sync.ps1 <Command> [Options]

Commands:
  init          First-time setup (move -> cache -> symlink -> push)
  push          Push local changes to remote
  pull          Pull remote changes to local cache
  sync          Bidirectional sync
  status        Show sync state of managed files
  consolidate   Merge memory from another source (-From PATH)
  bootstrap     Set up fresh workstation from remote
  rollback      Restore from remote backup (-Timestamp TS)
  export        Package config for transfer to another workstation

Options:
  -DryRun           Show what would happen without changes
  -Force            Overwrite without conflict checks
  -WorkstationId ID Override hostname-based ID
  -From PATH        Source for consolidate
  -Timestamp TS     Backup timestamp for rollback
  -OutputPath PATH  Export destination (default: temp dir)

"@
}

# ═══════════════════════════════════════════════════════════════════════
# Dispatch
# ═══════════════════════════════════════════════════════════════════════
switch ($Command) {
    "init"        { Invoke-Init }
    "push"        { Invoke-Push }
    "pull"        { Invoke-Pull }
    "sync"        { Invoke-Sync }
    "status"      { Invoke-Status }
    "consolidate" { Invoke-Consolidate }
    "bootstrap"   { Invoke-Bootstrap }
    "rollback"    { Invoke-Rollback }
    "export"      { Invoke-Export }
    "help"        { Show-Help }
    default       { Show-Help }
}
