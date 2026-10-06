#!/usr/bin/env bash
# Shared by precompact-checkpoint.sh and handoff-context.sh: print the ids of
# the in_progress pearls that belong to THIS session, one per line.
#
# `th pearls prime --in-progress --cwd` matches on the recorded worktree. In a
# linked worktree that is specific enough. In the primary checkout it is not:
# every pearl ever checkpointed from there matches, and on a busy repo that
# injected ~95k chars into every compact/resume. So in the primary checkout a
# pearl only counts when this session checkpointed it last or its id is in the
# branch name.
#
# Usage (sourced): relevant_pearl_ids <cwd> <session_id>   — needs $TH and jq.
relevant_pearl_ids() {
    local cwd="$1" sid="$2" json branch primary=false gd common
    json="$(cd "$cwd" && "$TH" pearls prime --in-progress --cwd "$cwd" --json 2>/dev/null)" || return 0
    [ -n "$json" ] || return 0
    gd="$(git -C "$cwd" rev-parse --path-format=absolute --git-dir 2>/dev/null || true)"
    common="$(git -C "$cwd" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
    [ -n "$gd" ] && [ "$gd" = "$common" ] && primary=true
    branch="$(git -C "$cwd" branch --show-current 2>/dev/null || true)"
    printf '%s' "$json" | jq -r --arg sid "$sid" --arg br "$branch" --argjson primary "$primary" '
        .[]? | .pearl.id as $id | select(
            ($primary | not)
            or ($sid != "" and (.handoff.agent_session_id // "") == $sid)
            or ($br != "" and ($br | contains($id)))
        ) | $id' 2>/dev/null || true
}
