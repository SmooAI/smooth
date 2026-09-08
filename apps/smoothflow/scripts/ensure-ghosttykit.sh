#!/usr/bin/env bash
# Fetch the pinned prebuilt GhosttyKit.xcframework into Vendor/ (gitignored).
# Adapted from cmux's scripts/ensure-ghosttykit.sh (MIT) — download-only: we do
# not build ghostty from source, and we refuse anything whose sha256 is not the
# one pinned in ghosttykit.lock. Idempotent; a matching stamp file short-circuits.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(dirname "$HERE")"
LOCK="$HERE/ghosttykit.lock"
DEST="$APP_DIR/Vendor/GhosttyKit.xcframework"
STAMP="$DEST/.ghostty_sha"

# shellcheck disable=SC1090
source <(grep -E '^(ghostty_sha|tar_sha256|flavor)=' "$LOCK")

if [[ -f "$STAMP" && "$(cat "$STAMP")" == "$ghostty_sha" ]]; then
  echo "==> GhosttyKit.xcframework already at $ghostty_sha"
  exit 0
fi

URL="https://github.com/manaflow-ai/ghostty/releases/download/xcframework-${ghostty_sha}-${flavor}/GhosttyKit.xcframework.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
echo "==> Fetching GhosttyKit.xcframework for ${ghostty_sha:0:12}..."
curl -fSL --retry 3 --retry-delay 2 -o "$TMP/gk.tar.gz" "$URL"
actual="$(shasum -a 256 "$TMP/gk.tar.gz" | awk '{print $1}')"
if [[ "$actual" != "$tar_sha256" ]]; then
  echo "error: GhosttyKit checksum mismatch (expected $tar_sha256, got $actual)" >&2
  exit 1
fi
tar --no-same-owner -xzf "$TMP/gk.tar.gz" -C "$TMP"
[[ -d "$TMP/GhosttyKit.xcframework" ]] || { echo "error: archive did not contain GhosttyKit.xcframework" >&2; exit 1; }
# Xcode 26 can fail to resolve symbols from the universal static archive until
# the ranlib index is refreshed after extraction (cmux hit this too).
"$(xcrun --find ranlib)" "$TMP/GhosttyKit.xcframework/macos-arm64_x86_64/"*.a
mkdir -p "$APP_DIR/Vendor"
rm -rf "$DEST"
mv "$TMP/GhosttyKit.xcframework" "$DEST"
echo "$ghostty_sha" > "$STAMP"
echo "==> Installed $DEST"
