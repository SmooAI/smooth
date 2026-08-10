#!/usr/bin/env bash
# smooth-statusline: shows which th-mail agent this Claude Code session is, and
# how much mail is waiting for it. Pearl th-2f33b6.
#
#     ⚙ th:fix-auth ✉3
#
# Why it's worth a statusline: the SessionStart hook registers every session
# under a handle, but that handle scrolls off the top of the transcript within a
# minute. Agents then guess it, guess wrong, and mail the void. The statusline
# is the one place it stays visible — and the unread count is the only signal
# that someone is waiting on you when you aren't running the /th-mail watcher.
#
# Claude Code passes the statusline a JSON payload on stdin; `session_id` is a
# top-level string (docs: https://code.claude.com/docs/en/statusline). The
# SessionStart hook writes the registered handle to
# ~/.smooth/agent-sessions/<session_id> SYNCHRONOUSLY, so there is no new writer
# here and no race — we only ever read.
#
# Prints with NO trailing newline (Claude Code takes the first line as-is), and
# exits 0 silently whenever there's nothing to say: no jq, no session id, no
# state file, no th. A statusline that errors is noise on every single render.
set -uo pipefail

INPUT="$(cat 2>/dev/null || true)"
[ -n "$INPUT" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

SESSION_ID="$(printf '%s' "$INPUT" | jq -r '.session_id // empty' 2>/dev/null || true)"
[ -n "$SESSION_ID" ] || exit 0

STATE_DIR="${SMOOTH_AGENT_SESSIONS_DIR:-$HOME/.smooth/agent-sessions}"
HANDLE="$(cat "$STATE_DIR/$SESSION_ID" 2>/dev/null || true)"
# Strip ALL whitespace, not just newlines: handles are `[a-z0-9-]` by
# construction, so any is stray, and a file holding only spaces must read as
# "no handle" rather than render `th:  `.
HANDLE="$(printf '%s' "$HANDLE" | tr -d '[:space:]')"
[ -n "$HANDLE" ] || exit 0

DIM=$'\033[2m'
CYAN=$'\033[36m'
YELLOW=$'\033[33m'
RESET=$'\033[0m'

# The mailbox is machine-local SQLite, so this is a sub-millisecond read — but a
# statusline runs on every render, so cap it anyway if the box has a timeout(1).
# `th` on a stripped PATH, a locked db, a stale binary: all just mean no count.
UNREAD=""
if command -v th >/dev/null 2>&1; then
    TIMEOUT=""
    for t in timeout gtimeout; do
        command -v "$t" >/dev/null 2>&1 && { TIMEOUT="$t"; break; }
    done
    UNREAD="$($TIMEOUT ${TIMEOUT:+1} th msg unread-count --agent "$HANDLE" 2>/dev/null || true)"
    # Only a bare positive integer is worth rendering.
    [[ "$UNREAD" =~ ^[0-9]+$ ]] || UNREAD=""
    [ "${UNREAD:-0}" -gt 0 ] 2>/dev/null || UNREAD=""
fi

printf '%s' "${DIM}⚙ ${RESET}${CYAN}th:${HANDLE}${RESET}"
[ -n "$UNREAD" ] && printf '%s' " ${YELLOW}✉${UNREAD}${RESET}"
exit 0
