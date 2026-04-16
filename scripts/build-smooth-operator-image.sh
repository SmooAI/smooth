#!/usr/bin/env bash
# Build (and optionally push) the Smooth Operator OCI image.
#
# One unified Smooth Operator image with mise baked in — the agent
# handles its own toolchain install at runtime (node, python, rust,
# go, …) and persists installs into /opt/smooth/cache/mise via the
# project-scoped bind mount.
#
# Default publishing target: ghcr.io/smooai/smooth-operator (public).
# Microsandbox pulls directly from this registry — no Docker Hub
# hop, no local-only image gotchas.
#
# Steps:
#   1. Cross-compile smooth-operator-runner to aarch64-unknown-linux-musl
#      (delegates to build-operator-runner.sh — incremental).
#   2. docker/podman build docker/Dockerfile.smooth-operator tagged
#      with the workspace version and `latest`.
#   3. If --push is passed, push both tags to the registry.
#
# Usage:
#   scripts/build-smooth-operator-image.sh                  # build only, tag with workspace version
#   scripts/build-smooth-operator-image.sh v1.2.3           # explicit tag, build only
#   scripts/build-smooth-operator-image.sh --push           # build + push version + latest
#   scripts/build-smooth-operator-image.sh v1.2.3 --push    # build v1.2.3 + push
#
# Environment:
#   SMOOTH_IMAGE_TOOL   `docker` (default) or `podman`
#   SMOOTH_IMAGE_REPO   image repository (default: ghcr.io/smooai/smooth-operator)
#
# Pushing to ghcr.io requires a token with `write:packages` scope:
#     gh auth refresh -h github.com -s write:packages,read:packages
#     gh auth token | $SMOOTH_IMAGE_TOOL login ghcr.io -u "$USER" --password-stdin

set -euo pipefail

cd "$(dirname "$0")/.."

TOOL="${SMOOTH_IMAGE_TOOL:-docker}"
REPO="${SMOOTH_IMAGE_REPO:-ghcr.io/smooai/smooth-operator}"

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

echo "==> Cross-compiling smooth-operator-runner"
bash scripts/build-operator-runner.sh

RUNNER_BIN="target/aarch64-unknown-linux-musl/release/smooth-operator-runner"
if [ ! -f "$RUNNER_BIN" ]; then
    echo "error: expected $RUNNER_BIN but build did not produce it" >&2
    exit 1
fi

echo "==> Building $REPO:$VERSION"
$TOOL build \
    --file docker/Dockerfile.smooth-operator \
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
