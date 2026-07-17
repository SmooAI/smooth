#!/bin/bash
# Install the Big Smooth daemon launchd agent on smoo-hub.
#
# Idempotent: re-run after script/plist changes to pick up the new version.
# Kills any hand-started nohup daemon (freeing the port), boots out an existing
# agent (if any), updates the plist, and bootstraps it again under the user's
# GUI session domain. Replaces the fragile nohup start with a KeepAlive agent
# that survives reboots and auto-respawns.
#
# Run this ON smoo-hub. Either:
#   ssh smoo-hub 'cd ~/dev/smooai/smooth && ./scripts/smoo-hub/install-smooth-daemon.sh'
# Or from a fresh login session interactively.
#
# Mirrors scripts/smoo-hub/install-docker-watchdog.sh in the smooai repo.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLIST_SRC="${SCRIPT_DIR}/com.smooai.smooth-daemon.plist"
PLIST_DST="${HOME}/Library/LaunchAgents/com.smooai.smooth-daemon.plist"
LABEL="com.smooai.smooth-daemon"
UID_NUM="$(id -u)"

if [ ! -f "${PLIST_SRC}" ]; then
    echo "Error: missing plist at ${PLIST_SRC}" >&2
    exit 1
fi

mkdir -p "${HOME}/Library/LaunchAgents" "${HOME}/.smooth"

# Kill any hand-started nohup daemon so the port (127.0.0.1:8788) is free
# before launchd starts its own copy. `pkill` exits non-zero when nothing
# matches — swallow that.
pkill -f 'smooth-daemon run' >/dev/null 2>&1 || true

# Bootout any existing instance so the new plist actually takes effect.
# `bootout` errors if the service is not loaded — swallow that case.
launchctl bootout "gui/${UID_NUM}/${LABEL}" >/dev/null 2>&1 || true

cp "${PLIST_SRC}" "${PLIST_DST}"
echo "Installed plist → ${PLIST_DST}"

launchctl bootstrap "gui/${UID_NUM}" "${PLIST_DST}"
launchctl enable "gui/${UID_NUM}/${LABEL}"

# Force one immediate (re)start so the install proves itself end-to-end.
launchctl kickstart -k "gui/${UID_NUM}/${LABEL}"

echo
echo "Daemon installed. Verify:"
echo "  launchctl print gui/${UID_NUM}/${LABEL} | head"
echo "  curl -fsS http://127.0.0.1:8788/health"
echo "  tail -f ~/.smooth/daemon.log"
