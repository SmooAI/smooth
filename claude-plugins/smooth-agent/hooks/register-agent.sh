#!/usr/bin/env bash
# smooth-agent SessionStart hook.
#
# Registers THIS Claude Code session on the th-mail bus so Big Smooth — and any
# other agent — can reach it by name. stdout from a SessionStart hook is injected
# into the session as context.
#
# Every session that has the plugin enabled registers, not just `th claude run`
# workers — the whole point is that agents are active and mailable by default:
#   - Worker sessions (`th claude run`) carry SMOOTH_AGENT_HANDLE (their session
#     id) → register under it.
#   - A `SMOOTH_AGENT` override, if set, wins next.
#   - A plain `claude` launch derives a STABLE per-(user, host, repo) handle so it
#     is addressable too — e.g. `brent@laptop/smooai`. Stable + idempotent means
#     one registry entry per checkout, refreshed each session, not unbounded
#     growth. (Two concurrent sessions in the same checkout share the handle;
#     that's the intended "reach whoever's working in <repo>" model.)
set -euo pipefail

# th absent (e.g. an outside contributor without the CLI) → nothing to do. The
# plugin's other pieces (/smooth, skills, guardrail hooks) still work.
command -v th >/dev/null 2>&1 || exit 0

handle="${SMOOTH_AGENT_HANDLE:-${SMOOTH_AGENT:-}}"
if [ -z "$handle" ]; then
    repo="$(basename "$(git rev-parse --show-toplevel 2>/dev/null || pwd)")"
    host="$(hostname -s 2>/dev/null || hostname 2>/dev/null || echo host)"
    handle="$(whoami)@${host}/${repo}"
fi

# Idempotent registration; swallow errors (offline, no auth, wedged store) so a
# hiccup never blocks session startup. `th agent register` is push-by-default so
# other clones' `th agent list` see this agent.
th agent register --name "$handle" --harness claude-code >/dev/null 2>&1 || true

echo "th-mail: online as agent '$handle'. You are reachable by Big Smooth and other agents — answer pings and coordinate with the agent-comms skill; check your inbox with 'th msg inbox --agent $handle'. Track work as pearls (pearls-flow skill)."
exit 0
