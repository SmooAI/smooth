#!/usr/bin/env bash
# Render a Score JSON into a Shields.io "endpoint" badge JSON at $2.
#
# Why a separate file from docs/bench-latest.json: The Line's release
# artefact needs to serve two readers with incompatible shapes —
#   - docs/bench-latest.json  : raw Score JSON, consumed by the `th`
#                                 binary's build.rs (baked into the
#                                 binary as BENCH_SCORE_JSON so
#                                 `th bench score` can reprint it).
#   - docs/bench-badge.json   : Shields.io endpoint JSON (this file's
#                                 output), consumed by img.shields.io
#                                 to render the README badge.
# Same source (score.json), two artefacts. The workflow writes both on
# every tag.
#
# Consumed by the README badge:
#   https://img.shields.io/endpoint?url=.../docs/bench-badge.json
#
# Input score.json shape comes from smooth_bench::score::Score — only
# `overall_pass_rate` (a fraction 0.0–1.0) and `budget_usd_hit` are used.
#
# Color thresholds (matches the README/plan — not red, since a low Line
# means "below target", not "broken"):
#   >= 0.80  → brightgreen
#   >= 0.60  → yellow
#   else     → orange
#
# When `budget_usd_hit` is true the score is partial, so we suffix the
# message with a warning glyph so the badge visibly differs from a
# clean sweep.
#
# Usage:
#   render-badge.sh <score.json> <output.json>

set -euo pipefail

score_path="${1:?usage: render-badge.sh <score.json> <output.json>}"
out_path="${2:?usage: render-badge.sh <score.json> <output.json>}"

if ! command -v jq >/dev/null 2>&1; then
    echo "render-badge: jq not found on PATH" >&2
    exit 1
fi

if [[ ! -f "$score_path" ]]; then
    echo "render-badge: score file not found: $score_path" >&2
    exit 1
fi

pass_rate=$(jq -r '.overall_pass_rate' "$score_path")
budget_hit=$(jq -r '.budget_usd_hit' "$score_path")

# Round to one decimal place — matches the summary table + history row
# so a reader comparing the three sees consistent numbers.
message=$(jq -r '(.overall_pass_rate * 100 * 10 | round / 10 | tostring) + "%"' "$score_path")

# Budget-cap flag → partial sample. Suffix with ⚠ so the badge is
# visually distinct and viewers know to check bench-history.md.
if [[ "$budget_hit" == "true" ]]; then
    message="${message} ⚠"
fi

# Threshold → color. awk handles the float comparison portably
# (bash -lt/-gt only work on integers).
color=$(awk -v r="$pass_rate" 'BEGIN {
    if (r + 0 >= 0.80) print "brightgreen";
    else if (r + 0 >= 0.60) print "yellow";
    else print "orange";
}')

jq -n \
    --arg message "$message" \
    --arg color "$color" \
    '{schemaVersion: 1, label: "the line", message: $message, color: $color}' \
    >"$out_path"

echo "render-badge: wrote $out_path"
