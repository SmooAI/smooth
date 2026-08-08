#!/usr/bin/env bash
# Tests for render-model-scores.sh. Run: bash scripts/the-line/test-model-scores.sh
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
render="$here/render-model-scores.sh"
pass=0
fail=0

check() { # check <name> <condition-description> <actual> <expected-substring>
    if [[ "$3" == *"$4"* ]]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        echo "FAIL: $1"
        echo "  expected to contain: $4"
        echo "  got: $3"
    fi
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/docs"

cat >"$work/board.json" <<'JSON'
{
  "suite": "convo", "trials": 1, "scenario_count": 9,
  "models": [
    { "model": "deepseek-v4-flash", "pass_rate_pct": 88.9, "passed": 8, "conclusive": 9, "inconclusive": 0, "cost_usd": 0.0188, "cost_per_pass_usd": 0.0024, "duration_s": 204.1 },
    { "model": "unpriced", "pass_rate_pct": 55.5, "passed": 5, "conclusive": 9, "inconclusive": 0, "duration_s": 10.0 }
  ]
}
JSON

bash "$render" "$work/board.json" "$work/docs" >/dev/null

badge=$(cat "$work/docs/model-badge.json")
check "badge names the best model" "" "$badge" "deepseek-v4-flash 88.9%"
check "badge is green above 80" "" "$badge" "brightgreen"
check "badge is a shields endpoint" "" "$badge" '"schemaVersion": 1'

table=$(cat "$work/docs/Model-Leaderboard.md")
check "table lists both models" "" "$table" 'deepseek-v4-flash'
check "table carries the cost" "" "$table" '$0.0188'
# The regression that matters: an unmeasured cost must never render as $0.
check "missing cost renders as a dash" "" "$table" '| — | — |'
check "one-trial runs carry the noise warning" "" "$table" '1 trial per scenario'
check "scoreboard copied verbatim" "" "$(cat "$work/docs/model-scores.json")" '"suite": "convo"'

# Colour thresholds.
sed 's/88.9/72.0/' "$work/board.json" >"$work/mid.json"
bash "$render" "$work/mid.json" "$work/docs" >/dev/null
check "60-80 is yellow" "" "$(cat "$work/docs/model-badge.json")" "yellow"

sed 's/88.9/41.0/' "$work/board.json" >"$work/low.json"
bash "$render" "$work/low.json" "$work/docs" >/dev/null
check "below 60 is orange" "" "$(cat "$work/docs/model-badge.json")" "orange"

# Multi-trial runs drop the anecdote warning.
sed 's/"trials": 1/"trials": 3/' "$work/board.json" >"$work/multi.json"
bash "$render" "$work/multi.json" "$work/docs" >/dev/null
if grep -q "trial per scenario" "$work/docs/Model-Leaderboard.md"; then
    fail=$((fail + 1)); echo "FAIL: a 3-trial run should not warn about single trials"
else
    pass=$((pass + 1))
fi

# Failure modes: an empty board must not publish a badge claiming success.
echo '{"suite":"convo","trials":1,"scenario_count":0,"models":[]}' >"$work/empty.json"
if bash "$render" "$work/empty.json" "$work/docs" >/dev/null 2>&1; then
    fail=$((fail + 1)); echo "FAIL: an empty scoreboard should be rejected"
else
    pass=$((pass + 1))
fi

if bash "$render" "$work/nope.json" "$work/docs" >/dev/null 2>&1; then
    fail=$((fail + 1)); echo "FAIL: a missing input should be rejected"
else
    pass=$((pass + 1))
fi

echo "model-scores: $pass passed, $fail failed"
[[ "$fail" -eq 0 ]]
