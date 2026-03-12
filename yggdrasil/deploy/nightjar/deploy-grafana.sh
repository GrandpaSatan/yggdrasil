#!/usr/bin/env bash
# Deploy Prometheus + Grafana observability stack to Nightjar.
# Run from the repository root:
#   bash deploy/nightjar/deploy-grafana.sh
#
# Required environment variables:
#   NIGHTJAR_HOST  — IP or hostname of the Nightjar node (e.g. REDACTED_NIGHTJAR_IP)
#
# Optional environment variables:
#   DEPLOY_USER    — SSH user on Nightjar (default: yggdrasil)
set -euo pipefail

NIGHTJAR="${DEPLOY_USER:-yggdrasil}@${NIGHTJAR_HOST:?Set NIGHTJAR_HOST}"
REMOTE_DIR="~/yggdrasil-observability"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "[1/3] Syncing config to Nightjar..."
ssh "$NIGHTJAR" "mkdir -p $REMOTE_DIR"
scp -r "$SCRIPT_DIR/." "$NIGHTJAR:$REMOTE_DIR/"

echo "[2/3] Starting stack..."
ssh "$NIGHTJAR" \
  "cd $REMOTE_DIR && docker compose -f docker-compose.grafana.yml pull && docker compose -f docker-compose.grafana.yml up -d"

echo "[3/3] Waiting for Grafana..."
sleep 5
if ssh "$NIGHTJAR" "curl -sf http://localhost:3000/api/health > /dev/null"; then
    echo "Grafana is up: http://${NIGHTJAR_HOST}:3000"
    echo "Prometheus:    http://${NIGHTJAR_HOST}:9099"
else
    echo "WARNING: Grafana health check failed — check 'docker logs ygg-grafana' on Nightjar"
fi
