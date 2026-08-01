#!/bin/bash
# Install Big Smooth as an app on THIS Mac (pearl th-349caa).
#
# Builds smooth-daemon, packages it as `Big Smooth.app` (scripts/macos/
# make-app-bundle.sh), and installs it to ~/Applications so you can double-click
# it — launched as a .app, it shows the menu-bar item automatically.
#
# This is the local/laptop counterpart to scripts/smoo-hub/deploy.sh (which does
# the same over SSH to the hub). Same generic bundle builder underneath.
#
# Usage:
#   scripts/macos/install-local.sh                 # build + install to ~/Applications
#   scripts/macos/install-local.sh --login-item    # also auto-start at login
#   scripts/macos/install-local.sh --open          # launch it after installing
#   SIGN_IDENTITY="Apple Distribution: Smoo LLC (DTX9733844)" \
#     scripts/macos/install-local.sh               # stable signing (default: ad-hoc)

set -euo pipefail

LOGIN_ITEM=0
OPEN_AFTER=0
for arg in "$@"; do
    case "$arg" in
        --login-item) LOGIN_ITEM=1 ;;
        --open) OPEN_AFTER=1 ;;
        *) echo "unknown flag: $arg" >&2; exit 2 ;;
    esac
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
APPDIR="$HOME/Applications"
APP="$APPDIR/Big Smooth.app"
EXE="$APP/Contents/MacOS/smooth-daemon"
LABEL="ai.smoo.big-smooth"
PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"

say() { printf '\n\033[1;36m==> %s\033[0m\n' "$1"; }

say "Building the web SPA (embedded into the daemon via rust-embed)"
# Without this the daemon serves the build-time placeholder page. `pnpm install:th`
# does this too; a standalone cargo build does not.
( cd "$REPO_ROOT" && pnpm build:web )

say "Building smooth-daemon + th (release)"
# AWS_LC_SYS_NO_ASM=1 dodges the aws-lc-sys iOS-asm build break on this toolchain.
# `th` rides along (same target dir, so make-app-bundle.sh finds it by default)
# to be bundled at Contents/Resources/bin/th for the menu bar's "Install th CLI…".
( cd "$REPO_ROOT" && AWS_LC_SYS_NO_ASM=1 cargo build --release -p smooai-smooth-daemon -p smooai-smooth-cli )
# Resolve the actual target dir — honors a global `target-dir` in
# ~/.cargo/config.toml (or CARGO_TARGET_DIR), not just ./target.
TARGET_DIR="$(cd "$REPO_ROOT" && cargo metadata --format-version 1 --no-deps 2>/dev/null | grep -o '"target_directory":"[^"]*"' | head -1 | sed 's/.*:"//; s/"$//')"
TARGET_DIR="${TARGET_DIR:-$REPO_ROOT/target}"
DAEMON_BIN="$TARGET_DIR/release/smooth-daemon"
[ -x "$DAEMON_BIN" ] || { echo "error: build did not produce $DAEMON_BIN" >&2; exit 1; }

say "Packaging Big Smooth.app"
STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT
BUILT="$("$REPO_ROOT/scripts/macos/make-app-bundle.sh" "$DAEMON_BIN" "$STAGE" "$VERSION")"

say "Installing to ${APPDIR}"
mkdir -p "$APPDIR"
if [ -d "$APP" ]; then rm -rf "$APP"; fi
# `ditto` preserves the bundle (and the code signature) faithfully.
ditto "$BUILT" "$APP"
echo "  installed: $APP"

if [ "$LOGIN_ITEM" -eq 1 ]; then
    say "Installing login-item (auto-start at login)"
    mkdir -p "$HOME/Library/LaunchAgents"
    cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>${LABEL}</string>
    <key>ProgramArguments</key>
    <array><string>${EXE}</string><string>run</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>ThrottleInterval</key><integer>10</integer>
</dict>
</plist>
PLIST
    launchctl bootout "gui/$(id -u)/${LABEL}" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$PLIST"
    launchctl enable "gui/$(id -u)/${LABEL}"
    launchctl kickstart "gui/$(id -u)/${LABEL}"
    echo "  login-item installed (${PLIST})"
fi

if [ "$OPEN_AFTER" -eq 1 ]; then
    say "Launching Big Smooth"
    open "$APP"
fi

say "Done. Double-click 'Big Smooth' in ~/Applications (or run: open \"$APP\") — the menu-bar item appears automatically."
