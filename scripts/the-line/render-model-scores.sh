#!/usr/bin/env bash
# Render a smooth-bench Scoreboard JSON into the published model artefacts.
#
# The Line (render-badge.sh) answers "is the agent getting better?" — one
# number over time, one model. This answers a different question: "which
# MODEL should we run?" Those are separate badges on purpose; folding a
# per-model comparison into The Line's single number would make a routing
# change look like a quality regression.
#
# Inputs
#   $1  scoreboard.json  — `smooth-bench convo --scoreboard <path>`
#
# Outputs
#   docs/model-scores.json   : the scoreboard, verbatim (machine-readable)
#   docs/model-badge.json    : Shields.io endpoint JSON for the README
#   docs/Model-Leaderboard.md: the human table
#
# Colour thresholds match render-badge.sh so the two badges read
# consistently:
#   >= 80% brightgreen · >= 60% yellow · else orange
#
# Usage:
#   render-model-scores.sh <scoreboard.json> [docs_dir]

set -euo pipefail

board="${1:?usage: render-model-scores.sh <scoreboard.json> [docs_dir]}"
docs="${2:-docs}"

command -v jq >/dev/null 2>&1 || { echo "render-model-scores: jq not found on PATH" >&2; exit 1; }
[[ -f "$board" ]] || { echo "render-model-scores: not found: $board" >&2; exit 1; }
[[ -d "$docs" ]] || { echo "render-model-scores: not a directory: $docs" >&2; exit 1; }

count=$(jq -r '.models | length' "$board")
[[ "$count" -gt 0 ]] || { echo "render-model-scores: scoreboard has no models" >&2; exit 1; }

cp "$board" "$docs/model-scores.json"

suite=$(jq -r '.suite' "$board")
trials=$(jq -r '.trials' "$board")
scenarios=$(jq -r '.scenario_count' "$board")
best_model=$(jq -r '.models[0].model' "$board")
best_pct=$(jq -r '.models[0].pass_rate_pct' "$board")

color=$(awk -v r="$best_pct" 'BEGIN {
    if (r + 0 >= 80) print "brightgreen";
    else if (r + 0 >= 60) print "yellow";
    else print "orange";
}')

jq -n --arg m "$best_model ${best_pct}%" --arg c "$color" \
    '{schemaVersion: 1, label: "model bench", message: $m, color: $c}' \
    >"$docs/model-badge.json"

# The table. `cost_usd` is absent when the run could not resolve it above
# the gateway key's noise — render that as "—" rather than $0.00, which
# would read as free.
{
    echo "# Model Leaderboard"
    echo
    echo "#engineering"
    echo
    echo "> [!info] Which model should Smooth run?"
    echo "> Scored by \`smooth-bench $suite\` on $scenarios scenario(s), $trials trial(s) each."
    echo "> Regenerate with \`smooth-bench $suite --model A --model B --scoreboard board.json\`"
    echo "> then \`scripts/the-line/render-model-scores.sh board.json\`."
    echo
    echo "| model | pass | rate | cost | \$/pass | time |"
    echo "| --- | --- | --- | --- | --- | --- |"
    jq -r '.models[] |
        "| `\(.model)` | \(.passed)/\(.conclusive) | \(.pass_rate_pct)% | " +
        (if .cost_usd  then "$\(.cost_usd)"  else "—" end) + " | " +
        (if .cost_per_pass_usd then "$\(.cost_per_pass_usd)" else "—" end) + " | " +
        "\(.duration_s)s |"' "$board"
    echo
    echo "## Reading this"
    echo
    echo "- **Percentages are per suite.** \`convo\` and \`agentic\` measure different"
    echo "  things; do not compare a number here against one from the other suite."
    if [[ "$trials" -lt 2 ]]; then
        echo "- ⚠️ **$trials trial per scenario** — agent behaviour is stochastic, so a"
        echo "  one-scenario gap between two models is noise, not a ranking. Re-run with"
        echo "  \`--trials 3\` before acting on a close result."
    fi
    echo "- **A missing cost (—)** means the run could not resolve spend above the"
    echo "  shared gateway key's background traffic. It is not \$0."
    echo "- Cheap models have repeatedly matched expensive ones here. Check \`\$/pass\`,"
    echo "  not just the rate, before promoting anything into the premium tier."
    echo
    echo "## Related"
    echo
    echo "- [[Engineering/Bench-Harness]] — how the suites work"
    echo "- [[Engineering/LLM-Request-Parameters]] — why a model can score 0% for a reason that isn't quality"
} >"$docs/Model-Leaderboard.md"

echo "render-model-scores: wrote $docs/model-scores.json, $docs/model-badge.json, $docs/Model-Leaderboard.md"
