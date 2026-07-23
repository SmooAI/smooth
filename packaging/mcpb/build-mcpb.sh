#!/usr/bin/env bash
# Build the Smooth .mcpb Desktop Extension bundle around a compiled `th` binary.
#
# Usage:
#   ./build-mcpb.sh [path-to-th-binary] [output.mcpb]
#
# Defaults: binary = ~/.cargo/bin/th, output = ./smooth.mcpb (next to this script).
#
# Staging: copies the binary to server/th beside manifest.json in a temp dir,
# copies icon.png if present (and wires the manifest `icon` key), then runs
# `npx @anthropic-ai/mcpb pack` to produce the bundle.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TH_BIN="${1:-$HOME/.cargo/bin/th}"
OUT="${2:-$SCRIPT_DIR/smooth.mcpb}"

# Absolute output path so it survives the `cd` into the stage dir.
case "$OUT" in
    /*) : ;;
    *) OUT="$(pwd)/$OUT" ;;
esac

die() { echo "error: $*" >&2; exit 1; }

[ -f "$TH_BIN" ] || die "th binary not found at '$TH_BIN'. Pass a path: ./build-mcpb.sh /path/to/th (build with 'pnpm install:th' or 'brew install SmooAI/tools/th')."
[ -x "$TH_BIN" ] || die "th binary at '$TH_BIN' is not executable."
command -v npx >/dev/null 2>&1 || die "npx not found. Install Node.js 18+ (https://nodejs.org) — the bundler is 'npx @anthropic-ai/mcpb'."

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE/server"
cp "$SCRIPT_DIR/manifest.json" "$STAGE/manifest.json"
cp "$TH_BIN" "$STAGE/server/th"
chmod +x "$STAGE/server/th"

# Optional icon: drop a 512x512 icon.png next to this script and it gets bundled.
if [ -f "$SCRIPT_DIR/icon.png" ]; then
    cp "$SCRIPT_DIR/icon.png" "$STAGE/icon.png"
    if command -v python3 >/dev/null 2>&1; then
        python3 - "$STAGE/manifest.json" <<'PY'
import json, sys
p = sys.argv[1]
with open(p) as f:
    m = json.load(f)
m["icon"] = "icon.png"
with open(p, "w") as f:
    json.dump(m, f, indent=4)
    f.write("\n")
PY
    else
        echo "warn: icon.png found but python3 missing — bundle will omit the 'icon' manifest key." >&2
    fi
fi

echo "Packing $OUT from:"
echo "  manifest: $SCRIPT_DIR/manifest.json"
echo "  binary:   $TH_BIN"

npx --yes @anthropic-ai/mcpb pack "$STAGE" "$OUT"

echo "Built: $OUT"
