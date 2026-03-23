#!/usr/bin/env bash
# Yggdrasil post-resume service recovery.
# Restarts all Yggdrasil services in dependency order after a suspend/resume cycle.
# Deployed to: /opt/yggdrasil/bin/yggdrasil-resume.sh
set -uo pipefail

LOG_TAG="yggdrasil-resume"
log() { logger -t "$LOG_TAG" "$1"; echo "$1"; }

log "Resume detected. Starting service recovery..."

# 1. Wait for network (resume can have a brief network gap)
NETWORK_TIMEOUT=30
ELAPSED=0
while [ "$ELAPSED" -lt "$NETWORK_TIMEOUT" ]; do
    if ip route show default | grep -q default; then
        log "Network available after ${ELAPSED}s"
        break
    fi
    sleep 1
    ELAPSED=$((ELAPSED + 1))
done

if [ "$ELAPSED" -ge "$NETWORK_TIMEOUT" ]; then
    log "WARNING: Network not available after ${NETWORK_TIMEOUT}s, proceeding anyway"
fi

# 2. Ensure Docker is running (PostgreSQL and Ollama containers auto-restart with it)
if ! systemctl is-active --quiet docker.service; then
    log "Docker not active, restarting..."
    systemctl restart docker.service
    sleep 5
fi

# 3. Restart services in dependency order:
#    Ollama (IPEX container) -> Mimir (needs PG + Ollama) -> Odin (needs Mimir) -> MCP Remote (needs Odin)
SERVICES=(
    "yggdrasil-ollama-ipex"
    "yggdrasil-mimir"
    "yggdrasil-odin"
    "yggdrasil-mcp-remote"
)

for svc in "${SERVICES[@]}"; do
    if systemctl is-enabled --quiet "$svc" 2>/dev/null; then
        log "Restarting ${svc}..."
        systemctl restart "$svc" || log "WARNING: Failed to restart ${svc}"
    fi
done

# 4. Health checks using existing wait-for-health.sh
HEALTH_SCRIPT="/opt/yggdrasil/bin/wait-for-health.sh"
if [ -x "$HEALTH_SCRIPT" ]; then
    log "Waiting for Mimir health..."
    "$HEALTH_SCRIPT" http://localhost:9090/health 60 || log "WARNING: Mimir health check failed"

    log "Waiting for Odin health..."
    "$HEALTH_SCRIPT" http://localhost:8080/health 60 || log "WARNING: Odin health check failed"
fi

log "Resume recovery complete."
