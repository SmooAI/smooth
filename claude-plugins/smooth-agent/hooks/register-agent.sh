#!/usr/bin/env bash
# smooth-agent SessionStart hook.
#
# Registers this Claude Code session on the th-mail bus so Big Smooth can
# reach it. No-op unless the session was launched by `th claude run`, which
# exports SMOOTH_AGENT_HANDLE=<session-id>. stdout from a SessionStart hook
# is injected into the session as context.
set -euo pipefail

handle="${SMOOTH_AGENT_HANDLE:-}"

# Not a Big Smooth worker (a plain `claude` launch) → nothing to do.
[ -z "$handle" ] && exit 0
command -v th >/dev/null 2>&1 || exit 0

# Idempotent registration; swallow errors so a hiccup never blocks startup.
th agent register --name "$handle" --harness claude-code >/dev/null 2>&1 || true

echo "th-mail: online as agent '$handle'. Report status to Big Smooth and answer pings with the agent-comms skill; check 'th msg inbox --agent $handle'. Track work as pearls (pearls-flow skill)."
exit 0
