#!/usr/bin/env bash
# Deploy Prometheus + Grafana observability stack to Nightjar (REDACTED_NIGHTJAR_IP).
# Run from the repository root:
#   bash deploy/nightjar/deploy-grafana.sh
set -euo pipefail

NIGHTJAR="yggdrasil@REDACTED_NIGHTJAR_IP"
REMOTE_DIR="~/yggdrasil-observability"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "[1/3] Syncing config to Nightjar..."
sshpass -p "CHANGEME" ssh "$NIGHTJAR" "mkdir -p $REMOTE_DIR"
sshpass -p "CHANGEME" scp -r "$SCRIPT_DIR/." "$NIGHTJAR:$REMOTE_DIR/"

echo "[2/3] Starting stack..."
sshpass -p "CHANGEME" ssh "$NIGHTJAR" \
  "cd $REMOTE_DIR && docker compose -f docker-compose.grafana.yml pull && docker compose -f docker-compose.grafana.yml up -d"

echo "[3/3] Waiting for Grafana..."
sleep 5
if sshpass -p "CHANGEME" ssh "$NIGHTJAR" "curl -sf http://localhost:3000/api/health > /dev/null"; then
    echo "Grafana is up: http://REDACTED_NIGHTJAR_IP:3000 (admin/admin)"
    echo "Prometheus:    http://REDACTED_NIGHTJAR_IP:9099"
else
    echo "WARNING: Grafana health check failed — check 'docker logs ygg-grafana' on Nightjar"
fi
