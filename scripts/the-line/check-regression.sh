#!/usr/bin/env bash
# Regression gate — fail if this release's overall_pass_rate drops more than
# the allowed threshold (default 2.0 percentage points) from the last row in
# docs/bench-history.md.
#
# If the history file has no data rows yet, we're on the first release of The
# Line — nothing to compare against, and we exit 0.
#
# Inputs:
#   $1  path to score.json
#   $2  path to docs/bench-history.md
#
# Env:
#   REGRESSION_THRESHOLD_PP  default 2.0 (percentage points)
#
# Exit codes:
#   0  no regression (or first release)
#   1  regression — current drop exceeds threshold

set -euo pipefail

score_path="${1:?usage: check-regression.sh <score.json> <bench-history.md>}"
history_path="${2:?usage: check-regression.sh <score.json> <bench-history.md>}"
threshold="${REGRESSION_THRESHOLD_PP:-2.0}"

if ! command -v jq >/dev/null 2>&1; then
    echo "check-regression: jq not found on PATH" >&2
    exit 2
fi

current_pct=$(jq -r '.overall_pass_rate * 100' "$score_path")

if [[ ! -f "$history_path" ]]; then
    echo "check-regression: no history file yet — first release of The Line, skipping regression check" >&2
    exit 0
fi

# Data rows match: `| vX.Y.Z | YYYY-MM-DD | NN.N | ...`. The header/separator
# rows begin with `| version |` or `| ---` so the regex filters them out.
last_row=$(grep -E '^\| v[0-9]' "$history_path" | tail -n 1 || true)

if [[ -z "$last_row" ]]; then
    echo "check-regression: no prior data rows in $history_path — first release, skipping regression check" >&2
    exit 0
fi

# Row shape (1-indexed cells between pipes):
#   1=version 2=date 3=overall% 4=cpp% ... 11=sha
prev_pct=$(echo "$last_row" | awk -F'|' '{ gsub(/^[ \t]+|[ \t]+$/, "", $4); print $4 }')

if [[ -z "$prev_pct" ]]; then
    echo "check-regression: could not parse previous overall% from: $last_row" >&2
    exit 2
fi

drop=$(awk -v p="$prev_pct" -v c="$current_pct" 'BEGIN { printf "%.2f", p - c }')
exceeds=$(awk -v d="$drop" -v t="$threshold" 'BEGIN { print (d > t) ? 1 : 0 }')

echo "check-regression: previous=${prev_pct}% current=${current_pct}% drop=${drop}pp threshold=${threshold}pp"

if [[ "$exceeds" == "1" ]]; then
    echo "check-regression: FAIL — drop of ${drop}pp exceeds threshold of ${threshold}pp" >&2
    exit 1
fi

echo "check-regression: OK"
exit 0
