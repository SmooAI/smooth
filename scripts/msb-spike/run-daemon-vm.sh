#!/usr/bin/env bash
# Pearl th-a63c22 — boot the REAL linux `smooth-daemon` inside a microsandbox
# microVM, with default-deny egress + a single allow rule for the LLM gateway.
#
# This is the exact per-task `msb run` shape a `--isolation microvm` backend
# would emit. Verified against msb 0.4.6 (libkrun) on macOS arm64.
#
# GOTCHAS (already paid for — do not re-learn):
#   1. `-d/--detach` SILENTLY IGNORES the command after `--` (boots the image
#      entrypoint instead). Use attached `msb run -- <cmd>` and background it
#      from the shell.
#   2. `msb exec` into a detached sandbox HANGS in 0.4.6. Don't build on it.
#   3. `--script NAME=BODY` bodies need a `#!/bin/sh` shebang (else ENOEXEC).
#
# The daemon is a GLIBC binary -> `debian`, NOT alpine.
#
# Usage: run-daemon-vm.sh <workspace-dir> [host-port] [guest-port]
# Env:   SMOOAI_GATEWAY_KEY (required for a real LLM turn), SMOOTH_LOCAL_TOKEN
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="${BIN_DIR:-$HERE/vmbin}"
WORKSPACE="${1:?usage: run-daemon-vm.sh <workspace-dir> [host-port] [guest-port]}"
HOST_PORT="${2:-18791}"
GUEST_PORT="${3:-8791}"
IMAGE="${IMAGE:-debian}"
NAME="${NAME:-smooth-bench-vm}"
GATEWAY_HOST="${GATEWAY_HOST:-llm.smoo.ai}"
GATEWAY_URL="${SMOOAI_GATEWAY_URL:-https://${GATEWAY_HOST}/v1}"
# strict_auth is ON in the local flavor — the /ws connection needs ?token=.
: "${SMOOTH_LOCAL_TOKEN:=spike-token}"

exec msb run --name "$NAME" \
  -p "${HOST_PORT}:${GUEST_PORT}" \
  -v "${BIN_DIR}:/opt" \
  -v "${WORKSPACE}:/work" \
  -e "SMOOTH_ADDR=0.0.0.0:${GUEST_PORT}" \
  -e "SMOOTH_WORKSPACE=/work" \
  -e "SMOOTH_LOCAL_TOKEN=${SMOOTH_LOCAL_TOKEN}" \
  -e "SMOOAI_GATEWAY_URL=${GATEWAY_URL}" \
  -e "SMOOTH_OPERATOR_DB=/tmp/operator-storage.db" \
  -e "SMOOTH_TAILSCALE_SERVE=0" \
  -e "HOME=/root" \
  -e "RUST_LOG=${RUST_LOG:-info}" \
  --net-default-egress deny \
  --net-rule "allow@${GATEWAY_HOST}:tcp:443" \
  ${SMOOAI_GATEWAY_KEY:+--secret "SMOOAI_GATEWAY_KEY=${SMOOAI_GATEWAY_KEY}@${GATEWAY_HOST}"} \
  "$IMAGE" -- /opt/smooth-daemon
