#!/usr/bin/env bash
# Build (and optionally push) the Smooth single-VM image.
#
# Pearl th-893801 Phase 2 iter-4f. This is the long-lived
# sandbox `th up` boots. Contains the boardroom binary +
# bundled CLIs (gh, aws, gcloud, az, kubectl, docker) + mise
# for language toolchains. State persists in the /root volume
# across `th down` / `th up` cycles.
#
# Steps:
#   1. Cross-compile boardroom + smooth-operator-runner +
#      smooth-dolt to aarch64-unknown-linux-musl (delegates
#      to build-boardroom.sh + build-operator-runner.sh).
#   2. docker/podman build docker/Dockerfile.smooth-vm.
#   3. If --push is passed, push both tags.
#
# Usage:
#   scripts/build-smooth-vm-image.sh                 # build, workspace version tag
#   scripts/build-smooth-vm-image.sh v1.2.3          # explicit tag
#   scripts/build-smooth-vm-image.sh --push          # build + push
#   scripts/build-smooth-vm-image.sh v1.2.3 --push   # both
#
# Environment:
#   SMOOTH_IMAGE_TOOL   `docker` (default) or `podman`
#   SMOOTH_IMAGE_REPO   default `ghcr.io/smooai/smooth-vm`

set -euo pipefail

cd "$(dirname "$0")/.."

TOOL="${SMOOTH_IMAGE_TOOL:-docker}"
REPO="${SMOOTH_IMAGE_REPO:-ghcr.io/smooai/smooth-vm}"

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

echo "==> Cross-compiling smooth-operator-runner"
bash scripts/build-operator-runner.sh

BOARDROOM_BIN="target/aarch64-unknown-linux-musl/release/boardroom"
RUNNER_BIN="target/aarch64-unknown-linux-musl/release/smooth-operator-runner"
DOLT_BIN="target/aarch64-unknown-linux-musl/release/smooth-dolt"

for bin in "$BOARDROOM_BIN" "$RUNNER_BIN" "$DOLT_BIN"; do
    if [ ! -f "$bin" ]; then
        echo "error: expected $bin but build did not produce it" >&2
        exit 1
    fi
done

echo "==> Building $REPO:$VERSION"
$TOOL build \
    --file docker/Dockerfile.smooth-vm \
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
