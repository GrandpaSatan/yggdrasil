#Requires -Version 5.1
<#
.SYNOPSIS
    ClaudeClient_Install.ps1 — Configure Claude Code + Yggdrasil MCP on Windows

.DESCRIPTION
    Sets up a Windows workstation with the Yggdrasil MCP ecosystem:
      1. Checks/installs prerequisites (Claude CLI, Rust toolchain)
      2. Builds ygg-mcp-server (local sync_docs server)
      3. Configures ~/.claude.json with:
         - yggdrasil        (HTTP)  -> remote MCP on Munin:9093
         - yggdrasil-local  (stdio) -> local sync_docs server
      4. Installs global CLAUDE.md (agent system prompt)
      5. Bootstraps project memory for the project directory

    Path-agnostic: works regardless of where the yggdrasil repo is cloned.
    Idempotent — safe to re-run.

.PARAMETER ProjectDir
    Project directory (the dir you open in your IDE).
    Auto-detected if omitted: uses repo parent if it has .git,
    otherwise uses the repo root itself.

.EXAMPLE
    .\ClaudeClient_Install.ps1
    # Auto-detect project dir

.EXAMPLE
    .\ClaudeClient_Install.ps1 -ProjectDir C:\Users\jesus\Documents\HardwareSetup
    # Explicit project dir
#>

[CmdletBinding()]
param(
    [Alias("p")]
    [string]$ProjectDir
)

$ErrorActionPreference = "Stop"

# ── Constants ─────────────────────────────────────────────────────────
$REMOTE_MCP_URL = "http://$env:MUNIN_IP:9093/mcp"
$SCRIPT_DIR     = Split-Path -Parent $MyInvocation.MyCommand.Definition
$REPO_ROOT      = (Resolve-Path (Join-Path $SCRIPT_DIR "..\..")).Path
$HOME_DIR       = $env:USERPROFILE
$CLAUDE_DIR     = Join-Path $HOME_DIR ".claude"
$CLAUDE_JSON    = Join-Path $HOME_DIR ".claude.json"
$LOCAL_BINARY   = Join-Path $REPO_ROOT "target\release\ygg-mcp-server.exe"
$LOCAL_CONFIG_DIR = Join-Path $HOME_DIR ".config\yggdrasil"
$LOCAL_CONFIG   = Join-Path $LOCAL_CONFIG_DIR "local-mcp.yaml"

# ── Logging helpers ───────────────────────────────────────────────────
function Log  ($msg) { Write-Host "[ygg] $msg" -ForegroundColor Cyan }
function Ok   ($msg) { Write-Host "  + $msg" -ForegroundColor Green }
function Warn ($msg) { Write-Host "  ! $msg" -ForegroundColor Yellow }
function Err  ($msg) { Write-Host "  x $msg" -ForegroundColor Red }

# ── Auto-detect project dir ──────────────────────────────────────────
if (-not $ProjectDir) {
    $Parent = Split-Path -Parent $REPO_ROOT
    if ((Test-Path (Join-Path $Parent ".git")) -or
        (Test-Path (Join-Path $Parent "docs")) -or
        (Test-Path (Join-Path $Parent "CLAUDE.md"))) {
        $ProjectDir = $Parent
    } else {
        $ProjectDir = $REPO_ROOT
    }
} else {
    $ProjectDir = (Resolve-Path $ProjectDir).Path
}

Log "Yggdrasil repo:  $REPO_ROOT"
Log "Project dir:     $ProjectDir"
Write-Host ""

# ═══════════════════════════════════════════════════════════════════════
# Step 1: Prerequisites
# ═══════════════════════════════════════════════════════════════════════
Log "Checking prerequisites..."

# -- Rust toolchain --
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($cargo) {
    $rustVer = (rustc --version 2>$null) -replace 'rustc\s+', ''
    Ok "Rust $rustVer"
} else {
    Log "Installing Rust toolchain..."
    $rustupInit = Join-Path $env:TEMP "rustup-init.exe"
    try {
        Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $rustupInit -UseBasicParsing
        & $rustupInit -y --default-toolchain stable
        # Refresh PATH for this session
        $env:PATH = "$HOME_DIR\.cargo\bin;$env:PATH"
        Ok "Rust installed (restart terminal for permanent PATH)"
    } catch {
        Err "Failed to install Rust: $_"
        Err "Install manually from https://rustup.rs"
        exit 1
    }
}

# -- Claude Code CLI --
$claude = Get-Command claude -ErrorAction SilentlyContinue
if ($claude) {
    Ok "Claude Code CLI found"
} else {
    Warn "Claude Code CLI not found in PATH"
    Warn "Install: npm install -g @anthropic-ai/claude-code"
    Warn "Continuing - MCP config will still be set up"
}

# ═══════════════════════════════════════════════════════════════════════
# Step 2: Build local MCP server
# ═══════════════════════════════════════════════════════════════════════
Log "Building ygg-mcp-server..."

$cargoToml = Join-Path $REPO_ROOT "Cargo.toml"
if (-not (Test-Path $cargoToml)) {
    Err "Cannot find Cargo.toml at $REPO_ROOT"
    exit 1
}

Push-Location $REPO_ROOT
try {
    & cargo build --release --bin ygg-mcp-server 2>&1 | Select-Object -Last 3
    if ($LASTEXITCODE -ne 0) {
        Err "cargo build failed (exit code $LASTEXITCODE)"
        Err "Ensure build dependencies are installed (Visual Studio Build Tools, OpenSSL)"
        exit 1
    }
    Ok "Binary: $LOCAL_BINARY"
} finally {
    Pop-Location
}

# ═══════════════════════════════════════════════════════════════════════
# Step 3: Local MCP config (generated per-workstation)
# ═══════════════════════════════════════════════════════════════════════
Log "Generating local MCP config..."

if (-not (Test-Path $LOCAL_CONFIG_DIR)) {
    New-Item -ItemType Directory -Path $LOCAL_CONFIG_DIR -Force | Out-Null
}

$ProjectName = (Split-Path -Leaf $ProjectDir).ToLower()
$timestamp = Get-Date -Format "o"

$configYaml = @"
# Auto-generated by ClaudeClient_Install.ps1 -- $timestamp
# Local MCP server config (ygg-mcp-server / yggdrasil-local)
# Repo: $REPO_ROOT
# Project: $ProjectDir
odin_url: "http://$env:MUNIN_IP:8080"
muninn_url: "http://$env:HUGIN_IP:9091"
timeout_secs: 300
generate_tok_per_sec: 15.0
prefetch_query: "active sprint $ProjectName"
project: "$ProjectName"
workspace_path: "$($ProjectDir -replace '\\', '/')"
"@

Set-Content -Path $LOCAL_CONFIG -Value $configYaml -Encoding UTF8
Ok "Config: $LOCAL_CONFIG"
Ok "  project=$ProjectName  workspace=$ProjectDir"

# ═══════════════════════════════════════════════════════════════════════
# Step 4: Configure Claude Code MCP servers in ~/.claude.json
# ═══════════════════════════════════════════════════════════════════════
Log "Configuring Claude Code MCP servers..."

if (-not (Test-Path $CLAUDE_DIR)) {
    New-Item -ItemType Directory -Path $CLAUDE_DIR -Force | Out-Null
}

if (-not (Test-Path $CLAUDE_JSON)) {
    Set-Content -Path $CLAUDE_JSON -Value "{}" -Encoding UTF8
    Warn "Created new $CLAUDE_JSON"
}

$claudeConfig = Get-Content -Path $CLAUDE_JSON -Raw | ConvertFrom-Json

# Ensure mcpServers property exists
if (-not (Get-Member -InputObject $claudeConfig -Name "mcpServers" -MemberType NoteProperty)) {
    $claudeConfig | Add-Member -NotePropertyName "mcpServers" -NotePropertyValue ([PSCustomObject]@{})
}

# -- Remote MCP server (HTTP) --
$remoteConfig = [PSCustomObject]@{
    type = "http"
    url  = $REMOTE_MCP_URL
}

$existingType = $null
if (Get-Member -InputObject $claudeConfig.mcpServers -Name "yggdrasil" -MemberType NoteProperty -ErrorAction SilentlyContinue) {
    $existingType = $claudeConfig.mcpServers.yggdrasil.type
}

switch ($existingType) {
    "http" {
        if ($claudeConfig.mcpServers.yggdrasil.url -eq $REMOTE_MCP_URL) {
            Ok "Remote MCP: already configured"
        } else {
            $claudeConfig.mcpServers.yggdrasil = $remoteConfig
            Warn "Remote MCP: updated URL"
        }
    }
    "stdio" {
        $claudeConfig.mcpServers.yggdrasil = $remoteConfig
        Warn "Remote MCP: upgraded stdio -> http"
    }
    default {
        $claudeConfig.mcpServers | Add-Member -NotePropertyName "yggdrasil" -NotePropertyValue $remoteConfig -Force
        Ok "Remote MCP: added"
    }
}

# -- Local MCP server (stdio) --
# Use forward slashes in the config path for YAML compatibility
$localConfigUnix = $LOCAL_CONFIG -replace '\\', '/'
$localBinaryPath = $LOCAL_BINARY -replace '/', '\'

$localMcpConfig = [PSCustomObject]@{
    type    = "stdio"
    command = $localBinaryPath
    args    = @("--config", $localConfigUnix)
    env     = [PSCustomObject]@{}
}

$claudeConfig.mcpServers | Add-Member -NotePropertyName "yggdrasil-local" -NotePropertyValue $localMcpConfig -Force
Ok "Local MCP: configured"

# -- Clear per-project MCP overrides that might shadow global config --
if (Get-Member -InputObject $claudeConfig -Name "projects" -MemberType NoteProperty -ErrorAction SilentlyContinue) {
    if (Get-Member -InputObject $claudeConfig.projects -Name $ProjectDir -MemberType NoteProperty -ErrorAction SilentlyContinue) {
        $proj = $claudeConfig.projects.$ProjectDir
        if (Get-Member -InputObject $proj -Name "mcpServers" -MemberType NoteProperty -ErrorAction SilentlyContinue) {
            if ($proj.mcpServers -and ($proj.mcpServers | Get-Member -MemberType NoteProperty).Count -gt 0) {
                $proj.mcpServers = [PSCustomObject]@{}
                Warn "Cleared per-project MCP overrides for $(Split-Path -Leaf $ProjectDir)"
            }
        }
    }
}

$claudeConfig | ConvertTo-Json -Depth 10 | Set-Content -Path $CLAUDE_JSON -Encoding UTF8
Ok "Saved $CLAUDE_JSON"

# ═══════════════════════════════════════════════════════════════════════
# Step 5: Install global CLAUDE.md
# ═══════════════════════════════════════════════════════════════════════
Log "Installing global CLAUDE.md..."

$claudeMdSrc = Join-Path $SCRIPT_DIR "CLAUDE.md"
$claudeMdDst = Join-Path $CLAUDE_DIR "CLAUDE.md"

if (Test-Path $claudeMdSrc) {
    if (Test-Path $claudeMdDst) {
        $srcHash = (Get-FileHash -Path $claudeMdSrc -Algorithm SHA256).Hash
        $dstHash = (Get-FileHash -Path $claudeMdDst -Algorithm SHA256).Hash
        if ($srcHash -eq $dstHash) {
            Ok "CLAUDE.md: up to date"
        } else {
            Copy-Item -Path $claudeMdSrc -Destination $claudeMdDst -Force
            Ok "CLAUDE.md: updated"
        }
    } else {
        Copy-Item -Path $claudeMdSrc -Destination $claudeMdDst -Force
        Ok "CLAUDE.md: installed"
    }
} else {
    Warn "CLAUDE.md source not found at $claudeMdSrc"
    Warn "Create it manually at $claudeMdDst or copy from another workstation"
}

# ═══════════════════════════════════════════════════════════════════════
# Step 6: Bootstrap project memory
# ═══════════════════════════════════════════════════════════════════════
Log "Bootstrapping project memory..."

$projHash = $ProjectDir -replace '[:\\]', '-'
$memoryDir  = Join-Path $CLAUDE_DIR "projects\$projHash\memory"
$memoryFile = Join-Path $memoryDir "MEMORY.md"

if (-not (Test-Path $memoryDir)) {
    New-Item -ItemType Directory -Path $memoryDir -Force | Out-Null
}

$bootstrapSrc = Join-Path $SCRIPT_DIR "MEMORY-bootstrap.md"
if (Test-Path $memoryFile) {
    Ok "Project memory: already exists (not overwriting)"
} elseif (Test-Path $bootstrapSrc) {
    Copy-Item -Path $bootstrapSrc -Destination $memoryFile -Force
    Ok "Project memory: bootstrapped"
} else {
    Warn "MEMORY-bootstrap.md not found — skipping memory bootstrap"
}

# ═══════════════════════════════════════════════════════════════════════
# Step 7: Verify remote server connectivity
# ═══════════════════════════════════════════════════════════════════════
Log "Verifying remote MCP server..."

$initBody = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"install-check","version":"1.0"}}}'

try {
    $resp = Invoke-WebRequest -Uri $REMOTE_MCP_URL `
        -Method POST `
        -Body $initBody `
        -ContentType "application/json" `
        -Headers @{ Accept = "application/json, text/event-stream" } `
        -TimeoutSec 10 `
        -UseBasicParsing `
        -ErrorAction Stop

    if ($resp.Content -match "serverInfo") {
        Ok "Remote server: reachable"
    } else {
        Warn "Remote server: unexpected response format"
    }
} catch {
    Warn "Remote server: not reachable (network tools will be unavailable)"
    Warn "Ensure you are on the same network as Munin ($env:MUNIN_IP)"
}

# ═══════════════════════════════════════════════════════════════════════
# Summary
# ═══════════════════════════════════════════════════════════════════════
Write-Host ""
Log "Installation complete!"
Write-Host ""
Write-Host "  Repo:        $REPO_ROOT"
Write-Host "  Project:     $ProjectDir"
Write-Host "  Remote MCP:  $REMOTE_MCP_URL"
Write-Host "  Local MCP:   $LOCAL_BINARY"
Write-Host "  Config:      $LOCAL_CONFIG"
Write-Host "  CLAUDE.md:   $claudeMdDst"
Write-Host "  Memory:      $memoryFile"
Write-Host ""
Write-Host "  Restart Claude Code to pick up the new MCP configuration."
Write-Host ""
# ═══════════════════════════════════════════════════════════════════════
# Step 8: Config sync bootstrap (if central config exists on Munin)
# ═══════════════════════════════════════════════════════════════════════
$syncScript = Join-Path $SCRIPT_DIR "claude-config-sync.ps1"
$remoteHostIP = if ($env:MUNIN_IP) { $env:MUNIN_IP } else { "localhost" }
$remoteUserSSH = if ($env:DEPLOY_USER) { $env:DEPLOY_USER } else { $env:USERNAME }

if (Test-Path $syncScript) {
    Log "Checking for centralized config on Munin..."
    try {
        $hasConfig = & ssh -o ConnectTimeout=3 -o BatchMode=yes "$remoteUserSSH@$remoteHostIP" "test -d /opt/yggdrasil/claude-config/agents && echo yes" 2>&1
        if ($hasConfig -eq "yes") {
            Ok "Central config found on Munin"
            $syncCache = Join-Path $CLAUDE_DIR ".sync-cache"
            if (Test-Path $syncCache) {
                Log "Sync cache exists - running sync..."
                & $syncScript sync
            } else {
                Log "First time - bootstrapping from central config..."
                & $syncScript bootstrap
            }
        } else {
            Warn "No central config on Munin (run 'claude-config-sync.ps1 init' to set up)"
        }
    } catch {
        Warn "Could not check Munin for central config (SSH not available?)"
    }
} else {
    Warn "claude-config-sync.ps1 not found - skipping config sync"
}

Write-Host ""
Log "Installation complete!"
Write-Host ""

Write-Host "  NOTE: If this is a fresh Windows install, you may need:" -ForegroundColor Yellow
Write-Host "    - Visual Studio Build Tools (C++ workload) for Rust compilation" -ForegroundColor Yellow
Write-Host "    - OpenSSL: winget install ShiningLight.OpenSSL" -ForegroundColor Yellow
