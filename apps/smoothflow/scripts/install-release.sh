#!/usr/bin/env bash
# Install the OFFICIAL published SmoothFlow release into /Applications, and
# leave exactly one registered copy on the machine (pearl th-9c3f4e).
#
#   apps/smoothflow/scripts/install-release.sh            # whatever the appcast says is current
#   apps/smoothflow/scripts/install-release.sh 0.2.3      # a specific version
#   apps/smoothflow/scripts/install-release.sh --dry-run  # report, change nothing
#
# Why this is a release step and not a thing someone does by hand:
#
#   * Every debug build a lane produces registers ANOTHER SmoothFlow bundle with
#     LaunchServices. Those copies are ad-hoc signed, they are not notarized,
#     and `open -a SmoothFlow` can pick one of them instead of the real app —
#     so a release "verified" by launching the wrong bundle proves nothing.
#     47 stale registrations had accumulated from lane worktrees before this
#     script existed.
#   * Hand-installing skips verification. A build that is signed but NOT
#     stapled still passes Gatekeeper, by asking Apple over the network at
#     first launch — so it looks fine on a fast connection at a desk and
#     stalls or fails offline, behind a captive portal, or when Apple's notary
#     service is slow. The only way to catch that is to check every time.
#
# This script refuses rather than installing anything that fails its checks.
set -euo pipefail

FEED="https://downloads.smoo.ai/smoothflow/appcast.xml"
BASE="https://downloads.smoo.ai/smoothflow"
DEST="/Applications/SmoothFlow.app"
TEAM_ID="DTX9733844"
AUTHORITY="Developer ID Application: Smoo LLC (${TEAM_ID})"

DRY_RUN=0
WANT=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        -h | --help)
            sed -n '2,12p' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *) WANT="$arg" ;;
    esac
done

say() { printf '\n\033[1;36m==> %s\033[0m\n' "$*" >&2; }
ok() { printf '\033[32m  ✓\033[0m %s\n' "$*" >&2; }
die() {
    printf '\n\033[1;31mREFUSING: %s\033[0m\n' "$*" >&2
    exit 1
}
run() { if [[ $DRY_RUN == 1 ]]; then printf '  would run: %s\n' "$*" >&2; else "$@"; fi; }

LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
[[ -x "$LSREGISTER" ]] || die "lsregister not found at $LSREGISTER"

# Every registered bundle whose path involves SmoothFlow but is NOT inside the
# official install. Matching only `*/SmoothFlow.app` is not enough: a debug
# build also registers the bundles NESTED inside it — Sparkle's
# `…/Sparkle.framework/Versions/B/Updater.app` and
# `SmoothFlowUITests-Runner.app` — and a single `lsregister -u` on the outer
# .app does not take those with it. The rows outlive the files, so deleting the
# build directory leaves them behind pointing at nothing.
# The exclusion is a PREFIX, not an exact match, so the copies nested inside
# /Applications/SmoothFlow.app (which has its own Updater.app) are kept.
stale_bundles() {
    "$LSREGISTER" -dump 2>/dev/null |
        grep -oE '/[^[:space:]"]*SmoothFlow[^[:space:]"]*\.app' |
        sort -u | grep -v "^${DEST}\(/\|$\)" || true
}

W="$(mktemp -d)"
MOUNTED=""
cleanup() {
    [[ -n "$MOUNTED" ]] && hdiutil detach "$MOUNTED" >/dev/null 2>&1 || true
    rm -rf "$W"
}
trap cleanup EXIT

# ---------------------------------------------------------------- 1. stale copies
# Every registered bundle outside /Applications is a debug build. Unregister it
# so `open -a SmoothFlow` can only ever resolve to the official install.
say "Unregistering SmoothFlow bundles outside /Applications"
STALE="$(stale_bundles)"
if [[ -z "$STALE" ]]; then
    ok "none registered outside /Applications"
else
    while IFS= read -r p; do
        [[ -n "$p" ]] || continue
        printf '  stale: %s%s\n' "$p" "$([[ -e "$p" ]] || echo '  (already deleted from disk)')" >&2
        run "$LSREGISTER" -u "$p" || true
    done <<< "$STALE"
    ok "$(wc -l <<< "$STALE" | tr -d ' ') stale registration(s) cleared"
fi

# ---------------------------------------------------------------- 2. build products
say "Deleting stale SmoothFlow build products"
FOUND=0
for d in "$HOME"/dev/smooai/smooth*/apps/smoothflow/build; do
    [[ -d "$d" ]] || continue
    FOUND=1
    printf '  %s (%s)\n' "$d" "$(du -sh "$d" 2>/dev/null | cut -f1)" >&2
    run rm -rf "$d"
done
for d in "$HOME"/Library/Developer/Xcode/DerivedData/SmoothFlow-*; do
    [[ -d "$d" ]] || continue
    FOUND=1
    printf '  %s (%s)\n' "$d" "$(du -sh "$d" 2>/dev/null | cut -f1)" >&2
    run rm -rf "$d"
done
[[ $FOUND == 1 ]] && ok "build products cleared" || ok "none present"

# ---------------------------------------------------------------- 3. fetch
say "Resolving the published release"
curl -fsS -o "$W/appcast.xml" "$FEED" || die "could not fetch $FEED"
LATEST="$(grep -oE '<sparkle:shortVersionString>[^<]+</sparkle:shortVersionString>' "$W/appcast.xml" |
    sed 's/.*>\(.*\)<.*/\1/' | tail -1)"
[[ -n "$LATEST" ]] || die "no version in the appcast"
VERSION="${WANT:-$LATEST}"
printf '  appcast current: %s\n  installing:      %s\n' "$LATEST" "$VERSION" >&2
grep -q "<sparkle:shortVersionString>${VERSION}</sparkle:shortVersionString>" "$W/appcast.xml" ||
    die "the appcast does not list $VERSION — it is not a published release"
grep -q 'sparkle:edSignature="[^"]\+"' "$W/appcast.xml" || die "the appcast entry has no edSignature"
ok "listed in the signed appcast"

DMG_URL="$BASE/SmoothFlow-$VERSION-arm64.dmg"
say "Downloading $DMG_URL"
curl -fsSL -o "$W/sf.dmg" "$DMG_URL" || die "could not download $DMG_URL"
ok "$(du -h "$W/sf.dmg" | cut -f1)"

# ---------------------------------------------------------------- 4. verify BEFORE installing
say "Verifying the downloaded artifact"
xcrun stapler validate "$W/sf.dmg" >/dev/null 2>&1 || die "the DMG has no stapled ticket"
ok "DMG stapled"

MNT="$W/mnt"
mkdir -p "$MNT"
hdiutil attach "$W/sf.dmg" -nobrowse -readonly -mountpoint "$MNT" >/dev/null || die "could not mount the DMG"
MOUNTED="$MNT"
SRC="$MNT/SmoothFlow.app"
[[ -d "$SRC" ]] || die "no SmoothFlow.app inside the DMG"

GOT="$(defaults read "$SRC/Contents/Info" CFBundleShortVersionString 2>/dev/null || true)"
[[ "$GOT" == "$VERSION" ]] || die "the DMG contains $GOT, not $VERSION"
ok "app is $GOT"

# The app's own ticket — separate from the DMG's, and the one that matters,
# because this bundle is what ends up running.
xcrun stapler validate "$SRC" >/dev/null 2>&1 || die "the .app inside the DMG has no stapled ticket"
ok "app stapled"

codesign --verify --strict "$SRC" 2>/dev/null || die "the app's signature does not verify"
codesign -dv "$SRC" 2>&1 | grep -q "Authority=$AUTHORITY" ||
    die "not signed by '$AUTHORITY' — refusing to install an unofficial build"
ok "signed by $AUTHORITY"

spctl -a -vv -t exec "$SRC" 2>&1 | grep -q "Notarized Developer ID" ||
    die "Gatekeeper does not report a Notarized Developer ID"
ok "Gatekeeper: Notarized Developer ID"

for f in "$SRC/Contents/MacOS"/*; do
    [[ -f "$f" ]] || continue
    file "$f" | grep -q Mach-O || continue
    if otool -L "$f" | tail -n +2 | grep -qE '/opt/homebrew|/usr/local'; then
        otool -L "$f" | tail -n +2 | grep -E '/opt/homebrew|/usr/local' >&2
        die "$(basename "$f") links non-system dylibs (above) — it will not run on a clean Mac"
    fi
done
ok "bundled binaries link system dylibs only"

if [[ $DRY_RUN == 1 ]]; then
    say "--dry-run: verified $VERSION, installed nothing"
    exit 0
fi

# ---------------------------------------------------------------- 5. install
if pgrep -f "$DEST/Contents/MacOS/SmoothFlow" >/dev/null 2>&1; then
    say "Quitting the running SmoothFlow"
    osascript -e 'tell application "SmoothFlow" to quit' >/dev/null 2>&1 || true
    for _ in $(seq 1 30); do
        pgrep -f "$DEST/Contents/MacOS/SmoothFlow" >/dev/null 2>&1 || break
        sleep 1
    done
    pgrep -f "$DEST/Contents/MacOS/SmoothFlow" >/dev/null 2>&1 &&
        die "SmoothFlow is still running; quit it and re-run (refusing to replace a live bundle)"
    ok "quit"
fi

say "Installing to $DEST"
[[ -e "$DEST" ]] && rm -rf "$DEST"
ditto "$SRC" "$DEST" || die "copy to $DEST failed"
hdiutil detach "$MOUNTED" >/dev/null 2>&1 && MOUNTED=""
ok "copied"

"$LSREGISTER" -f "$DEST" || true

# ---------------------------------------------------------------- 6. verify the INSTALLED copy
say "Verifying the installed copy"
INSTALLED="$(defaults read "$DEST/Contents/Info" CFBundleShortVersionString)"
[[ "$INSTALLED" == "$VERSION" ]] || die "installed copy reports $INSTALLED"
ok "version $INSTALLED"
codesign --verify --strict "$DEST" 2>/dev/null || die "installed copy: signature does not verify"
codesign -dv "$DEST" 2>&1 | grep -q "Authority=$AUTHORITY" || die "installed copy: wrong signing authority"
ok "signed by $AUTHORITY"
spctl -a -vv -t exec "$DEST" 2>&1 | grep -q "Notarized Developer ID" ||
    die "installed copy: Gatekeeper does not report a Notarized Developer ID"
ok "Gatekeeper: Notarized Developer ID"
xcrun stapler validate "$DEST" >/dev/null 2>&1 || die "installed copy has no stapled ticket"
ok "stapled"

REMAINING="$(stale_bundles)"
if [[ -n "$REMAINING" ]]; then
    printf '\n\033[33mwarning: still-registered copies outside /Applications:\033[0m\n%s\n' "$REMAINING" >&2
else
    ok "the only registered SmoothFlow on this machine is $DEST"
fi

say "SmoothFlow $VERSION installed and verified"
