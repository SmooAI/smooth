#!/usr/bin/env bash
# Release build of SmoothFlow.app → signed (+ notarized) DMG.
#
#   SIGN_IDENTITY="Developer ID Application: Smoo LLC (DTX9733844)" \
#   NOTARY_KEY=~/.appstoreconnect/private_keys/AuthKey_XXX.p8 NOTARY_KEY_ID=… NOTARY_ISSUER=… \
#   apps/smoothflow/scripts/build-release.sh
#
# No SIGN_IDENTITY → ad-hoc signed, not notarized (fine for local testing; TCC
# prompts work because TCC keys on the bundle, not the identity).
# SMOOTH_DAEMON_BIN (default ~/.cargo/bin/smooth-daemon) is bundled into
# Contents/MacOS when present, which is what enables LaunchAgent mode.
#
# ponytail: plain `xcodebuild build` + manual codesign instead of
# archive/exportArchive — the export path needs an ExportOptions.plist and a
# provisioning setup that adds nothing over codesign for a Developer ID app.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$(cd ../.. && pwd)"

SIGN_IDENTITY="${SIGN_IDENTITY:-}"
DAEMON_BIN="${SMOOTH_DAEMON_BIN:-$HOME/.cargo/bin/smooth-daemon}"
OUT="${OUT_DIR:-dist}"
APP="build/DerivedData/Build/Products/Release/SmoothFlow.app"

rm -rf "$OUT" "$APP"
mkdir -p "$OUT"
xcodegen generate
bash scripts/ensure-ghosttykit.sh
xcodebuild -project SmoothFlow.xcodeproj -scheme SmoothFlow -configuration Release -derivedDataPath build/DerivedData \
    CODE_SIGN_IDENTITY=- ENABLE_DEBUG_DYLIB=NO build | grep -E "error:|warning: .*Swift|BUILD" || true
[[ -d "$APP" ]] || { echo "error: $APP not built" >&2; exit 1; }

if [[ -x "$DAEMON_BIN" ]]; then
    cp "$DAEMON_BIN" "$APP/Contents/MacOS/smooth-daemon"
    echo "==> bundled $DAEMON_BIN ($("$DAEMON_BIN" --version 2>/dev/null | head -1))"
else
    echo "==> no smooth-daemon at $DAEMON_BIN — app will use ~/.cargo/bin or PATH at runtime (LaunchAgent mode unavailable)"
fi

ID="${SIGN_IDENTITY:--}"
TS="--timestamp"; [[ -n "$SIGN_IDENTITY" ]] || TS="--timestamp=none"
# Nested code first, then the bundle (never --deep with per-target entitlements).
if [[ -f "$APP/Contents/MacOS/smooth-daemon" ]]; then
    codesign --force --sign "$ID" --options runtime $TS "$APP/Contents/MacOS/smooth-daemon"
fi
codesign --force --sign "$ID" --options runtime $TS --entitlements entitlements.plist "$APP"
codesign --verify --strict --deep "$APP"
codesign -d --entitlements - "$APP" 2>/dev/null | grep -q personal-information.calendars || { echo "error: entitlements missing from signed app" >&2; exit 1; }
plutil -p "$APP/Contents/Info.plist" | grep -q NSCalendarsFullAccessUsageDescription || { echo "error: usage strings missing from Info.plist" >&2; exit 1; }

DMG="$OUT/SmoothFlow.dmg"
hdiutil create -volname SmoothFlow -srcfolder "$APP" -ov -format UDZO "$DMG" >/dev/null
if [[ -n "$SIGN_IDENTITY" ]]; then
    codesign --sign "$ID" $TS "$DMG"
    bash "$REPO/scripts/macos/notarize-and-staple.sh" "$DMG"
    spctl -a -vvv -t install "$APP" 2>&1 | tail -2 || true
fi
echo "==> $DMG"
