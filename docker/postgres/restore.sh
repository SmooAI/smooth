#!/usr/bin/env bash
# restore.sh ��� Restore smooth PostgreSQL database from backup
# Usage: bash docker/postgres/restore.sh <backup_file>

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKUP_FILE="${1:-}"

if [[ -z "$BACKUP_FILE" ]]; then
    echo "Usage: bash docker/postgres/restore.sh <backup_file>"
    echo ""
    echo "Available backups:"
    ls -1t "$SCRIPT_DIR/backups/"*.sql.gz 2>/dev/null || echo "  (none)"
    exit 1
fi

if [[ ! -f "$BACKUP_FILE" ]]; then
    # Try relative to backups dir
    BACKUP_FILE="$SCRIPT_DIR/backups/$BACKUP_FILE"
fi

if [[ ! -f "$BACKUP_FILE" ]]; then
    echo "Error: Backup file not found: $BACKUP_FILE"
    exit 1
fi

echo "WARNING: This will replace all data in the smooth database."
echo "Restoring from: $BACKUP_FILE"
read -p "Continue? (y/N) " -n 1 -r
echo

if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 0
fi

echo "Restoring..."

gunzip -c "$BACKUP_FILE" | docker compose -f "$SCRIPT_DIR/../docker-compose.yml" exec -T postgres \
    psql -U smooth smooth

echo "Restore complete."
