#!/usr/bin/env bash
# th-mail watcher — blocks until unread `th msg` mail arrives, prints the unread
# messages as JSON, and EXITS. Designed to run as a Claude Code background Bash
# task: when it exits, the harness re-invokes the main agent, which surfaces the
# mail to the user and re-arms the watcher.
#
# Usage: watch-once.sh <agent-name> [interval-secs] [max-lifetime-secs] [pull]
#   agent-name        whose inbox to watch (the handle you registered)
#   interval-secs     seconds between polls (default 15)
#   max-lifetime-secs safety cap; exit quietly after this with no mail (default 86400 = 24h)
#   pull              "1" to `--pull` the Dolt remote each poll (cross-machine). DEFAULT "0":
#                     do NOT pull. `--pull` WRITES to the shared Dolt store (fetch/merge) and
#                     contends on the write lock — it caused a store-wide "Error 1105: database
#                     is read only" that blocked every agent's writes. For same-machine agents
#                     the mailbox is the same local store, so reads see new mail without pulling.
#
# Exit 0 with a non-"[]" JSON array on stdout  => new mail (re-arm after handling).
# Exit 0 with "[]" on stdout                   => timed out, no mail (optionally re-arm).
#
# Note: this does NOT mark messages read. The main agent consumes + marks read via
# `th msg inbox --unread --mark-read` after surfacing, so nothing is lost if a
# watcher cycle's output is missed.

AGENT="${1:-${SMOOTH_AGENT:-}}"
INTERVAL="${2:-15}"
MAX="${3:-86400}"
PULL="${4:-0}"

poll_flags=()
if [ -n "$AGENT" ]; then
    poll_flags=(--agent "$AGENT")
fi
if [ "$PULL" = "1" ]; then
    poll_flags+=(--pull)
fi

elapsed=0
while [ "$elapsed" -lt "$MAX" ]; do
    out="$(th msg inbox --unread --json "${poll_flags[@]}" 2>/dev/null || true)"
    compact="$(printf '%s' "$out" | tr -d '[:space:]')"
    if [ -n "$compact" ] && [ "$compact" != "[]" ] && [ "$compact" != "null" ]; then
        printf '%s\n' "$out"
        exit 0
    fi
    sleep "$INTERVAL"
    elapsed=$((elapsed + INTERVAL))
done

echo "[]"
exit 0
