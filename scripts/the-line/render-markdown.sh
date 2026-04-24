#!/usr/bin/env bash
# Render a Score JSON into two markdown artifacts:
#   1. $SUMMARY_OUT — multi-line block for $GITHUB_STEP_SUMMARY and release notes.
#   2. $HISTORY_ROW_OUT — one pipe-delimited row for docs/bench-history.md.
#
# Inputs:
#   $1            path to score.json
#   $2            release version (e.g. v0.42.1)
#   SUMMARY_OUT   env — where to write the summary block
#   HISTORY_ROW_OUT env — where to write the single history row
#
# Requires: jq. Fails fast on any missing field.
#
# The Line columns (in order): cpp, go, java, javascript, python, rust —
# sorted lexicographically to match what `smooth-bench` emits via BTreeMap,
# so downstream grep patterns stay stable.

set -euo pipefail

score_path="${1:?usage: render-markdown.sh <score.json> <version>}"
version="${2:?usage: render-markdown.sh <score.json> <version>}"
summary_out="${SUMMARY_OUT:?SUMMARY_OUT env required}"
history_row_out="${HISTORY_ROW_OUT:?HISTORY_ROW_OUT env required}"

if ! command -v jq >/dev/null 2>&1; then
    echo "render-markdown: jq not found on PATH" >&2
    exit 1
fi

if [[ ! -f "$score_path" ]]; then
    echo "render-markdown: score file not found: $score_path" >&2
    exit 1
fi

pct() { jq -r "$1 * 100 | . * 10 | round / 10 | tostring" "$score_path"; }
num() { jq -r "$1" "$score_path"; }

overall_pct=$(pct '.overall_pass_rate')
cpp_pct=$(pct '.by_language.cpp.pass_rate')
go_pct=$(pct '.by_language.go.pass_rate')
java_pct=$(pct '.by_language.java.pass_rate')
js_pct=$(pct '.by_language.javascript.pass_rate')
py_pct=$(pct '.by_language.python.pass_rate')
rust_pct=$(pct '.by_language.rust.pass_rate')

cost=$(jq -r '.cost_usd | . * 100 | round / 100 | tostring' "$score_path")
median_ms=$(num '.median_task_ms')
median_s=$(awk -v ms="$median_ms" 'BEGIN { printf "%.1f", ms/1000 }')
tasks_green=$(num '.tasks_green')
tasks_attempted=$(num '.tasks_attempted')
commit_sha=$(num '.commit_sha')
short_sha="${commit_sha:0:12}"
ran_at=$(num '.ran_at')
ran_date="${ran_at:0:10}"
budget_hit=$(num '.budget_usd_hit')

{
    echo "### The Line — $version"
    echo
    echo "**Overall pass rate: ${overall_pct}%** (${tasks_green}/${tasks_attempted} tasks green)"
    echo
    echo "| Metric | Value |"
    echo "| --- | --- |"
    echo "| cpp | ${cpp_pct}% |"
    echo "| go | ${go_pct}% |"
    echo "| java | ${java_pct}% |"
    echo "| javascript | ${js_pct}% |"
    echo "| python | ${py_pct}% |"
    echo "| rust | ${rust_pct}% |"
    echo "| cost | \$${cost} |"
    echo "| median task | ${median_s}s |"
    echo "| commit | \`${short_sha}\` |"
    if [[ "$budget_hit" == "true" ]]; then
        echo
        echo "> Budget cap hit — score is partial."
    fi
} >"$summary_out"

printf '| %s | %s | %s | %s | %s | %s | %s | %s | %s | $%s | %s |\n' \
    "$version" \
    "$ran_date" \
    "$overall_pct" \
    "$cpp_pct" \
    "$go_pct" \
    "$java_pct" \
    "$js_pct" \
    "$py_pct" \
    "$rust_pct" \
    "$cost" \
    "$short_sha" \
    >"$history_row_out"
