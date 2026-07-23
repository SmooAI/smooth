#!/usr/bin/env bash
# Pearl th-a63c22 — the working `msb run` invocation shape for booting a
# smooth-operator LocalServer inside a microsandbox microVM.
#
# This is the exact per-task shape an `--isolation microvm` backend in
# crates/smooth-bench/src/engine.rs would emit instead of spawning a host
# process. Verified against msb 0.4.6 (libkrun) on macOS arm64.
#
# Proven properties (spike, 2026-07-23):
#   * -v host:guest      — bidirectional file sharing (agent edits land on host)
#   * -p HOST:GUEST      — host reaches the in-VM server over the forwarded port
#   * egress allowlist   — default-deny + one allow rule; everything else fails
#                          to even resolve. Ingress (the -p forward) is
#                          UNAFFECTED by --net-default-egress deny (verified).
#   * --secret K=V@HOST  — gateway key is only released to the allowed host
#
# GOTCHAS (cost real time in the spike — do not re-learn them):
#   1. `-d/--detach` SILENTLY IGNORES the command after `--`. It boots the
#      image entrypoint instead. Either use attached mode (as below, and let
#      the caller background it) or set the command via --entrypoint.
#   2. `msb exec` into a detached sandbox HANGS in 0.4.6, even for `echo`.
#      Do not build the integration on exec. Attached `msb run -- <cmd>` is
#      the reliable path, and it maps 1:1 onto how the bench already treats an
#      engine as a child process with a kill-on-drop guard.
#   3. `--script NAME=BODY` bodies need a `#!/bin/sh` shebang or the entrypoint
#      dies with ENOEXEC.
#
# Usage: run-engine-vm.sh <engine-binary-dir> <workspace-dir> [host-port] [guest-port]
set -euo pipefail

BIN_DIR="${1:?usage: run-engine-vm.sh <engine-binary-dir> <workspace-dir> [host-port] [guest-port]}"
WORKSPACE="${2:?missing workspace dir}"
HOST_PORT="${3:-18791}"
GUEST_PORT="${4:-8791}"
ENGINE_BIN="${ENGINE_BIN:-/opt/wsmini}"   # override with /opt/smooth-daemon once built
IMAGE="${IMAGE:-alpine}"                  # alpine is fine for a STATIC musl binary
NAME="${NAME:-smooth-bench-vm}"

# Egress: deny everything, then allow ONLY the LLM gateway. This is the
# load-bearing isolation — with this on, the agent physically cannot reach
# smoo-hub / prod / anything else; non-allowed domains fail at DNS.
GATEWAY_HOST="${GATEWAY_HOST:-llm.smoo.ai}"

exec msb run --name "$NAME" \
  -p "${HOST_PORT}:${GUEST_PORT}" \
  -v "${BIN_DIR}:/opt" \
  -v "${WORKSPACE}:/work" \
  -e "SMOOTH_ADDR=0.0.0.0:${GUEST_PORT}" \
  -e "SMOOTH_WORKSPACE=/work" \
  --net-default-egress deny \
  --net-rule "allow@${GATEWAY_HOST}:tcp:443" \
  ${SMOOAI_GATEWAY_KEY:+--secret "SMOOAI_GATEWAY_KEY=${SMOOAI_GATEWAY_KEY}@${GATEWAY_HOST}"} \
  "$IMAGE" -- "$ENGINE_BIN"
