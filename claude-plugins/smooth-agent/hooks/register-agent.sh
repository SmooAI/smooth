#!/usr/bin/env bash
# smooth-agent SessionStart hook.
#
# Registers THIS Claude Code session on the th-mail bus so Big Smooth — and any
# other agent — can reach it by name. stdout from a SessionStart hook is injected
# into the session as context.
#
# EVERY session registers (register-always), not just `th claude run` workers —
# the whole point is that agents are active and mailable by default:
#   - Worker sessions (`th claude run`) carry SMOOTH_AGENT_HANDLE (their session
#     id, already task-meaningful) → register under it, keep today's message.
#   - A plain `claude` launch gets a PLACEHOLDER handle derived from the session
#     (`cc-<cwd-basename>-<sid4>`) so it is addressable immediately, then is
#     nudged once (on-first-prompt.sh) to rename itself to something meaningful.
#
# Registration is always safe and cheap: as of pearl th-374f85 the mailbox is a
# machine-level SQLite file (`~/.smooth/mail.db`), not the per-repo Dolt store,
# so a register is a millisecond-scale local write with no remote to push and no
# single-writer lock to contend for. `--pid "$PPID"` hands the store THIS claude
# process, which is what lets `th agent list` reap the row when the session dies.
# Push-watching still stays opt-in via the /th-mail skill.
#
# SessionStart delivers a JSON payload on stdin (session_id, cwd, source). We
# read it once and parse defensively: a missing jq or empty stdin degrades to a
# fallback handle, never a non-zero exit (a SessionStart hook that fails is bad).
set -euo pipefail

# th absent (e.g. an outside contributor without the CLI) → nothing to do. The
# plugin's other pieces (/smooth, skills, guardrail hooks) still work.
command -v th >/dev/null 2>&1 || exit 0

INPUT="$(cat 2>/dev/null || true)"

# Parse the stdin payload defensively. jq missing / empty stdin → empty fields.
session_id=""
payload_cwd=""
if [ -n "$INPUT" ] && command -v jq >/dev/null 2>&1; then
    session_id="$(printf '%s' "$INPUT" | jq -r '.session_id // empty' 2>/dev/null || true)"
    payload_cwd="$(printf '%s' "$INPUT" | jq -r '.cwd // empty' 2>/dev/null || true)"
fi

# --- Worker path: an explicit handle was provided. Preserve today's behavior. ---
worker_handle="${SMOOTH_AGENT_HANDLE:-${SMOOTH_AGENT:-}}"
if [ -n "$worker_handle" ]; then
    # Detached (`( … & )`) purely so nothing can add latency to session start.
    ( th agent register --name "$worker_handle" --harness claude-code --pid "$PPID" >/dev/null 2>&1 || true ) &
    disown 2>/dev/null || true
    echo "th-mail: online as agent '$worker_handle'. You are reachable by Big Smooth and other agents — answer pings and coordinate with the agent-comms skill; check your inbox with 'th msg inbox --agent $worker_handle'. Track work as pearls (pearls-flow skill)."
    exit 0
fi

# --- Auto path: plain `claude` session. Derive a placeholder handle. ---
cwd_base="$(basename "${payload_cwd:-$PWD}" 2>/dev/null || echo session)"
if [ -n "$session_id" ]; then
    raw="cc-${cwd_base}-${session_id:0:4}"
else
    raw="cc-${cwd_base}-$$"
fi
# Sanitize to [a-z0-9-], lowercased, stripping everything else.
handle="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | tr -cd 'a-z0-9-' || true)"
[ -n "$handle" ] || handle="cc-session-$$"

# Idempotent registration. Detached (`( … & )`) so nothing blocks session start.
( th agent register --name "$handle" --harness claude-code --pid "$PPID" >/dev/null 2>&1 || true ) &
disown 2>/dev/null || true

# Persist the handle so on-first-prompt.sh knows what this session registered as.
# Written synchronously (not in the background job) so the first-prompt hook can
# rely on it existing immediately.
if [ -n "$session_id" ]; then
    state_dir="$HOME/.smooth/agent-sessions"
    mkdir -p "$state_dir" 2>/dev/null || true
    printf '%s' "$handle" >"$state_dir/$session_id" 2>/dev/null || true
fi

echo "th-mail: online as agent '$handle' (a placeholder). You are on the bus and reachable by Big Smooth and other agents — check your inbox with 'th msg inbox --agent $handle'. Once your task is clear, claim a task-meaningful handle with 'th agent claim <new-handle>' (carries your mail over). Push-watching for incoming mail stays opt-in via the /th-mail skill."
exit 0
