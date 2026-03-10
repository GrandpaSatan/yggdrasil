#!/usr/bin/env bash
# Database backup script for Hades (PostgreSQL + Qdrant).
# Run as a cron job at 3 AM daily:
#   0 3 * * * /opt/yggdrasil/deploy/backup-hades.sh >> /var/log/yggdrasil-backup.log 2>&1
#
# Prerequisites: postgresql-client must be installed on the backup executor.
#   apt install postgresql-client
set -euo pipefail

BACKUP_DIR="/mnt/raven/yggdrasil-backups"
DATE=$(date +%Y%m%d_%H%M%S)
RETENTION_DAYS=7

# Ensure backup directory exists.
mkdir -p "${BACKUP_DIR}"

echo "[${DATE}] Starting Yggdrasil backup"

# PostgreSQL dump (yggdrasil schema only).
# PostgreSQL runs as a Docker container on Munin (localhost:5432).
# Produces a custom-format dump suitable for pg_restore.
pg_dump -h 127.0.0.1 -U jhernandez -d yggdrasil \
  --schema=yggdrasil --format=custom \
  -f "${BACKUP_DIR}/pg_yggdrasil_${DATE}.dump"

echo "[${DATE}] PostgreSQL dump complete: pg_yggdrasil_${DATE}.dump"

# Qdrant snapshots.
# The Qdrant snapshot API triggers a snapshot and returns the snapshot file
# metadata. The snapshot is stored on the Qdrant server; this records the
# trigger event. For full off-node backup, rsync from the Qdrant data dir.
for collection in engrams code_chunks; do
    response=$(curl -s -X POST "http://REDACTED_HADES_IP:6333/collections/${collection}/snapshots")
    echo "[${DATE}] Qdrant snapshot triggered for ${collection}: ${response}"
    # Save the response (contains snapshot name/path) for reference.
    echo "${response}" > "${BACKUP_DIR}/qdrant_${collection}_${DATE}.snapshot"
done

# Cleanup old backups (older than RETENTION_DAYS days).
find "${BACKUP_DIR}" -name "pg_yggdrasil_*.dump" -mtime "+${RETENTION_DAYS}" -delete
find "${BACKUP_DIR}" -name "qdrant_*.snapshot" -mtime "+${RETENTION_DAYS}" -delete

echo "[${DATE}] Backup completed. Backup directory: ${BACKUP_DIR}"
echo "[${DATE}] Disk usage: $(du -sh "${BACKUP_DIR}" | cut -f1)"
