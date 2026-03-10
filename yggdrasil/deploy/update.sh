#!/usr/bin/env bash
# Rolling update of a service on a target node.
# Usage: ./update.sh <node> <service>
# Example: ./update.sh munin odin
set -euo pipefail

NODE=$1
SERVICE=$2
REMOTE="jhernandez@${NODE}"
INSTALL_DIR="/opt/yggdrasil"
BIN="${INSTALL_DIR}/bin/${SERVICE}"

# 1. Build release binary
cargo build --release --bin "$SERVICE"

# 2. Copy new binary (keep previous as .prev for rollback)
ssh "$REMOTE" "sudo cp ${BIN} ${BIN}.prev 2>/dev/null || true"
rsync -avz "target/release/${SERVICE}" "${REMOTE}:${BIN}.new"
ssh "$REMOTE" "sudo mv ${BIN}.new ${BIN}"

# 3. Restart service
ssh "$REMOTE" "sudo systemctl restart yggdrasil-${SERVICE}"

# 4. Wait for health
sleep 2
HEALTH_PORT=$(ssh "$REMOTE" "systemctl show yggdrasil-${SERVICE} -p ExecStart" | grep -oP ':\d+' | head -1 | tr -d ':')
if [ -n "$HEALTH_PORT" ]; then
    ssh "$REMOTE" "${INSTALL_DIR}/bin/wait-for-health.sh http://localhost:${HEALTH_PORT}/health 30"
fi

echo "Update complete: ${SERVICE} on ${NODE}"
ssh "$REMOTE" "sudo systemctl status yggdrasil-${SERVICE} --no-pager"
