#!/usr/bin/env bash
# th-mail watcher — blocks until unread `th msg` mail arrives, prints it as JSON,
# and EXITS. Designed to run as a Claude Code background Bash task: when it
# exits, the harness re-invokes the main agent, which surfaces the mail and
# re-arms the watcher.
#
# Since pearl th-374f85 this is a thin wrapper around `th msg watch --once
# --json` — the blocking poll lives in the binary, against the machine-level
# mail store (`~/.smooth/mail.db`). There is no remote to pull from and no
# single-writer lock to contend for, so the old `pull` argument is gone.
#
# Usage: watch-once.sh <agent-name> [interval-secs] [max-lifetime-secs]
#
# Exit 0 with a non-"[]" JSON array on stdout => new mail (re-arm after handling).
# Exit 0 with "[]" on stdout                  => timed out, no mail.
# Non-zero                                    => the mail store FAILED. Mail
#                                                state is unknown; it is NOT
#                                                "no mail" (pearl th-ad0701).
#
# It does NOT acknowledge anything: the main agent consumes via
# `th msg inbox --unread --mark-read` (or `th msg ack`) after surfacing, so
# nothing is lost if a watcher cycle's output is missed.

AGENT="${1:-${SMOOTH_AGENT:-}}"
INTERVAL="${2:-15}"
MAX="${3:-86400}"

flags=(--once --json --interval "$INTERVAL")
if [ -n "$AGENT" ]; then
    flags+=(--agent "$AGENT")
fi

# `th msg watch --once` blocks until mail arrives; the outer timeout is the
# lifetime cap. No `timeout` binary on stock macOS, so background + wait.
th msg watch "${flags[@]}" &
watcher=$!
( sleep "$MAX"; kill "$watcher" 2>/dev/null ) &
reaper=$!

wait "$watcher"
status=$?
kill "$reaper" 2>/dev/null

# 0 => mail was printed. Killed by the lifetime cap (any signal, 128+n) => the
# watcher timed out with nothing, which is genuinely "no mail".
[ "$status" -eq 0 ] && exit 0
if [ "$status" -ge 128 ]; then
    echo "[]"
    exit 0
fi

# Anything else is `th` itself failing — a broken/unreadable mail store, a full
# disk, no `th` on PATH. Reporting "[]" here is what made a 100%-full disk read
# as an empty inbox (pearl th-ad0701): a failed read must never wear the same
# output as a successful empty one.
echo "th msg watch failed (exit $status) — mail state UNKNOWN, not empty" >&2
exit "$status"
