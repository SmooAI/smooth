#!/usr/bin/env bash
# Thin wrapper for the internal Smooth benchmark harness.
#
# Not part of the user-facing `th` binary. Requires Big Smooth to
# be running locally (`th up`) so the harness can dispatch tasks
# over WebSocket.
#
# Example:
#   scripts/bench.sh aider-polyglot --task grade-school --lang python
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo run --release -p smooai-smooth-bench -- "$@"
