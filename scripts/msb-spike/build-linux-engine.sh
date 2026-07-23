#!/usr/bin/env bash
# Pearl th-a63c22 — produce a LINUX aarch64 binary that msb can boot.
#
# Two modes:
#   ./build-linux-engine.sh wsmini   — cross-compile the minimal WS fixture
#                                      (pure Rust, ~13s, PROVEN working)
#   ./build-linux-engine.sh daemon   — the real `smooth-daemon` LocalServer
#                                      (BLOCKED on cross-compile, see below)
#
# WHY TWO MODES — spike finding (2026-07-23):
#
# Cross-compiling with cargo-zigbuild to aarch64-unknown-linux-musl works
# perfectly for pure-Rust trees; `wsmini` builds to a 945KB static ELF in 13s
# and runs unmodified in a stock alpine microVM.
#
# The REAL daemon does NOT cross-compile, because openssl-sys enters the graph
# from two independent roots:
#
#   web-push 0.11 -> isahc -> curl -> curl-sys + openssl-sys   (direct dep)
#   reqwest -> hyper-tls -> native-tls -> openssl-sys          (transitive)
#
# The first needs libcurl AND openssl for the target; the second needs openssl.
# The reqwest root is NOT locally fixable: reqwest's default-tls is enabled by
# the external git crates (smooai-smooth-operator, smooai-client-shared), and
# cargo feature unification pulls native-tls in regardless of what this repo
# sets. Removing it means an upstream change in those repos.
#
# => The recommended path is `daemon` mode below: build INSIDE a linux
#    container, where openssl/libcurl are one apt-get away and NOTHING in the
#    dependency tree has to change. Both git deps are PUBLIC, so the container
#    needs no credentials. On Apple Silicon linux/arm64 is native under Docker
#    Desktop, so there is no qemu penalty.
set -euo pipefail

MODE="${1:-wsmini}"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
OUT="${OUT:-$HERE/vmbin}"
mkdir -p "$OUT"

# Always build into an ISOLATED target dir. A shared cargo target dir gets
# poisoned by other worktrees (stale build-script paths -> phantom protoc
# failures) and parallel agents deadlock on the build lock.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/th-msb-spike-target}"

case "$MODE" in
wsmini)
    # PROVEN path. Requires: rustup target add aarch64-unknown-linux-musl
    # and cargo-zigbuild (zig supplies the musl cross-linker).
    cd "$HERE/wsmini"
    cargo zigbuild --release --target aarch64-unknown-linux-musl
    cp "$CARGO_TARGET_DIR/aarch64-unknown-linux-musl/release/wsmini" "$OUT/wsmini"
    file "$OUT/wsmini"
    ;;
daemon)
    # RECOMMENDED path for the real engine. Produces a glibc binary, so boot it
    # in a debian/ubuntu microVM (NOT alpine).
    # Note: mounts a container-local cargo cache volume so the host's
    # ~/.cargo is never written to by a linux container.
    docker run --rm --platform linux/arm64 \
        -v "$REPO:/src" \
        -v smooth-msb-spike-cargo:/usr/local/cargo/registry \
        -w /src \
        rust:bookworm \
        bash -c '
            set -e
            apt-get update -qq
            apt-get install -y -qq pkg-config libssl-dev libcurl4-openssl-dev protobuf-compiler
            cargo build --release -p smooai-smooth-daemon --bin smooth-daemon
        '
    cp "$REPO/target/release/smooth-daemon" "$OUT/smooth-daemon"
    file "$OUT/smooth-daemon"
    echo "NOTE: glibc binary — boot with IMAGE=debian, not alpine."
    ;;
*)
    echo "usage: $0 [wsmini|daemon]" >&2
    exit 2
    ;;
esac
