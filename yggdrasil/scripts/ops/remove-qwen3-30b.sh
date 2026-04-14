#!/usr/bin/env bash
# Sprint 065 Track B·P6 — remove retired qwen3:30b-a3b from Hugin.
#
# User directive 2026-04-14: retire qwen3:30b-a3b. The model is still
# loaded on Hugin Ollama and holding 18.56 GB. Run this script ONCE after
# confirming no flow template references qwen3:* (grep already clean at
# time of writing, but verify again to be safe).
#
# Usage: bash scripts/ops/remove-qwen3-30b.sh [--dry-run]

set -eu -o pipefail

HUGIN_SSH="${HUGIN_SSH:-hugin}"
DRY_RUN="${1:-}"

echo "Sprint 065 B·P6 — remove qwen3:30b-a3b from $HUGIN_SSH"
echo

# 1. Pre-audit: grep for any residual references in configs.
echo "1. Auditing repo for stale qwen3:30b-a3b references..."
if grep -rE "qwen3[:-]?30b-a3b" /home/jesushernandez/Documents/Code/Yggdrasil/yggdrasil/configs/ /home/jesushernandez/Documents/Code/Yggdrasil/yggdrasil/deploy/config-templates/ 2>/dev/null; then
    echo "FAIL: residual references found — update configs before removing model"
    exit 1
fi
echo "   clean — no residual references"

# 2. Pre-check: model still present?
echo
echo "2. Checking model presence on Hugin..."
if ssh "$HUGIN_SSH" 'ollama list 2>/dev/null | grep -q "qwen3:30b-a3b"'; then
    echo "   qwen3:30b-a3b is loaded on $HUGIN_SSH"
else
    echo "   qwen3:30b-a3b NOT loaded — nothing to do"
    exit 0
fi

# 3. Remove.
if [ "$DRY_RUN" = "--dry-run" ]; then
    echo
    echo "3. DRY RUN — would execute: ssh $HUGIN_SSH 'ollama rm qwen3:30b-a3b'"
    exit 0
fi

echo
echo "3. Removing qwen3:30b-a3b..."
ssh "$HUGIN_SSH" 'ollama rm qwen3:30b-a3b'

# 4. Verify.
echo
echo "4. Verifying removal..."
if ssh "$HUGIN_SSH" 'ollama list 2>/dev/null | grep -q "qwen3:30b-a3b"'; then
    echo "FAIL: model still present after ollama rm"
    exit 1
fi

disk_free=$(ssh "$HUGIN_SSH" "du -sh /usr/share/ollama/.ollama/models 2>/dev/null | awk '{print \$1}'")
echo "   ✓ qwen3:30b-a3b removed"
echo "   Ollama models dir now: $disk_free"
echo
echo "Sprint 065 B·P6 complete."
