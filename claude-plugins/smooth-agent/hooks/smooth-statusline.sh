#!/usr/bin/env bash
# smooth-statusline: shows which th-mail agent this Claude Code session is, how
# much mail is waiting for it (pearl th-2f33b6), and how full its context is
# against the session's budget (SMOODEV-3891).
#
#     ⚙ th:fix-auth ✉3 ◔ 162k/300k 54%
#
# The context segment comes from `th harness budget --statusline`: dim under
# harness.context_suggest, yellow past it (a good time to /th-clear), red past
# harness.context_budget_warn (auto-compaction is near).
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
# exits 0 silently whenever there's nothing to say: no handle (no jq, no session
# id, no state file) and no context size (no th). A statusline that errors is noise on every single render.
set -uo pipefail

INPUT="$(cat 2>/dev/null || true)"
[ -n "$INPUT" ] || exit 0

DIM=$'\033[2m'
CYAN=$'\033[36m'
YELLOW=$'\033[33m'
RESET=$'\033[0m'

# A statusline runs on every render, so cap each `th` call if the box has a
# timeout(1). `th` on a stripped PATH, a locked db, a stale binary: all just
# mean that segment is missing.
TIMEOUT=""
for t in timeout gtimeout; do
    command -v "$t" >/dev/null 2>&1 && { TIMEOUT="$t"; break; }
done
HAVE_TH=""
command -v th >/dev/null 2>&1 && HAVE_TH=1

HANDLE=""
if command -v jq >/dev/null 2>&1; then
    SESSION_ID="$(printf '%s' "$INPUT" | jq -r '.session_id // empty' 2>/dev/null || true)"
    STATE_DIR="${SMOOTH_AGENT_SESSIONS_DIR:-$HOME/.smooth/agent-sessions}"
    [ -n "$SESSION_ID" ] && HANDLE="$(cat "$STATE_DIR/$SESSION_ID" 2>/dev/null || true)"
    # Strip ALL whitespace, not just newlines: handles are `[a-z0-9-]` by
    # construction, so any is stray, and a file holding only spaces must read as
    # "no handle" rather than render `th:  `.
    HANDLE="$(printf '%s' "$HANDLE" | tr -d '[:space:]')"
fi

# The mailbox is machine-local SQLite, so this is a sub-millisecond read.
UNREAD=""
if [ -n "$HANDLE" ] && [ -n "$HAVE_TH" ]; then
    UNREAD="$($TIMEOUT ${TIMEOUT:+1} th msg unread-count --agent "$HANDLE" 2>/dev/null || true)"
    # Only a bare positive integer is worth rendering.
    [[ "$UNREAD" =~ ^[0-9]+$ ]] || UNREAD=""
    [ "${UNREAD:-0}" -gt 0 ] 2>/dev/null || UNREAD=""
fi

# Context size vs the budget, read from the same payload. One line, or nothing.
CTX=""
if [ -n "$HAVE_TH" ]; then
    CTX="$(printf '%s' "$INPUT" | $TIMEOUT ${TIMEOUT:+1} th harness budget --harness claude-code --statusline 2>/dev/null || true)"
    CTX="${CTX%%$'\n'*}"
    [[ "$CTX" == *◔* ]] || CTX=""
fi

OUT=""
[ -n "$HANDLE" ] && OUT="${DIM}⚙ ${RESET}${CYAN}th:${HANDLE}${RESET}"
[ -n "$UNREAD" ] && OUT="$OUT ${YELLOW}✉${UNREAD}${RESET}"
[ -n "$CTX" ] && OUT="${OUT:+$OUT }$CTX"
printf '%s' "$OUT"
exit 0
