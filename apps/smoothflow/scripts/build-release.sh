#!/usr/bin/env bash
# Release build of SmoothFlow.app → signed (+ notarized) DMG.
#
#   SIGN_IDENTITY="Developer ID Application: Smoo LLC (DTX9733844)" \
#   NOTARY_KEY=~/.appstoreconnect/private_keys/AuthKey_XXX.p8 NOTARY_KEY_ID=… NOTARY_ISSUER=… \
#   apps/smoothflow/scripts/build-release.sh
#
# No SIGN_IDENTITY → ad-hoc signed, not notarized (fine for local testing; TCC
# prompts work because TCC keys on the bundle, not the identity).
# SMOOTH_DAEMON_BIN (default ~/.cargo/bin/smooth-daemon) and SMOOTH_TH_BIN
# (default ~/.cargo/bin/th) are bundled into Contents/MacOS when present; the
# daemon is what enables LaunchAgent mode, `th` is what the daemon's tools shell
# out to (Contents/MacOS is put on the child's PATH by DaemonManager).
#
# Output: $OUT_DIR/SmoothFlow-<version>-arm64.dmg — versioned because Sparkle's
# generate_appcast (smoothflow-publish.yml) keys items on the archive name and
# the CDN caches versioned artifacts as immutable.
#
# ponytail: plain `xcodebuild build` + manual codesign instead of
# archive/exportArchive — the export path needs an ExportOptions.plist and a
# provisioning setup that adds nothing over codesign for a Developer ID app.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$(cd ../.. && pwd)"

SIGN_IDENTITY="${SIGN_IDENTITY:-}"
DAEMON_BIN="${SMOOTH_DAEMON_BIN:-$HOME/.cargo/bin/smooth-daemon}"
TH_BIN="${SMOOTH_TH_BIN:-$HOME/.cargo/bin/th}"
OUT="${OUT_DIR:-dist}"
APP="build/DerivedData/Build/Products/Release/SmoothFlow.app"
VERSION="$(plutil -extract CFBundleShortVersionString raw Info.plist)"
[[ "$(plutil -extract CFBundleVersion raw Info.plist)" == "$VERSION" ]] || { echo "error: CFBundleVersion != CFBundleShortVersionString — use scripts/bump-version.sh" >&2; exit 1; }

rm -rf "$OUT" "$APP"
mkdir -p "$OUT"
# GhosttyKit first: xcodegen resolves the xcframework reference at generate time
# and a project generated against a missing Vendor/ fails the build even after
# the framework is fetched ("There is no XCFramework found").
bash scripts/ensure-ghosttykit.sh
xcodegen generate
xcodebuild -project SmoothFlow.xcodeproj -scheme SmoothFlow -configuration Release -derivedDataPath build/DerivedData \
    CODE_SIGN_IDENTITY=- ENABLE_DEBUG_DYLIB=NO build | grep -E "error:|warning: .*Swift|BUILD" || true
[[ -d "$APP" ]] || { echo "error: $APP not built" >&2; exit 1; }

for bin in "$DAEMON_BIN" "$TH_BIN"; do
    if [[ -x "$bin" ]]; then
        cp "$bin" "$APP/Contents/MacOS/$(basename "$bin")"
        echo "==> bundled $bin ($("$bin" --version 2>/dev/null | head -1))"
    else
        echo "==> no $(basename "$bin") at $bin — app will use ~/.cargo/bin or PATH at runtime"
    fi
done

ID="${SIGN_IDENTITY:--}"
TS="--timestamp"; [[ -n "$SIGN_IDENTITY" ]] || TS="--timestamp=none"
sign() { codesign --force --sign "$ID" --options runtime $TS "$@"; }
# Nested code first, then the bundle (never --deep with per-target entitlements).
for bin in smooth-daemon th; do
    [[ -f "$APP/Contents/MacOS/$bin" ]] && sign "$APP/Contents/MacOS/$bin"
done
# Sparkle.framework: Xcode ad-hoc signed it on embed; re-sign inside-out with the
# real identity (XPC services → Autoupdate → Updater.app → framework), per
# sparkle-project.org/documentation/sandboxing (the non-sandboxed subset).
SPARKLE="$APP/Contents/Frameworks/Sparkle.framework"
if [[ -d "$SPARKLE" ]]; then
    for xpc in "$SPARKLE"/Versions/B/XPCServices/*.xpc; do [[ -e "$xpc" ]] && sign --preserve-metadata=entitlements "$xpc"; done
    sign "$SPARKLE/Versions/B/Autoupdate"
    sign "$SPARKLE/Versions/B/Updater.app"
    sign "$SPARKLE"
else
    echo "error: Sparkle.framework not embedded" >&2; exit 1
fi
sign --entitlements entitlements.plist "$APP"
codesign --verify --strict --deep "$APP"
codesign -d --entitlements - "$APP" 2>/dev/null | grep -q personal-information.calendars || { echo "error: entitlements missing from signed app" >&2; exit 1; }
plutil -p "$APP/Contents/Info.plist" | grep -q NSCalendarsFullAccessUsageDescription || { echo "error: usage strings missing from Info.plist" >&2; exit 1; }
plutil -p "$APP/Contents/Info.plist" | grep -q SUPublicEDKey || { echo "error: Sparkle keys missing from Info.plist" >&2; exit 1; }
[[ -f "$APP/Contents/Resources/SmoothFlow.icns" ]] || { echo "error: SmoothFlow.icns missing from Resources" >&2; exit 1; }

DMG="$OUT/SmoothFlow-$VERSION-arm64.dmg"
hdiutil create -volname SmoothFlow -srcfolder "$APP" -ov -format UDZO "$DMG" >/dev/null
if [[ -n "$SIGN_IDENTITY" ]]; then
    codesign --sign "$ID" $TS "$DMG"
    bash "$REPO/scripts/macos/notarize-and-staple.sh" "$DMG"
    spctl -a -vvv -t install "$APP" 2>&1 | tail -2 || true
fi
echo "==> $DMG"
