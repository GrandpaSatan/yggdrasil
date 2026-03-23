#!/usr/bin/env bash
# First-time installation on a target node.
# Usage: ./install.sh <node> <services...>
# Example: ./install.sh munin odin mimir
# Example: ./install.sh hugin huginn muninn
set -euo pipefail

NODE=$1
shift
SERVICES=("$@")
REMOTE="${DEPLOY_USER:-yggdrasil}@${NODE}"
INSTALL_DIR="/opt/yggdrasil"
CONFIG_DIR="/etc/yggdrasil"

# 1. Create yggdrasil user and directories
ssh "$REMOTE" "sudo useradd -r -s /sbin/nologin yggdrasil 2>/dev/null || true"
ssh "$REMOTE" "sudo mkdir -p ${INSTALL_DIR}/bin ${CONFIG_DIR}"
ssh "$REMOTE" "sudo chown yggdrasil:yggdrasil ${INSTALL_DIR}"

# 2. Build release binaries
BIN_ARGS=()
for svc in "${SERVICES[@]}"; do
    BIN_ARGS+=(--bin "$svc")
done
cargo build --release "${BIN_ARGS[@]}"

# 3. Copy binaries
for svc in "${SERVICES[@]}"; do
    rsync -avz --rsync-path="sudo rsync" "target/release/${svc}" "${REMOTE}:${INSTALL_DIR}/bin/"
done

# 4. Copy config files
for svc in "${SERVICES[@]}"; do
    config_dir="configs/${svc}"
    if [ -d "$config_dir" ]; then
        ssh "$REMOTE" "sudo mkdir -p ${CONFIG_DIR}/${svc}"
        rsync -avz --rsync-path="sudo rsync" "${config_dir}/" "${REMOTE}:${CONFIG_DIR}/${svc}/"
    fi
done

# 5. Install systemd units
for svc in "${SERVICES[@]}"; do
    unit="deploy/systemd/yggdrasil-${svc}.service"
    if [ -f "$unit" ]; then
        rsync -avz --rsync-path="sudo rsync" "$unit" "${REMOTE}:/etc/systemd/system/"
    fi
done
rsync -avz --rsync-path="sudo rsync" "deploy/wait-for-health.sh" "${REMOTE}:${INSTALL_DIR}/bin/"
ssh "$REMOTE" "sudo chmod +x ${INSTALL_DIR}/bin/wait-for-health.sh"

# 5b. Deploy power management overrides (Munin only — laptop server)
if [ "$NODE" = "munin" ]; then
    echo "Deploying power management overrides for Munin (laptop server)..."

    # logind override: prevent suspend on lid close / idle
    ssh "$REMOTE" "sudo mkdir -p /etc/systemd/logind.conf.d"
    rsync -avz --rsync-path="sudo rsync" \
        "deploy/munin/power/logind.conf.d/99-yggdrasil-nosuspend.conf" \
        "${REMOTE}:/etc/systemd/logind.conf.d/"

    # sleep override: disable all sleep states
    ssh "$REMOTE" "sudo mkdir -p /etc/systemd/sleep.conf.d"
    rsync -avz --rsync-path="sudo rsync" \
        "deploy/munin/power/sleep.conf.d/99-yggdrasil-nosleep.conf" \
        "${REMOTE}:/etc/systemd/sleep.conf.d/"

    # Resume recovery service + script
    rsync -avz --rsync-path="sudo rsync" \
        "deploy/munin/power/yggdrasil-resume.service" \
        "${REMOTE}:/etc/systemd/system/"
    rsync -avz --rsync-path="sudo rsync" \
        "deploy/munin/power/yggdrasil-resume.sh" \
        "${REMOTE}:${INSTALL_DIR}/bin/"
    ssh "$REMOTE" "sudo chmod +x ${INSTALL_DIR}/bin/yggdrasil-resume.sh"

    # Mask sleep targets (belt and suspenders)
    ssh "$REMOTE" "sudo systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target"

    # Restart logind to pick up new config
    ssh "$REMOTE" "sudo systemctl restart systemd-logind"

    # Enable resume recovery service
    ssh "$REMOTE" "sudo systemctl daemon-reload"
    ssh "$REMOTE" "sudo systemctl enable yggdrasil-resume.service"

    echo "Power management configured: suspend disabled, resume safety net enabled."
fi

# 6. Reload systemd and enable services
ssh "$REMOTE" "sudo systemctl daemon-reload"
for svc in "${SERVICES[@]}"; do
    ssh "$REMOTE" "sudo systemctl enable yggdrasil-${svc}.service"
done

echo "Installation complete on ${NODE}. Start services with:"
for svc in "${SERVICES[@]}"; do
    echo "  sudo systemctl start yggdrasil-${svc}"
done
