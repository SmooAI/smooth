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

# Killed by the lifetime cap (or otherwise produced nothing) => no mail.
if [ "$status" -ne 0 ]; then
    echo "[]"
fi
exit 0
