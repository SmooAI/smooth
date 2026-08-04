#!/usr/bin/env bash
# Build `smooth-daemon` (with the current web SPA embedded via rust-embed) from
# LOCAL source and hot-swap it into the installed Big Smooth.app, re-sign, and
# relaunch — the desktop-app equivalent of `pnpm install:th`.
#
# The Electron shell bundles the daemon at Contents/Resources/smooth-daemon and
# spawns it on launch, so swapping that binary + restarting is all it takes to
# run new daemon + new UI. We re-sign afterwards (Developer ID if it's in the
# keychain, else ad-hoc) so Gatekeeper still accepts the bundle.
#
# Env:
#   BIG_SMOOTH_APP   override the app path (default /Applications/Big Smooth.app)
#   SIGN_IDENTITY    signing identity (default the Smoo Developer ID)
#   CARGO_TARGET_DIR respected via `cargo metadata` (works with a global target-dir)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APP="${BIG_SMOOTH_APP:-/Applications/Big Smooth.app}"
cd "$REPO_ROOT"

[ -d "$APP" ] || {
    echo "error: Big Smooth not installed at '$APP'. Install the DMG first (or set BIG_SMOOTH_APP)." >&2
    exit 1
}
DEST="$APP/Contents/Resources/smooth-daemon"
[ -e "$DEST" ] || {
    echo "error: no bundled daemon at '$DEST' — is this the Electron Big Smooth?" >&2
    exit 1
}

echo "==> building web SPA (embedded into the daemon)"
pnpm build:web

echo "==> building smooth-daemon (release)"
cargo build --release -p smooai-smooth-daemon
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
BIN="$TARGET_DIR/release/smooth-daemon"
[ -x "$BIN" ] || {
    echo "error: build did not produce '$BIN'" >&2
    exit 1
}

echo "==> stopping the running app + daemon"
osascript -e 'quit app "Big Smooth"' 2>/dev/null || true
pkill -f "$APP/Contents/MacOS/Big Smooth" 2>/dev/null || true
pkill -f "$DEST" 2>/dev/null || true
sleep 1

echo "==> swapping in the fresh daemon"
cp "$BIN" "$DEST"

IDENTITY="${SIGN_IDENTITY:-Developer ID Application: Smoo LLC (DTX9733844)}"
ENT="$REPO_ROOT/desktop/build/entitlements.mac.plist"
if security find-identity -v -p codesigning 2>/dev/null | grep -q "$IDENTITY"; then
    echo "==> re-signing with: $IDENTITY"
    codesign --force --options runtime --entitlements "$ENT" -s "$IDENTITY" "$DEST"
    codesign --force --options runtime --entitlements "$ENT" -s "$IDENTITY" "$APP"
else
    echo "==> '$IDENTITY' not in keychain — ad-hoc signing (fine for local dev)"
    codesign --force -s - "$DEST"
    codesign --force -s - "$APP"
fi

echo "==> relaunching"
open "$APP"
echo "✅ Big Smooth reinstalled from local source."
echo "   daemon: $BIN"
