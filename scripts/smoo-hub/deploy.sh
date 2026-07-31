#!/bin/bash
# Build, code-sign, and deploy Big Smooth to smoo-hub.
# (pearls th-56ee9f, th-f4baa5)
#
# Run this on the BUILD machine (your laptop), NOT on smoo-hub — it builds the
# release binaries locally, packages the daemon as a STABLY-signed
# `Big Smooth.app` bundle (+ signs `th`), ships them over SSH, installs the app
# to ~/Applications, and restarts the launchd agent on the hub.
#
# The bundle (vs a bare binary) is what lets macOS show native "Big Smooth wants
# to access…" TCC prompts — see scripts/macos/make-app-bundle.sh + Info.plist.
#
# Why the signing matters (learned the hard way, 2026-07-30):
#   * Ad-hoc signed binaries (Rust's default on Apple Silicon) have a
#     cdhash-based designated requirement that changes every build. That means:
#       - macOS kills a freshly-copied binary with OS_REASON_CODESIGNING,
#       - launchd's recorded LWCR (launch constraint) rejects the new cdhash,
#       - and any Full Disk Access grant is bound to the old cdhash, so it dies.
#   * Signing with a stable team cert + a FIXED --identifier makes the DR
#     team-based and constant across rebuilds:
#       identifier "ai.smoo.smooth-daemon" and anchor apple generic and
#       certificate leaf[subject.CN] = "<SIGN_IDENTITY>"
#     TCC keys the FDA grant to that DR, so the grant survives every redeploy,
#     and the codesigning/LWCR churn goes away.
#
# One-time host setup (can't be scripted — needs a human at the hub's console):
#   Grant Full Disk Access to ~/smooth-daemon and ~/.cargo/bin/th
#   (run `th doctor --fix-fda` on the hub). Needed because the workspace lives
#   on an external volume (/Volumes/…), which macOS TCC-gates. Persists after
#   this script, thanks to the stable signature.
#
# One-time build-machine setup: the first sign pops a keychain prompt to use
# the private key — click "Always Allow" once so future signs are headless.
#
# Usage:
#   scripts/smoo-hub/deploy.sh [host]        # default host: smoo-hub
#   SIGN_IDENTITY="Developer ID Application: Smoo LLC (DTX9733844)" \
#     scripts/smoo-hub/deploy.sh             # upgrade cert later (re-grant FDA once)
#   scripts/smoo-hub/deploy.sh --dry-run     # print the plan, build/sign nothing

set -euo pipefail

HOST="smoo-hub"
DRY_RUN=0
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        -*) echo "unknown flag: $arg" >&2; exit 2 ;;
        *) HOST="$arg" ;;
    esac
done

# Stable signing identity. Apple Distribution works (off-label but a stable,
# team-based DR); Developer ID Application is the textbook cert — override via
# the SIGN_IDENTITY env var. The FIXED identifiers below are load-bearing: never
# change them, or the DR changes and the FDA grant is lost.
SIGN_IDENTITY="${SIGN_IDENTITY:-Apple Distribution: Smoo LLC (DTX9733844)}"
DAEMON_ID="ai.smoo.smooth-daemon"
TH_ID="ai.smoo.th"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

LABEL="com.smooai.smooth-daemon"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"

say() { printf '\n\033[1;36m==> %s\033[0m\n' "$1"; }

say "Target host: ${HOST}   Signing identity: ${SIGN_IDENTITY}   Version: ${VERSION:-?}"

if [ "$DRY_RUN" -eq 1 ]; then
    echo "  would: cargo build --release -p smooai-smooth-cli -p smooai-smooth-daemon"
    echo "  would: assemble + sign 'Big Smooth.app' (${DAEMON_ID}) via scripts/macos/make-app-bundle.sh"
    echo "  would: codesign th (${TH_ID})"
    echo "  would: ship the .app to ${HOST}:~/Applications + th, install plist, bootout/bootstrap, health-check"
    exit 0
fi

# Fail early if the signing identity isn't in the keychain.
if ! security find-identity -v -p codesigning | grep -qF "$SIGN_IDENTITY"; then
    echo "error: signing identity not found in keychain: ${SIGN_IDENTITY}" >&2
    echo "       list identities with: security find-identity -v -p codesigning" >&2
    exit 1
fi

say "Building release binaries"
# AWS_LC_SYS_NO_ASM=1 dodges the aws-lc-sys iOS-asm build break on this toolchain.
( cd "$REPO_ROOT" && AWS_LC_SYS_NO_ASM=1 cargo build --release -p smooai-smooth-cli -p smooai-smooth-daemon )

DAEMON_BIN="$REPO_ROOT/target/release/smooth-daemon"
TH_BIN="$REPO_ROOT/target/release/th"
for b in "$DAEMON_BIN" "$TH_BIN"; do
    [ -x "$b" ] || { echo "error: build did not produce $b" >&2; exit 1; }
done

say "Assembling + signing 'Big Smooth.app'"
# The daemon ships as a signed .app bundle (not a bare binary) so its Info.plist
# usage strings unlock native TCC prompts (removable-volume/FDA, Calendar, …).
# First run prompts for keychain access — click "Always Allow".
APP="$(SIGN_IDENTITY="$SIGN_IDENTITY" "$REPO_ROOT/scripts/macos/make-app-bundle.sh" "$DAEMON_BIN" "$STAGE" "$VERSION")"
echo "  bundle DR: $(codesign -d -r- "$APP" 2>&1 | grep -i designated)"
# th stays a plain CLI (invoked from the shell; its FDA is secondary).
cp "$TH_BIN" "$STAGE/th"
codesign --force --sign "$SIGN_IDENTITY" --identifier "$TH_ID" "$STAGE/th"
codesign --verify --strict "$STAGE/th"

say "Shipping to ${HOST}"
# Tar the bundle (preserves the .app tree over one SSH stream); relative remote
# paths resolve against the remote home dir.
( cd "$STAGE" && tar czf - "Big Smooth.app" ) | ssh "$HOST" 'cat > /tmp/big-smooth-app.tgz'
scp -q "$STAGE/th" "${HOST}:.cargo/bin/th.new"
# Ship the launchd plist too — its Program path now points at the bundle exe.
scp -q "$REPO_ROOT/scripts/smoo-hub/com.smooai.smooth-daemon.plist" "${HOST}:Library/LaunchAgents/${LABEL}.plist"

say "Swapping + restarting the daemon on ${HOST}"
# SC2087: heredoc body intentionally runs remotely (quoted 'REMOTE').
# SC2029: $LABEL is meant to expand client-side into the remote command line.
# shellcheck disable=SC2087,SC2029
ssh "$HOST" "LABEL='$LABEL' bash -s" <<'REMOTE'
set -euo pipefail
UID_NUM=$(id -u)
PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"
APPDIR="$HOME/Applications"; mkdir -p "$APPDIR"
# Timestamped backups so a bad deploy is one mv away from rollback.
ts=$(date +%Y%m%d-%H%M%S)
[ -d "$APPDIR/Big Smooth.app" ] && mv "$APPDIR/Big Smooth.app" "$APPDIR/Big Smooth.app.bak-$ts"
tar xzf /tmp/big-smooth-app.tgz -C "$APPDIR" && rm -f /tmp/big-smooth-app.tgz
[ -f "$HOME/.cargo/bin/th" ] && mv "$HOME/.cargo/bin/th" "$HOME/.cargo/bin/th.bak-$ts"
mv "$HOME/.cargo/bin/th.new" "$HOME/.cargo/bin/th" && chmod +x "$HOME/.cargo/bin/th"
# The daemon shells out to `th`, but its launchd PATH (/opt/homebrew/bin:
# /usr/local/bin:…) doesn't include ~/.cargo/bin. Symlink the deployed th onto
# that PATH so the daemon (and the login shell) resolve THIS build, not a stale
# brew/hand-installed copy. Symlink → target, so future deploys stay current.
for d in /opt/homebrew/bin /usr/local/bin; do
    if [ -d "$d" ]; then ln -sf "$HOME/.cargo/bin/th" "$d/th"; echo "  linked $d/th -> ~/.cargo/bin/th"; break; fi
done
echo "  installed: $(codesign -dv "$APPDIR/Big Smooth.app" 2>&1 | grep -i TeamIdentifier)"
# Full bootout/bootstrap re-derives the LWCR from the (now stable) identity.
launchctl bootout "gui/${UID_NUM}/${LABEL}" 2>/dev/null || true
sleep 1; pkill -f "smooth-daemon run" 2>/dev/null || true; sleep 1
launchctl bootstrap "gui/${UID_NUM}" "$PLIST"
launchctl enable "gui/${UID_NUM}/${LABEL}"
launchctl kickstart "gui/${UID_NUM}/${LABEL}"
sleep 5
if curl -fsS http://127.0.0.1:8788/health >/dev/null 2>&1; then
    echo "  health: ok"
else
    echo "  health: FAILED — check ~/.smooth/daemon.err" >&2
    tail -3 "$HOME/.smooth/daemon.err" >&2 || true
    exit 1
fi
REMOTE

say "Deployed 'Big Smooth.app'. On first workspace/Calendar access it now shows a native 'Big Smooth wants to access…' prompt at ${HOST}'s console — click Allow (one time; persists via the stable signature). \`th doctor --fix-fda\` still works as a manual fallback."
