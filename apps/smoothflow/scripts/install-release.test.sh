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

[[ $fail == 0 ]] && echo "install-release.sh matcher: all checks passed"
exit $fail
