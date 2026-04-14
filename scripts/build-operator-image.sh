#!/usr/bin/env bash
# Build the smooai-operator OCI image.
#
# Steps:
#   1. Cross-compile smooth-operator-runner to aarch64-unknown-linux-musl
#      (delegates to build-operator-runner.sh — incremental).
#   2. docker/podman build docker/Dockerfile.operator tagged with the
#      workspace version and `latest`.
#
# Usage:
#   scripts/build-operator-image.sh                 # tags with workspace version
#   scripts/build-operator-image.sh v1.2.3          # explicit tag
#
# Environment:
#   SMOOTH_IMAGE_TOOL   `docker` (default) or `podman`
#   SMOOTH_IMAGE_REPO   image repository (default: smooai/operator)

set -euo pipefail

cd "$(dirname "$0")/.."

TOOL="${SMOOTH_IMAGE_TOOL:-docker}"
REPO="${SMOOTH_IMAGE_REPO:-smooai/operator}"

if ! command -v "$TOOL" >/dev/null 2>&1; then
    echo "error: $TOOL not found on PATH (override with SMOOTH_IMAGE_TOOL)" >&2
    exit 1
fi

# Resolve the tag: explicit arg wins, else workspace version from Cargo.toml.
if [ $# -ge 1 ]; then
    VERSION="$1"
else
    VERSION=$(awk -F '"' '/^version/ {print $2; exit}' Cargo.toml)
    if [ -z "${VERSION:-}" ]; then
        echo "error: could not read workspace version from Cargo.toml" >&2
        exit 1
    fi
fi

echo "==> Cross-compiling smooth-operator-runner"
bash scripts/build-operator-runner.sh

RUNNER_BIN="target/aarch64-unknown-linux-musl/release/smooth-operator-runner"
if [ ! -f "$RUNNER_BIN" ]; then
    echo "error: expected $RUNNER_BIN but build did not produce it" >&2
    exit 1
fi

echo "==> Building $REPO:$VERSION"
$TOOL build \
    --file docker/Dockerfile.operator \
    --tag "$REPO:$VERSION" \
    --tag "$REPO:latest" \
    --build-arg "VERSION=$VERSION" \
    --platform linux/arm64 \
    .

echo "==> Built $REPO:$VERSION ($REPO:latest)"
echo "    Publish with: $TOOL push $REPO:$VERSION && $TOOL push $REPO:latest"
