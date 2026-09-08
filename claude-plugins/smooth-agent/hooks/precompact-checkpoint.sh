#!/usr/bin/env bash
# smooth-agent PreCompact hook (pearl th-9483e8).
#
# Right before the harness compacts the context, snapshot the handoff state of
# every in_progress pearl that belongs to this session's worktree (recorded
# worktree == cwd's repo, or the pearl id is in the branch name) with
# `th pearls checkpoint <id> --auto`. The post-compaction SessionStart
# (handoff-context.sh) reads it back so the session resumes cold.
#
# Never blocks: th/jq missing, no store, no matching pearls → exit 0, silent.
# Test: flow-hook.test.sh (PreCompact section) — override TH with a stub.
set -u

TH="${TH:-th}"
command -v "$TH" >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0

input="$(cat 2>/dev/null || true)"
session_id="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null || true)"
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null || true)"
[ -n "$cwd" ] || cwd="$PWD"
[ -d "$cwd" ] || exit 0

ids="$(cd "$cwd" && "$TH" pearls prime --in-progress --cwd "$cwd" --json 2>/dev/null | jq -r '.[].pearl.id' 2>/dev/null || true)"
for id in $ids; do
    (cd "$cwd" && "$TH" pearls checkpoint "$id" --auto --cwd "$cwd" ${session_id:+--session-id "$session_id"} >/dev/null 2>&1) || true
done
exit 0
