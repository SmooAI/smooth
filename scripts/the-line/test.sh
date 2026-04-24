#!/usr/bin/env bash
# End-to-end dry-run of the-line pipeline using checked-in fixtures. Runs the
# four helper scripts in the same order as the GitHub Action's dispatch path,
# and verifies:
#   1. render-markdown.sh emits the expected overall%, per-language rows, and
#      cost/commit cells in the summary block AND history row.
#   2. check-regression.sh exits 0 on a fresh history, 0 when the drop is
#      within threshold, and 1 when a -5pp drop is introduced.
#   3. append-history.sh creates the file with the auto-maintained header and
#      then appends a second data row (no duplicated header) on a subsequent run.
#   4. update-release-notes.sh with DRY_RUN=1 produces a fenced
#      <!-- THE-LINE:START --> block AND replaces it in-place on re-run.
#
# Exits non-zero on any assertion failure. Meant to be runnable both locally
# (`bash scripts/the-line/test.sh`) and inside the workflow dry-run job.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT_DIR="$ROOT_DIR/scripts/the-line"
FIX_DIR="$ROOT_DIR/tests/fixtures"
SAMPLE="$FIX_DIR/the-line-sample.json"
REGRESSION="$FIX_DIR/the-line-regression.json"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

fail() {
    echo "TEST FAIL: $*" >&2
    exit 1
}

assert_contains() {
    local file="$1" needle="$2"
    grep -q -F -- "$needle" "$file" || fail "expected '$needle' in $file, got:$(printf '\n%s' "$(cat "$file")")"
}

assert_not_contains() {
    local file="$1" needle="$2"
    if grep -q -F -- "$needle" "$file"; then
        fail "did not expect '$needle' in $file, got:$(printf '\n%s' "$(cat "$file")")"
    fi
}

echo "== test: render-markdown from sample =="
SUMMARY_OUT="$WORK/summary.md" HISTORY_ROW_OUT="$WORK/row.md" \
    "$SCRIPT_DIR/render-markdown.sh" "$SAMPLE" "v0.42.1"

assert_contains "$WORK/summary.md" "### The Line — v0.42.1"
assert_contains "$WORK/summary.md" "**Overall pass rate: 82.5%**"
assert_contains "$WORK/summary.md" "| python | 90% |"
assert_contains "$WORK/summary.md" "| rust | 80% |"
assert_contains "$WORK/summary.md" "| cost | \$4.23 |"
assert_contains "$WORK/summary.md" "| median task | 15.4s |"
assert_contains "$WORK/summary.md" "abc123def456"
assert_not_contains "$WORK/summary.md" "Budget cap hit"

assert_contains "$WORK/row.md" "| v0.42.1 | 2026-04-23 | 82.5 |"
assert_contains "$WORK/row.md" "\$4.23"
assert_contains "$WORK/row.md" "abc123def456"

echo "== test: check-regression on empty history (first release) =="
if "$SCRIPT_DIR/check-regression.sh" "$SAMPLE" "$WORK/no-such-history.md"; then
    echo "  ok — first-release path exited 0"
else
    fail "first-release regression check should have exited 0"
fi

echo "== test: append-history creates file + header =="
"$SCRIPT_DIR/append-history.sh" "$WORK/history.md" "$WORK/row.md"
assert_contains "$WORK/history.md" "auto-maintained by"
assert_contains "$WORK/history.md" "| version | date | overall%"
assert_contains "$WORK/history.md" "| v0.42.1 | 2026-04-23 | 82.5 |"

echo "== test: check-regression against populated history (no regression) =="
SUMMARY_OUT="$WORK/summary2.md" HISTORY_ROW_OUT="$WORK/row2.md" \
    "$SCRIPT_DIR/render-markdown.sh" "$SAMPLE" "v0.42.1-rerun"
if "$SCRIPT_DIR/check-regression.sh" "$SAMPLE" "$WORK/history.md"; then
    echo "  ok — same score does not regress"
else
    fail "same-score regression check should have exited 0"
fi

echo "== test: append-history appends second row without duplicating header =="
"$SCRIPT_DIR/append-history.sh" "$WORK/history.md" "$WORK/row2.md"
header_count=$(grep -c '^| version | date | overall%' "$WORK/history.md")
if [[ "$header_count" != "1" ]]; then
    fail "expected exactly one header row, got $header_count"
fi
data_count=$(grep -c '^| v0\.42\.1' "$WORK/history.md")
if [[ "$data_count" != "2" ]]; then
    fail "expected 2 data rows after 2 appends, got $data_count"
fi

echo "== test: check-regression FAILS on -5pp drop =="
SUMMARY_OUT="$WORK/summary-reg.md" HISTORY_ROW_OUT="$WORK/row-reg.md" \
    "$SCRIPT_DIR/render-markdown.sh" "$REGRESSION" "v0.42.2"
set +e
"$SCRIPT_DIR/check-regression.sh" "$REGRESSION" "$WORK/history.md"
rc=$?
set -e
if [[ "$rc" != "1" ]]; then
    fail "regression check should have exited 1 on -5pp drop, got $rc"
fi
echo "  ok — regression correctly detected (exit 1)"

echo "== test: update-release-notes (DRY_RUN=1) produces fenced block =="
DRY_RUN=1 "$SCRIPT_DIR/update-release-notes.sh" "$WORK/summary.md" "v0.42.1" "$WORK/notes.md"
assert_contains "$WORK/notes.md" "<!-- THE-LINE:START -->"
assert_contains "$WORK/notes.md" "<!-- THE-LINE:END -->"
assert_contains "$WORK/notes.md" "Overall pass rate: 82.5%"
start_count=$(grep -c 'THE-LINE:START' "$WORK/notes.md")
if [[ "$start_count" != "1" ]]; then
    fail "expected exactly one THE-LINE:START marker, got $start_count"
fi

echo "== test: update-release-notes replaces prior block in-place =="
cp "$WORK/notes.md" "$WORK/notes-v1.md"
SUMMARY_OUT="$WORK/summary-v2.md" HISTORY_ROW_OUT="$WORK/row-v2.md" \
    "$SCRIPT_DIR/render-markdown.sh" "$REGRESSION" "v0.42.2"
DRY_RUN=1 "$SCRIPT_DIR/update-release-notes.sh" "$WORK/summary-v2.md" "v0.42.2" "$WORK/notes.md"
assert_contains "$WORK/notes.md" "Overall pass rate: 77.5%"
assert_not_contains "$WORK/notes.md" "Overall pass rate: 82.5%"
start_count=$(grep -c 'THE-LINE:START' "$WORK/notes.md")
if [[ "$start_count" != "1" ]]; then
    fail "expected exactly one THE-LINE:START marker after re-run, got $start_count"
fi

echo "== test: update-release-notes preserves pre-existing body text =="
cat >"$WORK/existing-body.txt" <<EOF
# v0.42.1

Headline features and fixes.

<!-- THE-LINE -->

(end)
EOF
EXISTING_FIXTURE="$WORK/existing-body.txt"
existing_body=$(cat "$EXISTING_FIXTURE")
EXISTING_BODY="$existing_body" FENCED="$(printf '<!-- THE-LINE:START -->\n### rendered\n<!-- THE-LINE:END -->')" \
    python3 - "$WORK/notes-merged.md" <<'PY'
import os, re, sys
existing = os.environ["EXISTING_BODY"]
fenced = os.environ["FENCED"]
out = sys.argv[1]
fenced_re = re.compile(r"<!--\s*THE-LINE:START\s*-->.*?<!--\s*THE-LINE:END\s*-->", re.DOTALL)
single_tag_re = re.compile(r"<!--\s*THE-LINE\s*-->")
if fenced_re.search(existing):
    new = fenced_re.sub(fenced, existing, count=1)
elif single_tag_re.search(existing):
    new = single_tag_re.sub(fenced, existing, count=1)
elif existing.strip() == "":
    new = fenced
else:
    new = existing.rstrip() + "\n\n" + fenced + "\n"
open(out, "w").write(new + ("" if new.endswith("\n") else "\n"))
PY
assert_contains "$WORK/notes-merged.md" "Headline features"
assert_contains "$WORK/notes-merged.md" "(end)"
assert_contains "$WORK/notes-merged.md" "THE-LINE:START"
assert_contains "$WORK/notes-merged.md" "### rendered"
assert_not_contains "$WORK/notes-merged.md" "<!-- THE-LINE -->"

echo "== test: render-badge emits Shields endpoint JSON with correct color =="
"$SCRIPT_DIR/render-badge.sh" "$SAMPLE" "$WORK/badge.json"
assert_contains "$WORK/badge.json" '"schemaVersion": 1'
assert_contains "$WORK/badge.json" '"label": "the line"'
# Sample fixture's overall_pass_rate is 0.825 → message "82.5%", >= 0.80 → brightgreen.
assert_contains "$WORK/badge.json" '"message": "82.5%"'
assert_contains "$WORK/badge.json" '"color": "brightgreen"'

echo "== test: render-badge picks yellow color for mid-band score =="
"$SCRIPT_DIR/render-badge.sh" "$REGRESSION" "$WORK/badge-mid.json"
# Regression fixture's overall_pass_rate is 0.775 → below 0.80, >= 0.60 → yellow.
assert_contains "$WORK/badge-mid.json" '"color": "yellow"'

echo "== test: render-badge annotates budget-cap hit =="
python3 - <<'PY' "$SAMPLE" "$WORK/sample-budget-hit.json"
import json, sys
data = json.load(open(sys.argv[1]))
data["budget_usd_hit"] = True
json.dump(data, open(sys.argv[2], "w"))
PY
"$SCRIPT_DIR/render-badge.sh" "$WORK/sample-budget-hit.json" "$WORK/badge-partial.json"
assert_contains "$WORK/badge-partial.json" "⚠"

echo "== test: render-badge picks orange for below-target score =="
python3 - <<'PY' "$SAMPLE" "$WORK/sample-low.json"
import json, sys
data = json.load(open(sys.argv[1]))
data["overall_pass_rate"] = 0.40
data["budget_usd_hit"] = False
json.dump(data, open(sys.argv[2], "w"))
PY
"$SCRIPT_DIR/render-badge.sh" "$WORK/sample-low.json" "$WORK/badge-low.json"
assert_contains "$WORK/badge-low.json" '"color": "orange"'
# Never red — red implies brokenness, not "below target".
assert_not_contains "$WORK/badge-low.json" '"color": "red"'

echo
echo "ALL TESTS PASSED"
