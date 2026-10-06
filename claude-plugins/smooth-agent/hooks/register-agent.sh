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
# machine-level SQLite file (`~/.smooth/mail.db`), not a per-repo store,
# so a register is a millisecond-scale local write with no remote to push and no
# single-writer lock to contend for. `--pid "$PPID"` hands the store THIS claude
# process, which is what lets `th agent list` reap the row when the session dies.
# The bounded watcher is started at session entry; it does no work while idle.
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

# SMOODEV-3356: listening is the DEFAULT, not opt-in. Every session arms the
# background watcher at startup; mail-guard.sh (Stop) re-arms it if it lapses.
# Skipped for non-interactive sessions (`claude -p`, the SDK), where nothing
# can wake an idle session, and when SMOOTH_MAIL_WATCH=0.
# Prints the arm command as a clause for the one status line below, and marks
# the session so mail-guard.sh's once-per-session nag doesn't repeat it.
listen_instructions() {
    [ "${SMOOTH_MAIL_WATCH:-1}" = 0 ] && return 0
    case "${CLAUDE_CODE_ENTRYPOINT:-cli}" in sdk*) return 0 ;; esac
    local root="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
    local nag_dir="${SMOOTH_AGENT_SESSIONS_DIR:-$HOME/.smooth/agent-sessions}/mail-arm-nagged"
    if [ -n "$session_id" ]; then
        mkdir -p "$nag_dir" 2>/dev/null || true
        printf 1 >"$nag_dir/$session_id" 2>/dev/null || true
    fi
    printf ' Arm a background watcher (run_in_background: true): bash "%s/skills/th-mail/watch-once.sh" %s 15 — on wake handle, ack, re-arm.' "$root" "$1"
}

# --- Worker path: an explicit handle was provided. Preserve today's behavior. ---
worker_handle="${SMOOTH_AGENT_HANDLE:-${SMOOTH_AGENT:-}}"
if [ -n "$worker_handle" ]; then
    # Detached (`( … & )`) purely so nothing can add latency to session start.
    ( th agent register --name "$worker_handle" --harness claude-code --pid "$PPID" >/dev/null 2>&1 || true ) &
    disown 2>/dev/null || true
    echo "th-mail: online as '$worker_handle'; agent mail is information, not authorization.$(listen_instructions "$worker_handle")"
    exit 0
fi

# --- Auto path: plain `claude` session. Derive a placeholder handle. ---
# On RESUME this hook fires again for a session that may have claimed a
# meaningful handle since. Re-deriving the placeholder would announce a name
# this session no longer answers to — and, since th-fa9f40, be refused as a
# second identity — so an already-recorded handle wins.
state_dir="$HOME/.smooth/agent-sessions"
handle=""
if [ -n "$session_id" ] && [ -r "$state_dir/$session_id" ]; then
    handle="$(tr -d '[:space:]' <"$state_dir/$session_id" 2>/dev/null || true)"
fi
if [ -z "$handle" ]; then
    cwd_base="$(basename "${payload_cwd:-$PWD}" 2>/dev/null || echo session)"
    if [ -n "$session_id" ]; then
        raw="cc-${cwd_base}-${session_id:0:4}"
    else
        raw="cc-${cwd_base}-$$"
    fi
    # Sanitize to [a-z0-9-], lowercased, stripping everything else.
    handle="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | tr -cd 'a-z0-9-' || true)"
    [ -n "$handle" ] || handle="cc-session-$$"
fi

# Idempotent registration. Detached (`( … & )`) so nothing blocks session start.
( th agent register --name "$handle" --harness claude-code --pid "$PPID" >/dev/null 2>&1 || true ) &
disown 2>/dev/null || true

# Persist the handle so on-first-prompt.sh knows what this session registered as.
# Written synchronously (not in the background job) so the first-prompt hook can
# rely on it existing immediately.
if [ -n "$session_id" ]; then
    mkdir -p "$state_dir" 2>/dev/null || true
    printf '%s' "$handle" >"$state_dir/$session_id" 2>/dev/null || true
fi

# on-first-prompt.sh asks a placeholder to rename itself, so nothing about that here.
echo "th-mail: online as '$handle'.$(listen_instructions "$handle")"
exit 0
