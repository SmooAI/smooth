#!/usr/bin/env bash
# Merge a new smooth-bench scoreboard INTO an existing one, so benching two
# new models does not wipe the fourteen already on the board.
#
# `render-model-scores.sh` publishes exactly the board it is given. A run of
# `--model gpt-6-luna --model claude-haiku-5-5@anthropic` produces a board of
# two models; rendering it directly would replace the whole leaderboard with
# those two (SMOODEV-4125). Merge first, then render the result.
#
# Rows are only comparable when they were measured the same way, so the two
# boards must agree on `suite`, `trials` and `scenario_count`; anything else
# is refused rather than silently mixed. A model on both boards takes the
# NEW row. The result is ranked like `leaderboard::ranked`: pass rate
# descending, then cheaper first, unpriced last.
#
# Usage:
#   merge-model-scores.sh <existing.json> <new.json> > merged.json

set -euo pipefail

base="${1:?usage: merge-model-scores.sh <existing.json> <new.json>}"
new="${2:?usage: merge-model-scores.sh <existing.json> <new.json>}"

command -v jq >/dev/null 2>&1 || { echo "merge-model-scores: jq not found on PATH" >&2; exit 1; }
for f in "$base" "$new"; do
    [[ -f "$f" ]] || { echo "merge-model-scores: not found: $f" >&2; exit 1; }
done

for key in suite trials scenario_count; do
    a=$(jq -c ".$key" "$base")
    b=$(jq -c ".$key" "$new")
    [[ "$a" == "$b" ]] || {
        echo "merge-model-scores: refusing to merge — $key differs ($a vs $b); those rows were not measured the same way" >&2
        exit 1
    }
done

jq -s '
    .[0] as $base | .[1] as $new
    | ($new.models | map(.model)) as $replaced
    | $base
    | .models = (
        [$base.models[] | select(.model as $m | $replaced | index($m) | not)] + $new.models
        | sort_by(-.pass_rate_pct, (.cost_usd // infinite))
      )
' "$base" "$new"
