#!/usr/bin/env bash
# protect-postgres.sh — Warns when Docker commands might destroy PostgreSQL data.
# PostToolUse hook for Bash commands.

set -euo pipefail

TOOL_INPUT="${1:-}"

# Check for dangerous volume operations
if echo "$TOOL_INPUT" | grep -qE 'docker\s+compose\s+down\s+.*-v|docker\s+volume\s+rm\s+.*smooth-pgdata|docker\s+compose\s+.*--volumes'; then
    echo "WARNING: This command may destroy the smooth PostgreSQL data volume."
    echo "The smooth-pgdata volume contains leader memory, checkpoints, and auth data."
    echo ""
    echo "To safely stop the stack:  docker compose -f docker/docker-compose.yml down"
    echo "To backup first:           bash docker/postgres/backup.sh"
    echo "To intentionally destroy:  Use 'th db destroy --confirm'"
    exit 1
fi

exit 0
