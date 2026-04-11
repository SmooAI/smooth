#!/usr/bin/env bash
# Cross-compile smooth-operator-runner for aarch64-unknown-linux-musl.
#
# This is the binary that runs *inside* each Smooth Operator microVM. Big
# Smooth mounts it into the sandbox and execs it with a task + LLM config
# provided via environment variables. The runner hosts the agent loop,
# NarcHook tool surveillance, and streams JSON-lines `AgentEvent`s on stdout.
#
# One-time dev setup (needed on a fresh clone):
#   rustup target add aarch64-unknown-linux-musl
#   cargo install cargo-zigbuild
#   pip3 install ziglang          # cargo-zigbuild looks up zig via `python-zig`
#
# After that, run this script whenever `crates/smooth-operator-runner/` or
# any of its transitive deps change. The build is incremental.

set -euo pipefail

cd "$(dirname "$0")/.."

# cargo-zigbuild invokes `python-zig` to find its embedded zig install.
# On macOS / pip user installs, that script lands under ~/Library/Python
# which is not usually on $PATH — prepend it if present.
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

echo "==> Cross-compiling smooth-operator-runner for aarch64-unknown-linux-musl"
cargo zigbuild --target aarch64-unknown-linux-musl --release -p smooai-smooth-operator-runner

BIN="target/aarch64-unknown-linux-musl/release/smooth-operator-runner"
if [ ! -f "$BIN" ]; then
    echo "error: expected binary at $BIN but it wasn't produced" >&2
    exit 1
fi

SIZE=$(wc -c < "$BIN" | tr -d ' ')
echo "==> Built $BIN ($(( SIZE / 1024 / 1024 )) MiB, statically linked aarch64 ELF)"
echo "    Big Smooth will mount this binary into each sandbox at runtime."
