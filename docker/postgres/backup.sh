#!/usr/bin/env bash
# backup.sh — Backup smooth PostgreSQL database
# Usage: bash docker/postgres/backup.sh [backup_name]
# Backups are saved to docker/postgres/backups/

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKUP_DIR="$SCRIPT_DIR/backups"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_NAME="${1:-smooth_backup_$TIMESTAMP}"
BACKUP_FILE="$BACKUP_DIR/${BACKUP_NAME}.sql.gz"

mkdir -p "$BACKUP_DIR"

echo "Backing up smooth database..."

docker compose -f "$SCRIPT_DIR/../docker-compose.yml" exec -T postgres \
    pg_dump -U smooth smooth | gzip > "$BACKUP_FILE"

echo "Backup saved to: $BACKUP_FILE"
echo "Size: $(du -h "$BACKUP_FILE" | cut -f1)"

# Retain last 30 backups
cd "$BACKUP_DIR"
ls -1t *.sql.gz 2>/dev/null | tail -n +31 | xargs -r rm --
REMAINING=$(ls -1 *.sql.gz 2>/dev/null | wc -l)
echo "Total backups: $REMAINING"
