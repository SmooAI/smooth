#!/usr/bin/env bash
# Build (and optionally push) the Boardroom OCI image.
#
# Default publishing target: ghcr.io/smooai/boardroom (public).
#
# Steps:
#   1. Cross-compile boardroom + smooth-dolt to aarch64-unknown-linux-musl
#      (delegates to build-boardroom.sh — incremental).
#   2. docker/podman build docker/Dockerfile.boardroom tagged with the
#      workspace version and `latest`.
#   3. If --push is passed, push both tags to the registry.
#
# Usage:
#   scripts/build-boardroom-image.sh                 # build only, tag with workspace version
#   scripts/build-boardroom-image.sh v1.2.3          # explicit tag, build only
#   scripts/build-boardroom-image.sh --push          # build + push
#   scripts/build-boardroom-image.sh v1.2.3 --push   # build v1.2.3 + push
#
# Environment:
#   SMOOTH_IMAGE_TOOL   `docker` (default) or `podman`
#   SMOOTH_IMAGE_REPO   image repository (default: ghcr.io/smooai/boardroom)
#
# Pushing to ghcr.io requires a token with `write:packages` scope:
#     gh auth refresh -h github.com -s write:packages,read:packages
#     gh auth token | $SMOOTH_IMAGE_TOOL login ghcr.io -u "$USER" --password-stdin

set -euo pipefail

cd "$(dirname "$0")/.."

TOOL="${SMOOTH_IMAGE_TOOL:-docker}"
REPO="${SMOOTH_IMAGE_REPO:-ghcr.io/smooai/boardroom}"

if ! command -v "$TOOL" >/dev/null 2>&1; then
    echo "error: $TOOL not found on PATH (override with SMOOTH_IMAGE_TOOL)" >&2
    exit 1
fi

PUSH=0
VERSION=""
for arg in "$@"; do
    if [ "$arg" = "--push" ]; then
        PUSH=1
    elif [ -z "$VERSION" ]; then
        VERSION="$arg"
    else
        echo "error: unexpected argument '$arg'" >&2
        exit 1
    fi
done

if [ -z "$VERSION" ]; then
    VERSION=$(awk -F '"' '/^version/ {print $2; exit}' Cargo.toml)
    if [ -z "${VERSION:-}" ]; then
        echo "error: could not read workspace version from Cargo.toml" >&2
        exit 1
    fi
fi

echo "==> Cross-compiling boardroom + smooth-dolt"
bash scripts/build-boardroom.sh

# Resolve the real cargo target dir (shared dirs are configured
# in ~/.cargo/config.toml; see pearl th-target-bloat).
TARGET_DIR=$(cargo metadata --no-deps --format-version 1 2>/dev/null | python3 -c "import sys, json; print(json.load(sys.stdin)['target_directory'])")
if [ -z "$TARGET_DIR" ]; then
    TARGET_DIR="./target"
fi
SRC_BOARDROOM="$TARGET_DIR/aarch64-unknown-linux-musl/release/boardroom"
SRC_DOLT="$TARGET_DIR/aarch64-unknown-linux-musl/release/smooth-dolt"

# Dockerfile COPY paths are relative to the build context; mirror
# the binaries under ./target/ so the Dockerfile keeps working
# even when cargo's target dir lives elsewhere.
mkdir -p target/aarch64-unknown-linux-musl/release
BOARDROOM_BIN="target/aarch64-unknown-linux-musl/release/boardroom"
DOLT_BIN="target/aarch64-unknown-linux-musl/release/smooth-dolt"
cp -f "$SRC_BOARDROOM" "$BOARDROOM_BIN"
cp -f "$SRC_DOLT" "$DOLT_BIN"

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

if [ "$PUSH" -eq 1 ]; then
    echo "==> Pushing $REPO:$VERSION"
    $TOOL push "$REPO:$VERSION"
    echo "==> Pushing $REPO:latest"
    $TOOL push "$REPO:latest"
    echo "==> Pushed $REPO:$VERSION ($REPO:latest)"
else
    echo "    Push with: $TOOL push $REPO:$VERSION && $TOOL push $REPO:latest"
    echo "    Or re-run with --push."
fi
