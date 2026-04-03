#!/usr/bin/env bash
# install-memory.sh — Install ygg-memory hooks into Claude Code settings.json
# Sprint 045: Memory Sidecar v2
#
# Usage: ./install-memory.sh
# Idempotent — safe to re-run. Backs up settings.json before modifying.
set -euo pipefail

SETTINGS="$HOME/.claude/settings.json"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MEMORY_SCRIPT="$SCRIPT_DIR/ygg-memory.sh"

# ── Prerequisites ────────────────────────────────────────────────────
if ! command -v jq &>/dev/null; then
    echo "ERROR: jq is required. Install with: sudo apt install jq"
    exit 1
fi

if [ ! -f "$SETTINGS" ]; then
    echo "ERROR: $SETTINGS not found. Is Claude Code installed?"
    exit 1
fi

if [ ! -f "$MEMORY_SCRIPT" ]; then
    echo "ERROR: $MEMORY_SCRIPT not found."
    exit 1
fi

chmod +x "$MEMORY_SCRIPT"

# ── Backup ───────────────────────────────────────────────────────────
cp "$SETTINGS" "$SETTINGS.bak.$(date +%s)"
echo "Backed up settings.json"

# ── Remove old hooks ─────────────────────────────────────────────────
# Remove flag-file hooks, penalty hooks, old memory hooks
# Keep only HA verification hook (PreToolUse for ha_call_service_tool)
echo "Removing old hook system..."

# Build the new hooks config with jq
# Strategy: rebuild hooks from scratch, preserving only the HA safety hook
jq --arg script "$MEMORY_SCRIPT" '
  .hooks = {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": ($script + " init"),
            "timeout": 5000
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": ($script + " recall"),
            "timeout": 1000
          }
        ]
      },
      {
        "matcher": "mcp__yggdrasil__ha_call_service_tool",
        "hooks": [
          {
            "type": "command",
            "command": "if [ ! -f /tmp/ygg-hooks/ha_verified ]; then echo \"BLOCKED: Must call ha_get_states_tool or ha_list_entities_tool before controlling devices.\" >&2; exit 2; fi"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": ($script + " ingest"),
            "timeout": 5000
          }
        ]
      },
      {
        "matcher": "mcp__yggdrasil__ha_get_states_tool|mcp__yggdrasil__ha_list_entities_tool",
        "hooks": [
          {
            "type": "command",
            "command": "mkdir -p /tmp/ygg-hooks && touch /tmp/ygg-hooks/ha_verified"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": ($script + " sleep"),
            "timeout": 15000
          }
        ]
      }
    ]
  }
' "$SETTINGS" > "$SETTINGS.tmp" && mv "$SETTINGS.tmp" "$SETTINGS"

echo ""
echo "Memory hooks installed:"
echo "  SessionStart → ygg-memory.sh init    (recall prior context)"
echo "  PreToolUse   → ygg-memory.sh recall  (surface memories before edits)"
echo "  PostToolUse  → ygg-memory.sh ingest  (LLM judges + stores changes)"
echo "  Stop         → ygg-memory.sh sleep   (consolidate session memories)"
echo ""
echo "Preserved: HA device verification hook (safety check)"

# ── Versioned extension install/update ────────────────────────────────
EXT_DIR="$SCRIPT_DIR/../../extensions/yggdrasil-local"

if [ -d "$EXT_DIR/package.json" ] || [ -f "$EXT_DIR/package.json" ]; then
    SOURCE_VERSION=$(jq -r '.version // "0.0.0"' "$EXT_DIR/package.json" 2>/dev/null)
    INSTALLED_VERSION=$(ls -d "$HOME/.vscode/extensions/yggdrasil.yggdrasil-local-"* 2>/dev/null \
        | sed 's/.*yggdrasil-local-//' | sort -V | tail -1)
    INSTALLED_VERSION="${INSTALLED_VERSION:-not_installed}"

    if [ "$SOURCE_VERSION" = "$INSTALLED_VERSION" ]; then
        echo ""
        echo "Extension yggdrasil-local v${SOURCE_VERSION} already installed — up to date."
    elif command -v node &>/dev/null && command -v npm &>/dev/null; then
        echo ""
        echo "Updating extension: ${INSTALLED_VERSION} → ${SOURCE_VERSION}"
        (
            cd "$EXT_DIR"
            npm install --no-audit --no-fund 2>&1 | tail -1
            npm run compile 2>&1
        )

        if [ -f "$EXT_DIR/out/extension.js" ]; then
            if command -v code &>/dev/null; then
                (cd "$EXT_DIR" && npx @vscode/vsce package --no-dependencies 2>/dev/null)
                VSIX=$(ls -t "$EXT_DIR"/*.vsix 2>/dev/null | head -1)
                if [ -n "$VSIX" ]; then
                    code --install-extension "$VSIX" --force 2>/dev/null
                    echo "Extension installed: yggdrasil.yggdrasil-local v${SOURCE_VERSION}"
                fi
            fi
        else
            echo "Warning: Extension build failed"
        fi
    else
        echo ""
        echo "Skipping extension build (node/npm not found)"
    fi
else
    echo ""
    echo "Skipping extension (source not found at $EXT_DIR)"
fi

echo ""
echo "Restart Claude Code to activate."
