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

# Pearl th-a49716: support --if-stale for the install:th fast path.
# When passed, skip the build entirely if target/release/smooth-dolt
# already exists AND every *.go / go.mod / go.sum under
# go/smooth-dolt/ is older than the binary. Lets `pnpm install:th`
# always invoke this script without paying the ~30s Go build cost
# on a hot install; `pnpm install:th:full` skips the flag to force
# an unconditional rebuild.
IF_STALE=0
for arg in "$@"; do
    case "$arg" in
        --if-stale) IF_STALE=1 ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

cd "$(dirname "$0")/../go/smooth-dolt"

BIN_REL="../../target/release/smooth-dolt"
if [ "$IF_STALE" = "1" ] && [ -f "$BIN_REL" ]; then
    # Compare mtimes: if every Go source is older than the binary,
    # there's nothing to do. `find -newer` lists files strictly
    # newer than the reference; empty output → cache hit.
    if [ -z "$(find . \( -name '*.go' -o -name 'go.mod' -o -name 'go.sum' \) -newer "$BIN_REL" -print -quit)" ]; then
        echo "==> smooth-dolt is up-to-date (no source changes since last build)"
        exit 0
    fi
fi

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
