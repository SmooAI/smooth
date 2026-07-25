#!/usr/bin/env bash
# Pearl th-a63c22 — build the REAL `smooth-daemon` as a LINUX aarch64 binary,
# inside a container, so it can boot in a microsandbox microVM.
#
# Only the standalone daemon binary is built (package `smooai-smooth-daemon`,
# bin `smooth-daemon`). `th` itself is NOT built — that would drag in dolt /
# the web bundle / the TUI for no benefit here; `th` merely spawns the daemon.
#
# Caching: the apt/toolchain layer lives in the builder IMAGE; the cargo
# registry, git checkouts, and target dir live in NAMED VOLUMES. The host's
# ~/.cargo and ./target are never touched, so this cannot poison macOS builds
# (see MEMORY: "Shared cargo target poisoned by deleted worktree").
#
# Result is a GLIBC binary -> boot it in `debian`, NOT alpine.
#
# Usage: ./build-linux-daemon.sh [--clean]
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
OUT="${OUT:-$HERE/vmbin}"
IMAGE_TAG="${IMAGE_TAG:-smooth-linux-builder:bookworm}"
PLATFORM="${PLATFORM:-linux/arm64}"   # native under Docker Desktop on Apple Silicon
VOL_PREFIX="${VOL_PREFIX:-smooth-msb}"

if [[ "${1:-}" == "--clean" ]]; then
    echo "==> dropping cache volumes"
    docker volume rm -f "${VOL_PREFIX}-registry" "${VOL_PREFIX}-git" "${VOL_PREFIX}-target" >/dev/null || true
fi

mkdir -p "$OUT"

echo "==> building builder image ($PLATFORM)"
docker build --platform "$PLATFORM" -t "$IMAGE_TAG" -f "$HERE/Dockerfile.linux-builder" "$HERE"

echo "==> cargo build --release -p smooai-smooth-daemon --bin smooth-daemon"
docker run --rm --platform "$PLATFORM" \
    -v "$REPO:/src" \
    -v "${VOL_PREFIX}-registry:/usr/local/cargo/registry" \
    -v "${VOL_PREFIX}-git:/usr/local/cargo/git" \
    -v "${VOL_PREFIX}-target:/target" \
    "$IMAGE_TAG" \
    bash -euc '
        cargo build --release -p smooai-smooth-daemon --bin smooth-daemon
        # Copy out through the bind mount; /target is a volume the host cannot see.
        install -D -m 0755 /target/release/smooth-daemon /src/scripts/msb-spike/vmbin/smooth-daemon
    '

file "$OUT/smooth-daemon"
ls -lh "$OUT/smooth-daemon"
echo "NOTE: glibc binary — boot with IMAGE=debian, not alpine."
