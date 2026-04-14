#!/usr/bin/env bash
# Cross-compile the Boardroom binary for aarch64-unknown-linux-musl.
#
# The Boardroom binary runs Big Smooth *inside* its own microVM (alongside
# the Boardroom cast: Wonk, Goalie, Narc, Scribe, Archivist). Bootstrap
# Bill spawns the Boardroom VM on the host, bind-mounts this binary in,
# and execs it on boot.
#
# One-time dev setup (share with build-operator-runner.sh):
#   rustup target add aarch64-unknown-linux-musl
#   cargo install --locked cargo-zigbuild
#   pip3 install ziglang
#
# After that, run this script whenever smooth-bigsmooth or any of its
# transitive deps change. The build is incremental.

set -euo pipefail

cd "$(dirname "$0")/.."

if [ -d "$HOME/Library/Python" ]; then
    for pydir in "$HOME/Library/Python"/*/bin; do
        if [ -d "$pydir" ]; then
            export PATH="$pydir:$PATH"
        fi
    done
fi

if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    echo "error: cargo-zigbuild not installed" >&2
    echo "  cargo install --locked cargo-zigbuild" >&2
    exit 1
fi

if ! command -v python-zig >/dev/null 2>&1; then
    echo "error: ziglang (pip package providing python-zig) not installed" >&2
    echo "  pip3 install ziglang" >&2
    exit 1
fi

echo "==> Cross-compiling boardroom binary for aarch64-unknown-linux-musl"
# `--no-default-features` strips the `direct-sandbox` feature from
# smooth-bigsmooth (and in turn strips the `server` feature from
# smooth-bootstrap-bill and the whole `microsandbox` tree). Inside the
# Boardroom VM, Big Smooth talks to Bill over TCP; it never links
# microsandbox.
cargo zigbuild --target aarch64-unknown-linux-musl --release \
    -p smooai-smooth-bigsmooth --bin boardroom --no-default-features

BIN="target/aarch64-unknown-linux-musl/release/boardroom"
if [ ! -f "$BIN" ]; then
    echo "error: expected binary at $BIN but it wasn't produced" >&2
    exit 1
fi

SIZE=$(wc -c < "$BIN" | tr -d ' ')
echo "==> Built $BIN ($(( SIZE / 1024 / 1024 )) MiB, statically linked aarch64 ELF)"

# Also cross-compile smooth-dolt for the Boardroom VM.
# smooth-dolt is a Go binary; Go handles cross-compilation natively.
DOLT_BIN="target/aarch64-unknown-linux-musl/release/smooth-dolt"
echo "==> Cross-compiling smooth-dolt for linux/arm64"
if [ -d "go/smooth-dolt" ]; then
    cd go/smooth-dolt
    # gozstd requires CGO. For cross-compile to linux/arm64, use zig as the
    # C cross-compiler (same trick cargo-zigbuild uses).
    # Find zig — may be installed via pip (ziglang package) in a Python user dir
    ZIG_BIN=""
    if command -v zig >/dev/null 2>&1; then
        ZIG_BIN="zig"
    else
        for pydir in "$HOME/Library/Python"/*/lib/python/site-packages/ziglang; do
            if [ -f "$pydir/zig" ]; then
                ZIG_BIN="$pydir/zig"
                break
            fi
        done
    fi
    if [ -n "$ZIG_BIN" ]; then
        ZIG_CC="$ZIG_BIN cc -target aarch64-linux-musl"
        CC="$ZIG_CC" CGO_ENABLED=1 GOOS=linux GOARCH=arm64 \
            go build -tags gms_pure_go -o "../../$DOLT_BIN" . 2>&1 || {
            # Fallback: try without CGO (will fail if gozstd is needed at runtime)
            echo "  zig cross-compile failed, trying CGO_ENABLED=0..." >&2
            GOOS=linux GOARCH=arm64 CGO_ENABLED=0 go build -tags "gms_pure_go nozstd" -o "../../$DOLT_BIN" . 2>&1 || {
                echo "warning: smooth-dolt cross-compile failed (pearl store will not work in Boardroom VM)" >&2
                cd ../..
            }
        }
    else
        echo "warning: zig not found, cannot cross-compile smooth-dolt (install: pip3 install ziglang)" >&2
    fi  # ZIG_BIN check
    cd ../.. 2>/dev/null || true
    if [ -f "$DOLT_BIN" ]; then
        DOLT_SIZE=$(wc -c < "$DOLT_BIN" | tr -d ' ')
        echo "==> Built $DOLT_BIN ($(( DOLT_SIZE / 1024 / 1024 )) MiB)"
    fi
else
    echo "warning: go/smooth-dolt not found, skipping smooth-dolt build" >&2
fi

echo "    Bootstrap Bill mounts these binaries into the Boardroom VM at runtime."
