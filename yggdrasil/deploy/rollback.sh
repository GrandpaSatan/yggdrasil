#!/usr/bin/env bash
# Rollback a service to the previous binary.
# Usage: ./rollback.sh <node> <service>
set -euo pipefail

NODE=$1
SERVICE=$2
REMOTE="jhernandez@${NODE}"
BIN="/opt/yggdrasil/bin/${SERVICE}"

ssh "$REMOTE" "sudo cp ${BIN} ${BIN}.failed && sudo cp ${BIN}.prev ${BIN}"
ssh "$REMOTE" "sudo systemctl restart yggdrasil-${SERVICE}"
echo "Rolled back ${SERVICE} on ${NODE}. Previous binary preserved as ${BIN}.failed"
