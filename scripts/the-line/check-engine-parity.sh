#!/usr/bin/env bash
# Compare per-engine scoreboards against docs/engine-baseline.json.
#
# WHY A BASELINE AND NOT "ALL MUST PASS": two of the five engines are
# currently broken (th-11284c, th-df7007). A gate that demanded 100%
# everywhere would be red forever and would therefore be ignored, which
# is worse than no gate. A baseline fails on REGRESSION — the thing CI
# can actually act on — and reports an engine that starts working, so the
# exception gets removed rather than quietly becoming permanent.
#
# Usage:
#   check-engine-parity.sh <boards_dir> [baseline.json]
#
# `boards_dir` holds `board-<engine>.json` files as written by
# `smooth-bench agentic --scoreboard`.
#
# Exit 0 = no regression. Exit 1 = at least one engine below baseline.

set -uo pipefail

boards="${1:?usage: check-engine-parity.sh <boards_dir> [baseline.json]}"
baseline="${2:-docs/engine-baseline.json}"

command -v jq >/dev/null 2>&1 || { echo "check-engine-parity: jq not found" >&2; exit 2; }
[[ -d "$boards"   ]] || { echo "check-engine-parity: not a directory: $boards" >&2; exit 2; }
[[ -f "$baseline" ]] || { echo "check-engine-parity: baseline not found: $baseline" >&2; exit 2; }

regressions=0
improvements=0
missing=0
zeroes=0
engines=0

printf '%-8s %8s %8s   %s\n' engine actual baseline status
for engine in $(jq -r '.engines | keys[]' "$baseline"); do
    want=$(jq -r --arg e "$engine" '.engines[$e].min_pass_rate_pct' "$baseline")
    board="$boards/board-$engine.json"

    if [[ ! -f "$board" ]]; then
        printf '%-8s %8s %8s   %s\n' "$engine" "-" "$want" "NO BOARD (engine did not run)"
        missing=$((missing + 1))
        continue
    fi

    # An engine that produced only INCONCLUSIVE trials has no rate to
    # compare. Treat it as 0 — a crashing engine is a regression, not an
    # absence of data.
    got=$(jq -r '[.models[].pass_rate_pct] | if length == 0 then 0 else (add / length) end' "$board")

    verdict=$(awk -v g="$got" -v w="$want" 'BEGIN {
        if (g + 0 <  w + 0) print "REGRESSION";
        else if (g + 0 > w + 0) print "IMPROVED";
        else print "ok";
    }')
    printf '%-8s %7s%% %7s%%   %s\n' "$engine" "$got" "$want" "$verdict"
    [[ "$verdict" == "REGRESSION" ]] && regressions=$((regressions + 1))
    [[ "$verdict" == "IMPROVED"   ]] && improvements=$((improvements + 1))
    awk -v g="$got" 'BEGIN { exit !(g + 0 == 0) }' && zeroes=$((zeroes + 1))
    engines=$((engines + 1))
done

echo
if [[ "$improvements" -gt 0 ]]; then
    echo "$improvements engine(s) beat their baseline — raise it in $baseline so the gain is protected."
fi
if [[ "$missing" -gt 0 ]]; then
    echo "$missing engine(s) produced no scoreboard; treated as not-run, not as passing."
fi
# EVERY engine at a flat zero is not an engine finding. Five independent
# implementations do not break on the same night; one shared input did —
# usually an absent SMOOAI_GATEWAY_API_KEY, which lets every server boot
# happily and then answer nothing. Reporting that as N regressions cost two
# investigations before anyone read the logs (th-e73f0d), so say the real
# thing instead.
if [[ "$engines" -gt 1 && "$zeroes" -eq "$engines" ]]; then
    echo "FAIL: all $engines engine(s) scored 0% — that is a harness or credential fault, not $engines simultaneous regressions."
    echo "      Check SMOOAI_GATEWAY_API_KEY first, then the per-engine logs in the run artifact."
    exit 1
fi
if [[ "$regressions" -gt 0 ]]; then
    echo "FAIL: $regressions engine(s) below baseline."
    exit 1
fi
echo "OK: no engine regressed."
