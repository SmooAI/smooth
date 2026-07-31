#!/bin/bash
# Build, code-sign, and deploy Big Smooth (smooth-daemon + th) to smoo-hub.
# (pearl th-56ee9f)
#
# Run this on the BUILD machine (your laptop), NOT on smoo-hub — it builds the
# release binaries locally, signs them with a STABLE team identity, ships them
# over SSH, and restarts the launchd agent on the hub.
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

say() { printf '\n\033[1;36m==> %s\033[0m\n' "$1"; }

say "Target host: ${HOST}   Signing identity: ${SIGN_IDENTITY}"

if [ "$DRY_RUN" -eq 1 ]; then
    echo "  would: cargo build --release -p smooai-smooth-cli -p smooai-smooth-daemon"
    echo "  would: codesign smooth-daemon (${DAEMON_ID}) + th (${TH_ID})"
    echo "  would: scp both to ${HOST}, back up + swap, bootout/bootstrap, health-check"
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

say "Signing with stable identity + fixed identifiers"
cp "$DAEMON_BIN" "$STAGE/smooth-daemon"
cp "$TH_BIN" "$STAGE/th"
# First run prompts for keychain access — click "Always Allow".
codesign --force --sign "$SIGN_IDENTITY" --identifier "$DAEMON_ID" "$STAGE/smooth-daemon"
codesign --force --sign "$SIGN_IDENTITY" --identifier "$TH_ID" "$STAGE/th"
codesign --verify --strict "$STAGE/smooth-daemon"
codesign --verify --strict "$STAGE/th"
echo "  daemon DR: $(codesign -d -r- "$STAGE/smooth-daemon" 2>&1 | grep -i designated)"

say "Shipping to ${HOST}"
# Relative remote paths are resolved against the remote home dir.
scp -q "$STAGE/smooth-daemon" "${HOST}:smooth-daemon.new"
scp -q "$STAGE/th" "${HOST}:.cargo/bin/th.new"

say "Swapping + restarting the daemon on ${HOST}"
# SC2087: heredoc body intentionally runs remotely (quoted 'REMOTE').
# SC2029: $LABEL is meant to expand client-side into the remote command line.
# shellcheck disable=SC2087,SC2029
ssh "$HOST" "LABEL='$LABEL' bash -s" <<'REMOTE'
set -euo pipefail
UID_NUM=$(id -u)
PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"
# Timestamped backups so a bad deploy is one mv away from rollback.
ts=$(date +%Y%m%d-%H%M%S)
[ -f "$HOME/smooth-daemon" ] && mv "$HOME/smooth-daemon" "$HOME/smooth-daemon.bak-$ts"
mv "$HOME/smooth-daemon.new" "$HOME/smooth-daemon" && chmod +x "$HOME/smooth-daemon"
[ -f "$HOME/.cargo/bin/th" ] && mv "$HOME/.cargo/bin/th" "$HOME/.cargo/bin/th.bak-$ts"
mv "$HOME/.cargo/bin/th.new" "$HOME/.cargo/bin/th" && chmod +x "$HOME/.cargo/bin/th"
echo "  installed: $(codesign -dv "$HOME/smooth-daemon" 2>&1 | grep -i TeamIdentifier)"
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

say "Deployed. If Full Disk Access isn't granted yet, run \`th doctor --fix-fda\` on ${HOST}'s console (one time; it persists now)."
