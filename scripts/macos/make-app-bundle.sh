#!/bin/bash
# Assemble (and sign) Big Smooth.app around a built smooth-daemon binary.
# (pearl th-f4baa5)
#
# Generic + reusable: smoo-hub's deploy.sh calls it, and so can a future
# user-facing installer / release-artifact job. It does NOT deploy anything —
# it just produces a signed `Big Smooth.app` in the output dir and prints its
# path on stdout.
#
# Usage:
#   scripts/macos/make-app-bundle.sh <smooth-daemon-binary> <output-dir> [version]
#
# Signing identity comes from $SIGN_IDENTITY (default ad-hoc "-" for local dev).
# With a REAL identity the bundle also gets the HARDENED RUNTIME, which Apple's
# notary service requires; ad-hoc deliberately stays plain (hardened + ad-hoc
# buys nothing and only adds ways for a local run to die).
# The bundle identifier is FIXED (ai.smoo.smooth-daemon) — it's the stable TCC
# key; changing it resets every granted permission.
#
# The `th` CLI is bundled at Contents/Resources/bin/th when present, so the app
# can offer "Install th CLI…" from its menu bar (pearl th-a647da). Point $TH_BIN
# at it; the default is a `th` sitting next to the daemon binary (which is where
# `cargo build --release -p smooai-smooth-cli -p smooai-smooth-daemon` puts it).
# Missing = skipped, not an error — the daemon doesn't need it to run.
#
# macOS only (app bundles + codesign are macOS concepts).

set -euo pipefail

BIN="${1:?usage: make-app-bundle.sh <smooth-daemon-binary> <output-dir> [version]}"
OUT="${2:?usage: make-app-bundle.sh <smooth-daemon-binary> <output-dir> [version]}"
VERSION="${3:-0.0.0}"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"   # "-" = ad-hoc
TH_BIN="${TH_BIN:-$(dirname "$BIN")/th}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLIST_SRC="${SCRIPT_DIR}/Info.plist"

[ -f "$BIN" ] || { echo "error: binary not found: $BIN" >&2; exit 1; }
[ -f "$PLIST_SRC" ] || { echo "error: missing template: $PLIST_SRC" >&2; exit 1; }

APP="${OUT}/Big Smooth.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"

cp "$BIN" "$APP/Contents/MacOS/smooth-daemon"
chmod +x "$APP/Contents/MacOS/smooth-daemon"

# CFBundleExecutable must match the file name in Contents/MacOS.
sed "s/__VERSION__/${VERSION}/g" "$PLIST_SRC" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# Hardened runtime + secure timestamp: both are notarization prerequisites, and
# both are pointless (or actively annoying) without a Developer ID cert, so they
# key off the identity. Everything else — ad-hoc local dev, and smoo-hub's
# Apple Distribution deploys — signs exactly as it did before.
# (macOS ships bash 3.2, where "${arr[@]}" on an EMPTY array trips `set -u` —
# hence the ${arr[@]+…} guard at both use sites.)
SIGN_FLAGS=()
case "$SIGN_IDENTITY" in
    "Developer ID"*) SIGN_FLAGS=(--timestamp --options runtime) ;;
esac

# A nested Mach-O must carry its OWN signature — the outer bundle signature only
# seals it by hash, and notarization rejects an unsigned nested binary. Sign it
# BEFORE the bundle so the outer seal covers the final bytes.
if [ -f "$TH_BIN" ]; then
    mkdir -p "$APP/Contents/Resources/bin"
    cp "$TH_BIN" "$APP/Contents/Resources/bin/th"
    chmod +x "$APP/Contents/Resources/bin/th"
    codesign --force --sign "$SIGN_IDENTITY" --identifier ai.smoo.th ${SIGN_FLAGS[@]+"${SIGN_FLAGS[@]}"} "$APP/Contents/Resources/bin/th"
else
    echo "note: no th binary at ${TH_BIN} — 'Install th CLI…' will be unavailable in this build" >&2
fi

# Sign the whole BUNDLE (seals Info.plist into the signature). The fixed
# --identifier keeps the designated requirement stable across rebuilds so TCC
# grants persist.
codesign --force --sign "$SIGN_IDENTITY" --identifier ai.smoo.smooth-daemon ${SIGN_FLAGS[@]+"${SIGN_FLAGS[@]}"} "$APP"
codesign --verify --strict "$APP"

echo "$APP"
