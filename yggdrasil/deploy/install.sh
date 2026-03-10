#!/usr/bin/env bash
# First-time installation on a target node.
# Usage: ./install.sh <node> <services...>
# Example: ./install.sh munin odin mimir
# Example: ./install.sh hugin huginn muninn
set -euo pipefail

NODE=$1
shift
SERVICES=("$@")
REMOTE="jhernandez@${NODE}"
INSTALL_DIR="/opt/yggdrasil"
CONFIG_DIR="/etc/yggdrasil"

# 1. Create yggdrasil user and directories
ssh "$REMOTE" "sudo useradd -r -s /sbin/nologin yggdrasil 2>/dev/null || true"
ssh "$REMOTE" "sudo mkdir -p ${INSTALL_DIR}/bin ${CONFIG_DIR}"
ssh "$REMOTE" "sudo chown yggdrasil:yggdrasil ${INSTALL_DIR}"

# 2. Build release binaries
cargo build --release --bin "${SERVICES[@]}"

# 3. Copy binaries
for svc in "${SERVICES[@]}"; do
    rsync -avz "target/release/${svc}" "${REMOTE}:${INSTALL_DIR}/bin/"
done

# 4. Copy config files
for svc in "${SERVICES[@]}"; do
    config_dir="configs/${svc}"
    if [ -d "$config_dir" ]; then
        ssh "$REMOTE" "sudo mkdir -p ${CONFIG_DIR}/${svc}"
        rsync -avz "${config_dir}/" "${REMOTE}:${CONFIG_DIR}/${svc}/"
    fi
done

# 5. Install systemd units
for svc in "${SERVICES[@]}"; do
    unit="deploy/systemd/yggdrasil-${svc}.service"
    if [ -f "$unit" ]; then
        rsync -avz "$unit" "${REMOTE}:/etc/systemd/system/"
    fi
done
rsync -avz "deploy/wait-for-health.sh" "${REMOTE}:${INSTALL_DIR}/bin/"
ssh "$REMOTE" "sudo chmod +x ${INSTALL_DIR}/bin/wait-for-health.sh"

# 6. Reload systemd and enable services
ssh "$REMOTE" "sudo systemctl daemon-reload"
for svc in "${SERVICES[@]}"; do
    ssh "$REMOTE" "sudo systemctl enable yggdrasil-${svc}.service"
done

echo "Installation complete on ${NODE}. Start services with:"
for svc in "${SERVICES[@]}"; do
    echo "  sudo systemctl start yggdrasil-${svc}"
done
