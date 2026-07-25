#!/usr/bin/env bash
# Pearl th-a63c22 — prove the microVM's egress allowlist is actually enforced:
# the allowed gateway host resolves, everything else does not.
#
# Same net flags as run-daemon-vm.sh, in a throwaway VM. Uses `getent hosts`
# (glibc, always present in debian) so no package install is needed — the spike
# established that non-allowed domains fail at DNS.
#
# Usage: ./check-egress.sh [allowed-host] [denied-host]
set -euo pipefail

ALLOWED="${1:-llm.smoo.ai}"
DENIED="${2:-api.smoo.ai}"
NAME="${NAME:-smooth-egress-check}"

msb run --name "$NAME" \
  --net-default-egress deny \
  --net-rule "allow@${ALLOWED}:tcp:443" \
  debian -- /bin/sh -c "
    echo '--- ALLOWED: ${ALLOWED}';
    getent hosts ${ALLOWED} && echo 'RESOLVED (expected)' || echo 'BLOCKED (UNEXPECTED)';
    echo '--- DENIED: ${DENIED}';
    getent hosts ${DENIED} && echo 'RESOLVED (UNEXPECTED)' || echo 'BLOCKED (expected)';
  "
