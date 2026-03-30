#!/usr/bin/env bash
set -euo pipefail

# Write OpenCode config from environment
if [[ -n "${OPENCODE_CONFIG:-}" ]]; then
    echo "$OPENCODE_CONFIG" > /root/.opencode.json
fi

# Start MCP tool server in background
if command -v smooth-tools-server &>/dev/null; then
    smooth-tools-server \
        --socket "${SMOOTH_MCP_SOCKET:-/tmp/smooth-tools.sock}" &
    echo "MCP tool server started on ${SMOOTH_MCP_SOCKET}"
fi

# Start OpenCode in server mode
exec opencode --server --host 0.0.0.0 --port 4096
