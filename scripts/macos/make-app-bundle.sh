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
# The bundle identifier is FIXED (ai.smoo.smooth-daemon) — it's the stable TCC
# key; changing it resets every granted permission.
#
# macOS only (app bundles + codesign are macOS concepts).

set -euo pipefail

BIN="${1:?usage: make-app-bundle.sh <smooth-daemon-binary> <output-dir> [version]}"
OUT="${2:?usage: make-app-bundle.sh <smooth-daemon-binary> <output-dir> [version]}"
VERSION="${3:-0.0.0}"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"   # "-" = ad-hoc

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

# Sign the whole BUNDLE (seals Info.plist into the signature). The fixed
# --identifier keeps the designated requirement stable across rebuilds so TCC
# grants persist. (Hardened runtime / notarization is a follow-up, only needed
# for distribution off this machine.)
codesign --force --sign "$SIGN_IDENTITY" --identifier ai.smoo.smooth-daemon "$APP"
codesign --verify --strict "$APP"

echo "$APP"
