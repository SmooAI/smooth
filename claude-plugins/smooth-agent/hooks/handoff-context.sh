#!/usr/bin/env bash
# smooth-agent SessionStart hook, matcher `compact|resume` (pearl th-9483e8).
#
# After a compaction (or a `claude --resume`), inject the handoff packet of
# every in_progress pearl that belongs to this worktree as the hook's
# `additionalContext`, so the fresh context knows what it is working on, where
# the work is (worktree/branch/HEAD/dirty), what happened (checkpoint notes),
# and what to do next — without re-deriving any of it.
#
# Output is the SessionStart JSON envelope on stdout. Never blocks: th/jq
# missing, no store, no matching pearls → exit 0 and print nothing.
# Test: flow-hook.test.sh (SessionStart section) — override TH with a stub.
set -u

TH="${TH:-th}"
command -v "$TH" >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0

input="$(cat 2>/dev/null || true)"
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null || true)"
[ -n "$cwd" ] || cwd="$PWD"
[ -d "$cwd" ] || exit 0

packet="$(cd "$cwd" && "$TH" pearls prime --in-progress --cwd "$cwd" 2>/dev/null || true)"
case "$packet" in
    "" | "No in-progress pearls"*) exit 0 ;;
esac

context="$(printf 'Resuming after compaction/resume. Handoff packet(s) for the pearl(s) this worktree is working on (from `th pearls prime --in-progress --cwd .`; `th pearls checkpoint <id> --note "…" --next "…"` at each milestone):\n\n%s' "$packet")"
jq -cn --arg ctx "$context" '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $ctx}}'
exit 0
