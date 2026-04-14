#!/usr/bin/env bash
# Build the smooai-boardroom OCI image.
#
# Steps:
#   1. Cross-compile boardroom + smooth-dolt to aarch64-unknown-linux-musl
#      (delegates to build-boardroom.sh — incremental).
#   2. docker/podman build docker/Dockerfile.boardroom tagged with the
#      workspace version and `latest`.
#
# Usage:
#   scripts/build-boardroom-image.sh                # tags with workspace version
#   scripts/build-boardroom-image.sh v1.2.3         # explicit tag
#
# Environment:
#   SMOOTH_IMAGE_TOOL   `docker` (default) or `podman`
#   SMOOTH_IMAGE_REPO   image repository (default: smooai/boardroom)

set -euo pipefail

cd "$(dirname "$0")/.."

TOOL="${SMOOTH_IMAGE_TOOL:-docker}"
REPO="${SMOOTH_IMAGE_REPO:-smooai/boardroom}"

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

echo "==> Cross-compiling boardroom + smooth-dolt"
bash scripts/build-boardroom.sh

BOARDROOM_BIN="target/aarch64-unknown-linux-musl/release/boardroom"
DOLT_BIN="target/aarch64-unknown-linux-musl/release/smooth-dolt"

if [ ! -f "$BOARDROOM_BIN" ]; then
    echo "error: expected $BOARDROOM_BIN but build did not produce it" >&2
    exit 1
fi
if [ ! -f "$DOLT_BIN" ]; then
    echo "error: expected $DOLT_BIN but build did not produce it" >&2
    echo "       (boardroom needs smooth-dolt to be cross-compiled too; see build-boardroom.sh)" >&2
    exit 1
fi

echo "==> Building $REPO:$VERSION"
$TOOL build \
    --file docker/Dockerfile.boardroom \
    --tag "$REPO:$VERSION" \
    --tag "$REPO:latest" \
    --build-arg "VERSION=$VERSION" \
    --platform linux/arm64 \
    .

echo "==> Built $REPO:$VERSION ($REPO:latest)"
echo "    Publish with: $TOOL push $REPO:$VERSION && $TOOL push $REPO:latest"
