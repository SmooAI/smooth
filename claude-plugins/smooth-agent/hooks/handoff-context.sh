#!/usr/bin/env bash
# smooth-agent SessionStart hook, matcher `compact|resume` (pearl th-9483e8).
#
# After a compaction (or a `claude --resume`), inject the handoff packet of the
# in_progress pearl(s) THIS session is working on as the hook's
# `additionalContext`, so the fresh context knows what it is working on, where
# the work is, what happened and what to do next.
#
# Relevance is narrow on purpose (relevant-pearls.sh): in the primary checkout
# only pearls this session checkpointed or whose id is in the branch name. No
# relevant pearl → print nothing. At most $SMOOTH_HANDOFF_MAX packets (3), each
# cut to $SMOOTH_HANDOFF_PACKET_CHARS (1500).
#
# Never blocks: th/jq missing, no store, no matching pearls → exit 0 silently.
# Test: flow-hook.test.sh (SessionStart section) — override TH with a stub.
set -u

TH="${TH:-th}"
command -v "$TH" >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0
. "$(dirname "${BASH_SOURCE[0]}")/relevant-pearls.sh"

input="$(cat 2>/dev/null || true)"
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null || true)"
sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null || true)"
[ -n "$cwd" ] || cwd="$PWD"
[ -d "$cwd" ] || exit 0

ids="$(relevant_pearl_ids "$cwd" "$sid")"
[ -n "$ids" ] || exit 0

max="${SMOOTH_HANDOFF_MAX:-3}"
cap="${SMOOTH_HANDOFF_PACKET_CHARS:-1500}"
total=0
packet=""
for id in $ids; do
    total=$((total + 1))
    [ "$total" -le "$max" ] || continue
    one="$(cd "$cwd" && "$TH" pearls show "$id" --handoff 2>/dev/null || true)"
    # Long checkpoint histories are what bloat a resume; the full packet is one command away.
    [ "${#one}" -gt "$cap" ] && one="${one:0:$cap}"$'\n'"… (truncated — th pearls show $id --handoff)"
    [ -n "$one" ] && packet="$packet$one"$'\n\n'
done
[ -n "$packet" ] || exit 0
[ "$total" -gt "$max" ] && packet="${packet}… +$((total - max)) more — th pearls prime --in-progress --cwd ."$'\n'

context="$(printf 'Resumed. Handoff for the pearl(s) this session is on (checkpoint with `th pearls checkpoint <id> --note "…" --next "…"`):\n\n%s' "$packet")"
jq -cn --arg ctx "$context" '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $ctx}}'
exit 0
