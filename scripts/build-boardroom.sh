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
    -p smooth-bigsmooth --bin boardroom --no-default-features

BIN="target/aarch64-unknown-linux-musl/release/boardroom"
if [ ! -f "$BIN" ]; then
    echo "error: expected binary at $BIN but it wasn't produced" >&2
    exit 1
fi

SIZE=$(wc -c < "$BIN" | tr -d ' ')
echo "==> Built $BIN ($(( SIZE / 1024 / 1024 )) MiB, statically linked aarch64 ELF)"
echo "    Bootstrap Bill mounts this binary into the Boardroom VM at runtime."
