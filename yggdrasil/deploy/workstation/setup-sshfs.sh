#!/usr/bin/env bash
# setup-sshfs.sh — Mount Hugin's repo directory over SSHFS on the workstation.
#
# After mounting, /mnt/hugin-repos mirrors ${REMOTE_PATH} on Hugin
# (${HUGIN_HOST}).  Huginn can then index both local and remote repos without
# needing a separate Huginn instance on Hugin.
#
# Usage:
#   sudo ./setup-sshfs.sh [mount|umount|status]
#
# Required environment variables:
#   HUGIN_HOST    — IP or hostname of the Hugin node
#   DEPLOY_USER   — SSH user on Hugin (default: yggdrasil)
#   LOCAL_USER    — Local user for uid/gid ownership (default: current user)
#
# Prerequisites:
#   sudo apt install sshfs
#   SSH key auth configured: ssh ${DEPLOY_USER}@${HUGIN_HOST} works without password.
#
# Persistent mount (survives reboots):
#   Add to /etc/fstab:
#   ${DEPLOY_USER}@${HUGIN_HOST}:${REMOTE_PATH} /mnt/hugin-repos fuse.sshfs
#     defaults,_netdev,reconnect,ServerAliveInterval=15,ServerAliveCountMax=3,
#     IdentityFile=/home/${LOCAL_USER}/.ssh/id_ed25519,allow_other,uid=1000,gid=1000 0 0

set -euo pipefail

REMOTE_HOST="${HUGIN_HOST:?Set HUGIN_HOST to the IP or hostname of the remote node}"
REMOTE_USER="${DEPLOY_USER:-yggdrasil}"
REMOTE_PATH="${REMOTE_PATH:-/home/${DEPLOY_USER:-yggdrasil}/repos}"
MOUNT_POINT="/mnt/hugin-repos"
SSH_KEY="${HOME}/.ssh/id_ed25519"

usage() {
    echo "Usage: $0 [mount|umount|status]"
    echo ""
    echo "  mount   — create mount point and mount via SSHFS (default)"
    echo "  umount  — unmount /mnt/hugin-repos"
    echo "  status  — show whether the mount is active"
    exit 1
}

cmd="${1:-mount}"

case "$cmd" in
    mount)
        if mountpoint -q "$MOUNT_POINT"; then
            echo "Already mounted at $MOUNT_POINT"
            exit 0
        fi

        if [ ! -d "$MOUNT_POINT" ]; then
            echo "Creating mount point $MOUNT_POINT ..."
            mkdir -p "$MOUNT_POINT"
        fi

        echo "Mounting ${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_PATH} -> ${MOUNT_POINT} ..."
        sshfs \
            -o IdentityFile="$SSH_KEY" \
            -o reconnect \
            -o ServerAliveInterval=15 \
            -o ServerAliveCountMax=3 \
            -o allow_other \
            -o uid="$(id -u "${LOCAL_USER:-$(whoami)}" 2>/dev/null || id -u)" \
            -o gid="$(id -g "${LOCAL_USER:-$(whoami)}" 2>/dev/null || id -g)" \
            "${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_PATH}" \
            "$MOUNT_POINT"

        echo "Mounted successfully."
        echo ""
        echo "To enable Huginn indexing of Hugin repos, uncomment the"
        echo "  - \"/mnt/hugin-repos\" line in configs/huginn/config.yaml"
        echo "and restart the huginn service on Hugin."
        ;;

    umount|unmount)
        if ! mountpoint -q "$MOUNT_POINT"; then
            echo "$MOUNT_POINT is not mounted."
            exit 0
        fi
        echo "Unmounting $MOUNT_POINT ..."
        fusermount -u "$MOUNT_POINT" || umount "$MOUNT_POINT"
        echo "Unmounted."
        ;;

    status)
        if mountpoint -q "$MOUNT_POINT"; then
            echo "MOUNTED: $MOUNT_POINT"
            df -h "$MOUNT_POINT"
        else
            echo "NOT MOUNTED: $MOUNT_POINT"
        fi
        ;;

    *)
        usage
        ;;
esac
