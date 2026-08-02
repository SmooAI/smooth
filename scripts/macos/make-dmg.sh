#!/bin/bash
# Package `Big Smooth.app` into a drag-to-Applications .dmg (pearl th-a647da).
#
# The distributable counterpart to scripts/macos/make-app-bundle.sh: that one
# produces the signed .app, this one wraps it in the disk image users actually
# download. Nothing here builds or signs — sign the app FIRST (real Developer ID
# + hardened runtime), then dmg, then notarize the dmg
# (scripts/macos/notarize-and-staple.sh).
#
# Usage:
#   scripts/macos/make-dmg.sh <app-path> <output-dmg> [volume-name]
#
# Prints the .dmg path on stdout. macOS only (hdiutil).
#
# ponytail: plain hdiutil, no create-dmg dependency and no custom background /
# icon layout. A window with the app + an /Applications symlink is the whole
# convention. Add a .DS_Store layout when the plain one measurably confuses
# someone.

set -euo pipefail

APP="${1:?usage: make-dmg.sh <app-path> <output-dmg> [volume-name]}"
DMG="${2:?usage: make-dmg.sh <app-path> <output-dmg> [volume-name]}"
VOLNAME="${3:-Big Smooth}"

[ -d "$APP" ] || { echo "error: app bundle not found: $APP" >&2; exit 1; }

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# `ditto` copies the bundle faithfully — including the code signature, which
# `cp -r` can mangle on extended attributes.
ditto "$APP" "$STAGE/$(basename "$APP")"
ln -s /Applications "$STAGE/Applications"

mkdir -p "$(dirname "$DMG")"
rm -f "$DMG"

# UDZO = compressed read-only, the standard shipping format. `-quiet` keeps the
# stdout contract (the dmg path, and nothing else).
hdiutil create -quiet -srcfolder "$STAGE" -volname "$VOLNAME" -fs HFS+ -format UDZO -ov "$DMG" >&2

# Sign the dmg too when a real identity is configured — notarization staples to
# the dmg, and a signed dmg is what stops "damaged and can't be opened".
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
if [ "$SIGN_IDENTITY" != "-" ]; then
    codesign --force --sign "$SIGN_IDENTITY" "$DMG" >&2
    codesign --verify --strict "$DMG" >&2
fi

echo "$DMG"
