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
#   docs/model-leaderboard.json : the scoreboard, verbatim (every model scored)
#   docs/model-scores.json   : the MODEL PICKER CATALOG — the scoreboard filtered
#                              to the offered set, plus `unbenched` (below)
#   docs/model-badge.json    : Shields.io endpoint JSON for the README
#   docs/Model-Leaderboard.md: the human table
#
# Colour thresholds match render-badge.sh so the two badges read
# consistently:
#   >= 80% brightgreen · >= 60% yellow · else orange
#
# The offered set (SMOODEV-3342 model policy, 2026-09-26): every client model
# picker — the web SPA, iOS/Android Big Smooth (which fetch docs/model-scores.json
# from main at runtime), `th code` — offers ONLY the gpt-6-luna family, Groq,
# and gpt-6-sol as the explicit high-quality choice. The bench may score any
# model; only offered ones reach docs/model-scores.json. Offered models the run
# did not score are listed under `unbenched` so clients show them as "not yet
# benched" instead of inventing a number. The leaderboard table and the badge
# still report every model the run scored.
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

# A run where nothing was CONCLUSIVE has no scores — it has an outage. The
# scoreboard still lists its models at `pass_rate_pct: 0.0`, and publishing
# that put "deepseek-v4-flash 0.0%" on the README badge for a run in which
# all 45 scenarios were inconclusive. Nobody reading a 0% badge concludes
# "the harness never got an answer"; they conclude the model is broken.
conclusive=$(jq -r '[.models[].conclusive] | add // 0' "$board")
[[ "$conclusive" -gt 0 ]] || {
    echo "render-model-scores: refusing to publish — every scenario was INCONCLUSIVE (0 conclusive across $count model(s))." >&2
    echo "render-model-scores: that is an outage, not a 0% score. Fix the run and re-render." >&2
    exit 1
}

offered='["gpt-6-luna","gpt-6-luna-fast","gpt-6-luna-high","gpt-6-sol","groq-gpt-oss-120b","groq-gpt-oss-20b","groq-qwen3.8-27b"]'

# Shipped mobile builds decode `models` strictly (a numeric pass_rate_pct on
# every entry) and fall back to their BUNDLED list — which predates the policy
# — when it is empty. So an empty filtered list must never be published.
offered_benched=$(cp "$board" "$docs/model-leaderboard.json"
jq --argjson o "$offered" '[.models[] | select(.model as $m | $o | index($m))] | length' "$board")
[[ "$offered_benched" -gt 0 ]] || {
    echo "render-model-scores: refusing to publish — the run scored none of the offered models." >&2
    echo "render-model-scores: an empty catalog sends shipped phone apps back to their bundled pre-policy list." >&2
    exit 1
}

cp "$board" "$docs/model-leaderboard.json"
jq --argjson o "$offered" '
    .models |= map(select(.model as $m | $o | index($m)))
    | .unbenched = ($o - [.models[].model])
' "$board" >"$docs/model-scores.json"

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
    echo "| model | pass | rate | cost | \$/pass | time | safety |"
    echo "| --- | --- | --- | --- | --- | --- | --- |"
    jq -r '.models[] |
        "| `\(.model)` | \(.passed)/\(.conclusive) | \(.pass_rate_pct)% | " +
        (if .cost_usd  then "$\(.cost_usd)"  else "—" end) + " | " +
        (if .cost_per_pass_usd then "$\(.cost_per_pass_usd)" else "—" end) + " | " +
        "\(.duration_s)s | " +
        (if (.safety_violations // 0) == 0 then "clean" else "⚠️ \(.safety_violations)" end) + " |"' "$board"
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
    echo "- **A missing cost (—)** means the run could not price the model at all"
    echo "  (the gateway publishes no rate for it). It is not \$0 — a zero would"
    echo "  sort first and win a value ranking it never earned."
    echo "- **safety** counts trials where the agent breached a safety invariant:"
    echo "  destroyed data it was told to protect, or leaked a secret. It is"
    echo "  deliberately separate from the pass rate. A model can fail a scenario"
    echo "  for skipping a required note while having protected the data perfectly"
    echo "  — reading the rate as a safety score gets that exactly backwards."
    echo "- Cheap models have repeatedly matched expensive ones here. Check \`\$/pass\`,"
    echo "  not just the rate, before promoting anything into the premium tier."
    echo
    echo "## Related"
    echo
    echo "- [[Engineering/Bench-Harness]] — how the suites work"
    echo "- [[Engineering/LLM-Request-Parameters]] — why a model can score 0% for a reason that isn't quality"
} >"$docs/Model-Leaderboard.md"

echo "render-model-scores: wrote $docs/model-scores.json, $docs/model-leaderboard.json, $docs/model-badge.json, $docs/Model-Leaderboard.md"
