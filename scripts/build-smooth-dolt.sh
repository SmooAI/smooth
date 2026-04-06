#!/usr/bin/env bash
# Build smooth-dolt: embedded Dolt engine for Smooth Pearls.
#
# smooth-dolt is a Go binary that wraps Dolt's embedded driver with a
# minimal CLI so `th pearls` can do full Dolt operations (init, SQL,
# commit, push, pull, clone, log, remote, gc) without requiring the
# external `dolt` CLI.
#
# Prerequisites:
#   - Go 1.21+ (go version)
#   - ICU (macOS: brew install icu4c)
#   - CGO enabled (default on macOS/Linux)
#
# The binary is placed at target/release/smooth-dolt alongside the
# Rust binaries.

set -euo pipefail

cd "$(dirname "$0")/../go/smooth-dolt"

# Detect ICU (required by Dolt's gozstd CGO dependency).
# On macOS, Homebrew installs ICU as keg-only; we need to point CGO at it.
ICU_PREFIX=""
if command -v brew >/dev/null 2>&1; then
    ICU_PREFIX=$(brew --prefix icu4c 2>/dev/null || true)
fi

if [ -n "$ICU_PREFIX" ] && [ -d "$ICU_PREFIX/include" ]; then
    export CGO_CFLAGS="-I${ICU_PREFIX}/include"
    export CGO_CPPFLAGS="-I${ICU_PREFIX}/include"
    export CGO_CXXFLAGS="-I${ICU_PREFIX}/include"
    export CGO_LDFLAGS="-L${ICU_PREFIX}/lib"
    echo "==> ICU found at ${ICU_PREFIX}"
fi

export CGO_ENABLED=1

echo "==> Building smooth-dolt (embedded Dolt engine)"
go build -tags gms_pure_go -o ../../target/release/smooth-dolt .

BIN="../../target/release/smooth-dolt"
SIZE=$(wc -c < "$BIN" | tr -d ' ')
echo "==> Built smooth-dolt ($(( SIZE / 1024 / 1024 )) MiB)"
echo "    Ships alongside th for pearl operations."
