#!/usr/bin/env bash
# Self-check for dev-link-th.sh (pearl th-15866f).
#
# The script rewrites a symlink on PATH, so the branch that decides *whether*
# to write is the part that must not be wrong: clobbering a real Homebrew `th`
# would be a genuinely bad day. One runnable check per branch.
#
# Usage: bash scripts/dev-link-th.test.sh

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/dev-link-th.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0
check() {
    local name="$1" expected="$2" actual="$3"
    if [ "$expected" = "$actual" ]; then
        echo "  ok   $name"
        pass=$((pass + 1))
    else
        echo "  FAIL $name"
        echo "         expected: $expected"
        echo "         actual:   $actual"
        fail=$((fail + 1))
    fi
}

mkdir -p "$TMP/cargo/bin" "$TMP/bundle" "$TMP/pathdir"
CARGO_TH="$TMP/cargo/bin/th"
printf '#!/bin/sh\necho dev\n' > "$CARGO_TH"
chmod +x "$CARGO_TH"
printf '#!/bin/sh\necho bundle\n' > "$TMP/bundle/th"
chmod +x "$TMP/bundle/th"

echo "dev-link-th:"

# 1. A symlink into the app bundle is repointed at the dev build.
ln -sfn "$TMP/bundle/th" "$TMP/pathdir/th"
bash "$SCRIPT" --cargo-bin "$CARGO_TH" --path-th "$TMP/pathdir/th" >/dev/null
check "repoints a bundle symlink" "$CARGO_TH" "$(readlink "$TMP/pathdir/th")"

# 2. Idempotent — a second run leaves it alone and stays quiet.
out="$(bash "$SCRIPT" --cargo-bin "$CARGO_TH" --path-th "$TMP/pathdir/th" 2>&1)"
check "second run is a no-op" "" "$out"
check "second run keeps the link" "$CARGO_TH" "$(readlink "$TMP/pathdir/th")"

# 3. A REAL file is never clobbered — the important one.
rm -f "$TMP/pathdir/th"
printf '#!/bin/sh\necho homebrew\n' > "$TMP/pathdir/th"
chmod +x "$TMP/pathdir/th"
bash "$SCRIPT" --cargo-bin "$CARGO_TH" --path-th "$TMP/pathdir/th" 2>/dev/null
check "leaves a regular file intact" "homebrew" "$("$TMP/pathdir/th")"
check "regular file is still not a symlink" "no" "$([ -L "$TMP/pathdir/th" ] && echo yes || echo no)"

# 4. Opt-out is honored.
ln -sfn "$TMP/bundle/th" "$TMP/pathdir/th"
SMOOTH_NO_DEV_LINK=1 bash "$SCRIPT" --cargo-bin "$CARGO_TH" --path-th "$TMP/pathdir/th" >/dev/null
check "SMOOTH_NO_DEV_LINK=1 skips" "$TMP/bundle/th" "$(readlink "$TMP/pathdir/th")"

# 5. No dev build yet → do nothing at all.
ln -sfn "$TMP/bundle/th" "$TMP/pathdir/th"
bash "$SCRIPT" --cargo-bin "$TMP/cargo/bin/nonexistent" --path-th "$TMP/pathdir/th" >/dev/null
check "no cargo binary → no change" "$TMP/bundle/th" "$(readlink "$TMP/pathdir/th")"

echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
