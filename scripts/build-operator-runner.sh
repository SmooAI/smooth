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

# Resolve cargo's target dir dynamically — pearl th-target-bloat
# (2026-05-12) moved the workspace to a shared dir at
# ~/.cargo/shared-target via ~/.cargo/config.toml. Hardcoding
# "target/..." breaks under that config. `cargo metadata` returns
# the actual path cargo would write to, regardless of overrides.
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['target_directory'])")
if [ -z "$TARGET_DIR" ]; then
    TARGET_DIR="target"
fi

BIN="$TARGET_DIR/aarch64-unknown-linux-musl/release/smooth-operator-runner"
if [ ! -f "$BIN" ]; then
    echo "error: expected binary at $BIN but it wasn't produced" >&2
    exit 1
fi

SIZE=$(wc -c < "$BIN" | tr -d ' ')
echo "==> Built $BIN ($(( SIZE / 1024 / 1024 )) MiB, statically linked aarch64 ELF)"

# Mirror the freshly-built binary into ~/.smooth/runner-bin/ as well.
# Big Smooth's find_operator_runner_binary walks up from
# CARGO_MANIFEST_DIR looking for `target/aarch64-unknown-linux-musl/
# release/smooth-operator-runner`. A long-running `th up` daemon
# compiled in a worktree whose target/ no longer holds the binary
# can silently fall back to a stale `~/.smooth/runner-bin/` copy
# left by an earlier setup, which means new runner code (e.g. the
# coding_workflow role gate) never reaches the sandbox even after
# `bash scripts/build-operator-runner.sh`. Keeping both paths in
# lockstep removes that footgun.
RUNNER_BIN_DIR="$HOME/.smooth/runner-bin"
mkdir -p "$RUNNER_BIN_DIR"
cp "$BIN" "$RUNNER_BIN_DIR/smooth-operator-runner"
echo "==> Synced $RUNNER_BIN_DIR/smooth-operator-runner"
echo "    Big Smooth will mount this binary into each sandbox at runtime."
