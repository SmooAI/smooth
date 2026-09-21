#!/usr/bin/env bash
# Self-check for install-release.sh's stale-bundle matcher.
#   bash apps/smoothflow/scripts/install-release.test.sh
#
# The matcher decides which LaunchServices registrations get unregistered, so
# both directions are load-bearing:
#   * too narrow  → stray debug bundles stay registered, and `open -a SmoothFlow`
#                   can launch one instead of the release (a release "verified"
#                   by launching it then proves nothing);
#   * too wide    → it unregisters the official install's own nested bundles,
#                   breaking Sparkle's updater on a machine that was fine.
set -uo pipefail

DEST=/Applications/SmoothFlow.app

# Kept identical to stale_bundles() in install-release.sh. Verified below.
match() {
    grep -oE '/[^[:space:]"]*SmoothFlow[^[:space:]"]*\.app' |
        sort -u | grep -v "^${DEST}\(/\|$\)" || true
}

DUMP=$(
    cat <<'EOF'
path: /Applications/SmoothFlow.app
path: /Applications/SmoothFlow.app/Contents/Frameworks/Sparkle.framework/Versions/B/Updater.app
path: /Users/x/dev/wt/apps/smoothflow/build/DerivedData/Build/Products/Debug/SmoothFlow.app
path: /Users/x/dev/wt/apps/smoothflow/build/DerivedData/Build/Products/Debug/SmoothFlow.app/Contents/Frameworks/Sparkle.framework/Versions/B/Updater.app
path: /Users/x/dev/wt/apps/smoothflow/build/DerivedData/Build/Products/Debug/SmoothFlowUITests-Runner.app
path: /Applications/Safari.app
EOF
)
GOT=$(match <<< "$DUMP")

fail=0
want_present() { grep -qxF "$1" <<< "$GOT" || { echo "FAIL: should have matched: $1"; fail=1; }; }
want_absent() { grep -qxF "$1" <<< "$GOT" && { echo "FAIL: should NOT have matched: $1"; fail=1; }; }

# Stray debug build, and the two nested bundles a single `-u` on the outer .app
# leaves behind — the case that made a "cleaned" machine still dirty.
want_present /Users/x/dev/wt/apps/smoothflow/build/DerivedData/Build/Products/Debug/SmoothFlow.app
want_present /Users/x/dev/wt/apps/smoothflow/build/DerivedData/Build/Products/Debug/SmoothFlow.app/Contents/Frameworks/Sparkle.framework/Versions/B/Updater.app
want_present /Users/x/dev/wt/apps/smoothflow/build/DerivedData/Build/Products/Debug/SmoothFlowUITests-Runner.app

# The official install and anything inside it must survive.
want_absent /Applications/SmoothFlow.app
want_absent /Applications/SmoothFlow.app/Contents/Frameworks/Sparkle.framework/Versions/B/Updater.app
want_absent /Applications/Safari.app

[[ $(wc -l <<< "$GOT" | tr -d ' ') == 3 ]] || {
    echo "FAIL: expected exactly 3 matches, got:"
    echo "$GOT"
    fail=1
}

# The regex above is a copy; drift between it and the real script would make
# this test pass while the shipped matcher misbehaves.
SRC="$(dirname "${BASH_SOURCE[0]}")/install-release.sh"
grep -qF "grep -oE '/[^[:space:]\"]*SmoothFlow[^[:space:]\"]*\\.app'" "$SRC" || {
    echo "FAIL: install-release.sh's matcher no longer matches this test's copy"
    fail=1
}


# ---------------------------------------------------------------- signature gate
# Two independent bugs made this gate refuse every legitimate release; it was
# caught only by installing 0.2.3 by hand. Failing closed is the safe direction,
# but it made the standing release step useless, and nothing covered it.
#
#   1. `codesign -dv` alone prints NO Authority lines — they need --verbose=2.
#   2. Under `set -o pipefail`, `producer | grep -q` returns 141: grep exits on
#      the first match and SIGPIPEs the producer, so a MATCH reads as a failure.
#      This one bites every check in the script, not just codesign.
AUTHORITY="Developer ID Application: Smoo LLC (DTX9733844)"

grep -q 'codesign -dv --verbose=2 "\$SRC"' "$SRC" ||
    { echo "FAIL: downloaded-app authority check must use 'codesign -dv --verbose=2' (bare -dv prints no Authority lines)"; fail=1; }
grep -q 'codesign -dv --verbose=2 "\$DEST"' "$SRC" ||
    { echo "FAIL: installed-copy authority check must use 'codesign -dv --verbose=2'"; fail=1; }

# No verification may pipe a producer straight into `grep -q` while pipefail is
# on. Capture first, then grep from a here-string.
if grep -q '^set -[a-z]*o pipefail' "$SRC" && grep -nE '\| *grep -q' "$SRC" | grep -q .; then
    echo "FAIL: pipefail is on and a check still pipes into 'grep -q' (returns 141 on a match):"
    grep -nE '\| *grep -q' "$SRC" | sed 's/^/       /'
    fail=1
fi

# Behavioural proof of bug 2, independent of the script's text.
PIPED=0; bash -c 'set -uo pipefail; printf "Authority=x\n" | grep -q Authority=' || PIPED=$?
CAPTURED=0; bash -c 'set -uo pipefail; out="$(printf "Authority=x\n")"; grep -q Authority= <<< "$out"' || CAPTURED=$?
[[ $CAPTURED == 0 ]] || { echo "FAIL: capture-then-grep should succeed on a match (got $CAPTURED)"; fail=1; }

# Behavioural proof of the gate itself: ad-hoc signing yields no Authority line,
# so the Developer ID gate must reject such a bundle.
TMPROOT=$(mktemp -d); TMPAPP="$TMPROOT/T.app"
mkdir -p "$TMPAPP/Contents/MacOS"
printf '#!/bin/sh\n' > "$TMPAPP/Contents/MacOS/T"; chmod +x "$TMPAPP/Contents/MacOS/T"
cat > "$TMPAPP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>T</string>
<key>CFBundleIdentifier</key><string>ai.smoo.installreleasetest</string>
</dict></plist>
PLIST
if codesign -s - "$TMPAPP" >/dev/null 2>&1; then
    ADHOC="$(codesign -dv --verbose=2 "$TMPAPP" 2>&1 || true)"
    grep -q "Authority=$AUTHORITY" <<< "$ADHOC" &&
        { echo "FAIL: an ad-hoc signed bundle must not satisfy the Developer ID gate"; fail=1; }
else
    echo "skip: codesign unavailable — ad-hoc rejection not exercised"
fi
rm -rf "$TMPROOT"

# Positive case needs a real Developer ID signature, so it runs only where an
# official install exists. Skipped in CI rather than faked.
if [[ -d "$DEST" ]] && codesign --verify --strict "$DEST" >/dev/null 2>&1; then
    REAL="$(codesign -dv --verbose=2 "$DEST" 2>&1 || true)"
    grep -q "Authority=$AUTHORITY" <<< "$REAL" ||
        { echo "FAIL: the installed release does not present '$AUTHORITY' under --verbose=2"; fail=1; }
    BARE="$(codesign -dv "$DEST" 2>&1 || true)"
    grep -q "Authority=" <<< "$BARE" &&
        echo "NOTE: bare -dv prints Authority on this macOS; requiring --verbose=2 remains correct"
else
    echo "skip: no verifiable /Applications/SmoothFlow.app — positive case not exercised"
fi

[[ $fail == 0 ]] && echo "install-release.sh matcher: all checks passed"
exit $fail
