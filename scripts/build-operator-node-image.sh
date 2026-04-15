#!/usr/bin/env bash
# Build the smooai/operator-node OCI image — base smooai/operator
# variant with Node 20 + pnpm 10 baked in. Needed when the agent
# has to actually run JS/TS code in the sandbox (pnpm install,
# pnpm dev, vite, etc.), not just edit files.
#
# Usage:
#   scripts/build-operator-node-image.sh              # tag with workspace version
#   scripts/build-operator-node-image.sh v1.2.3       # explicit tag
#
# Environment:
#   SMOOTH_IMAGE_TOOL   `docker` (default) or `podman`
#   SMOOTH_IMAGE_REPO   image repo (default: smooai/operator-node)

set -euo pipefail

cd "$(dirname "$0")/.."

TOOL="${SMOOTH_IMAGE_TOOL:-docker}"
REPO="${SMOOTH_IMAGE_REPO:-smooai/operator-node}"

if ! command -v "$TOOL" >/dev/null 2>&1; then
    echo "error: $TOOL not found on PATH (override with SMOOTH_IMAGE_TOOL)" >&2
    exit 1
fi

if [ $# -ge 1 ]; then
    VERSION="$1"
else
    VERSION=$(awk -F '"' '/^version/ {print $2; exit}' Cargo.toml)
    if [ -z "${VERSION:-}" ]; then
        echo "error: could not read workspace version from Cargo.toml" >&2
        exit 1
    fi
fi

echo "==> Cross-compiling smooth-operator-runner (for the image)"
bash scripts/build-operator-runner.sh

RUNNER_BIN="target/aarch64-unknown-linux-musl/release/smooth-operator-runner"
if [ ! -f "$RUNNER_BIN" ]; then
    echo "error: expected $RUNNER_BIN but build did not produce it" >&2
    exit 1
fi

echo "==> Building $REPO:$VERSION"
$TOOL build \
    --file docker/Dockerfile.operator-node \
    --tag "$REPO:$VERSION" \
    --tag "$REPO:latest" \
    --build-arg "VERSION=$VERSION" \
    --platform linux/arm64 \
    .

echo "==> Built $REPO:$VERSION ($REPO:latest)"
echo "    Use via: SMOOTH_WORKER_IMAGE=$REPO:latest th up"
