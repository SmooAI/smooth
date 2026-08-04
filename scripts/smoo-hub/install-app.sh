#!/usr/bin/env bash
# Install the Electron Big Smooth.app (which carries its own daemon) onto a
# remote Mac (smoo-hub), replacing any old native app + standalone daemon, and
# keep it alive across reboots via a LaunchAgent.
#
# The app IS the package: one bundle carries the GUI + the daemon it manages.
# On smoo-hub the daemon must dodge the SmooHub dashboard, which owns :8787
# (local) + :443 (tailscale serve) — so we pin the daemon to :8788 local +
# :8443 serve via env the LaunchAgent injects into the app process.
#
# Usage: scripts/smoo-hub/install-app.sh [host] [path/to/Big Smooth.app]
#   host defaults to smoo-hub; app defaults to the freshly built one.
set -euo pipefail

HOST="${1:-smoo-hub}"
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APP="${2:-$REPO_ROOT/desktop/release/mac-arm64/Big Smooth.app}"
[ -d "$APP" ] || { echo "error: app not found at '$APP' — build it first (pnpm dist:mac)." >&2; exit 1; }

REMOTE_HOME="/Users/brentrager"
REMOTE_APP="$REMOTE_HOME/Applications/Big Smooth.app"
AGENT_LABEL="com.smooai.bigsmooth"
AGENT_PLIST="$REMOTE_HOME/Library/LaunchAgents/$AGENT_LABEL.plist"

say() { printf '\n\033[1;36m==> %s\033[0m\n' "$1" >&2; }

say "packing the app (ditto preserves symlinks + signature)"
STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT
ditto -c -k --sequesterRsrc --keepParent "$APP" "$STAGE/BigSmooth.zip"
say "shipping to $HOST"
scp -q "$STAGE/BigSmooth.zip" "$HOST:/tmp/BigSmooth.zip"

say "installing on $HOST (stop old, swap app, keep-alive LaunchAgent)"
# Everything the remote needs is derivable from $HOME there — avoid passing paths
# with spaces ("Big Smooth.app") through ssh args, which the remote shell
# re-splits on the space.
ssh "$HOST" bash -s <<'REMOTE'
set -euo pipefail
uid="$(id -u)"
REMOTE_HOME="$HOME"
REMOTE_APP="$HOME/Applications/Big Smooth.app"
AGENT_LABEL="com.smooai.bigsmooth"
AGENT_PLIST="$HOME/Library/LaunchAgents/$AGENT_LABEL.plist"

# 1. Stop the OLD standalone-daemon LaunchAgent (it runs the native app's daemon
#    binary directly). Leave the dashboard + docker-watchdog jobs alone.
for label in com.smooai.smooth-daemon com.smooai.smooth; do
  plist="$HOME/Library/LaunchAgents/$label.plist"
  if [ -e "$plist" ]; then
    echo "  booting out $label"
    launchctl bootout "gui/$uid/$label" 2>/dev/null || true
    mv "$plist" "$plist.disabled-$(date +%s)"
  fi
done
pkill -f "Big Smooth.app/Contents/MacOS/smooth-daemon" 2>/dev/null || true
pkill -f "Big Smooth.app/Contents/MacOS/Big Smooth" 2>/dev/null || true
sleep 1

# 2. Back up + replace the app bundle.
if [ -d "$REMOTE_APP" ]; then
  echo "  backing up old app"
  rm -rf "$HOME/.big-smooth-old.bak"
  mv "$REMOTE_APP" "$HOME/.big-smooth-old.bak"
fi
mkdir -p "$HOME/Applications"
ditto -x -k /tmp/BigSmooth.zip "$HOME/Applications/"
rm -f /tmp/BigSmooth.zip
xattr -dr com.apple.quarantine "$REMOTE_APP" 2>/dev/null || true
echo "  installed: $REMOTE_APP"

# 3. LaunchAgent: launch the APP (it carries + manages the daemon), with the
#    port env that dodges the dashboard, RunAtLoad + KeepAlive (survives reboot).
mkdir -p "$HOME/Library/LaunchAgents"
cat > "$AGENT_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>$AGENT_LABEL</string>
    <key>ProgramArguments</key>
    <array>
        <string>$REMOTE_APP/Contents/MacOS/Big Smooth</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <!-- Dodge the SmooHub dashboard (owns :8787 local + :443 serve). -->
        <key>SMOOTH_ADDR</key><string>127.0.0.1:8788</string>
        <key>SMOOTH_TAILSCALE_HTTPS_PORT</key><string>8443</string>
        <!-- Keep the SAME db + workspace the old daemon used, or history moves. -->
        <key>SMOOTH_OPERATOR_DB</key><string>$REMOTE_HOME/.smooth/operator.db</string>
        <key>SMOOTH_WORKSPACE</key><string>$REMOTE_HOME/dev</string>
        <!-- The daemon shells \`tailscale\`; a GUI agent needs it on PATH. -->
        <key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
        <key>HOME</key><string>$REMOTE_HOME</string>
        <key>RUST_LOG</key><string>info</string>
    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
PLIST

launchctl bootout "gui/$uid/$AGENT_LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$uid" "$AGENT_PLIST"
echo "  loaded LaunchAgent $AGENT_LABEL"
REMOTE

say "waiting for the daemon on :8443…"
for i in $(seq 1 20); do
    if curl -s -o /dev/null -m 3 "https://$HOST.tailc13b5a.ts.net:8443/health"; then
        echo "✅ Big Smooth app live on $HOST — daemon reachable at https://$HOST.tailc13b5a.ts.net:8443/"
        exit 0
    fi
    sleep 2
done
echo "⚠️  installed, but the daemon didn't answer :8443/health within 40s — check the app on $HOST." >&2
