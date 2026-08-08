#!/usr/bin/env bash
# Tests for check-engine-parity.sh. Run: bash scripts/the-line/test-engine-parity.sh
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
check="$here/check-engine-parity.sh"
pass=0; fail=0

work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
mkdir -p "$work/boards"

cat >"$work/baseline.json" <<'JSON'
{ "engines": {
  "rust": { "min_pass_rate_pct": 100.0 },
  "ts":   { "min_pass_rate_pct": 0.0 }
} }
JSON

board() { # board <engine> <pct>
    printf '{"suite":"agentic","trials":1,"scenario_count":2,"models":[{"model":"m","pass_rate_pct":%s,"passed":1,"conclusive":1,"inconclusive":0,"duration_s":1.0}]}\n' "$2" \
        >"$work/boards/board-$1.json"
}

ok() { pass=$((pass+1)); }
no() { fail=$((fail+1)); echo "FAIL: $1"; }

# At baseline → exit 0
board rust 100.0; board ts 0.0
out=$(bash "$check" "$work/boards" "$work/baseline.json"); rc=$?
[[ $rc -eq 0 ]] && ok || no "at-baseline should pass (rc=$rc)"
[[ "$out" == *"no engine regressed"* ]] && ok || no "should say no regression"

# Below baseline → exit 1, and name the engine
board rust 50.0
out=$(bash "$check" "$work/boards" "$work/baseline.json"); rc=$?
[[ $rc -eq 1 ]] && ok || no "regression should exit 1 (rc=$rc)"
[[ "$out" == *"REGRESSION"* ]] && ok || no "should mark the regression"

# A known-broken engine that starts working is reported, not ignored.
board rust 100.0; board ts 100.0
out=$(bash "$check" "$work/boards" "$work/baseline.json"); rc=$?
[[ $rc -eq 0 ]] && ok || no "an improvement must not fail the build"
[[ "$out" == *"IMPROVED"* && "$out" == *"raise it"* ]] && ok || no "an improvement should prompt raising the baseline"

# The regression that matters most: a crashing engine writes no board.
# Absence must never read as success.
board rust 100.0; rm -f "$work/boards/board-ts.json"
out=$(bash "$check" "$work/boards" "$work/baseline.json"); rc=$?
[[ "$out" == *"NO BOARD"* ]] && ok || no "a missing board must be visible"
[[ "$out" == *"not as passing"* ]] && ok || no "a missing board must be called out explicitly"

# An all-INCONCLUSIVE board (no models) counts as 0, not as absent data.
printf '{"suite":"agentic","trials":1,"scenario_count":2,"models":[]}\n' >"$work/boards/board-ts.json"
board rust 0.0
out=$(bash "$check" "$work/boards" "$work/baseline.json"); rc=$?
[[ $rc -eq 1 ]] && ok || no "a crashing rust engine must regress (rc=$rc)"

# Bad inputs are errors, not silent passes.
bash "$check" "$work/nope" "$work/baseline.json" >/dev/null 2>&1 && no "missing dir should fail" || ok
bash "$check" "$work/boards" "$work/nope.json"  >/dev/null 2>&1 && no "missing baseline should fail" || ok

echo "engine-parity: $pass passed, $fail failed"
[[ "$fail" -eq 0 ]]
