#!/bin/bash
# Tests for attest-push-hint.sh. Run: bash attest-push-hint.test.sh
#
# This hook has shipped broken twice, both times because it was "obviously fine":
# once resolving the repo from `.cwd` (the SESSION dir) so `cd <repo> && git push`
# was silently missed, and once with a `(-[^ ]+\s+)*push` regex whose flag value ate
# the `push` token in `git -C <path> push`.
#
# The first of those had a passing test suite. It passed because the payloads were
# hand-written with the cwd the fix expected, instead of the shape Claude Code
# actually emits — the tests agreed with the bug. So: every payload here is built by
# `payload()` in the real wire shape, and the cases that matter set `cwd` to a
# DIFFERENT repo than the one being pushed, which is the situation that broke it.

set -uo pipefail
HOOK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/attest-push-hint.sh"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# A repo that opts into attestation, one that doesn't, and one whose scripts/ci/
# holds nothing runnable. The hook is repo-agnostic: it fires wherever the
# convention exists — an executable scripts/ci/<name>.sh — and suggests
# `th attest`, which is generic. There is no per-repo runner script to look for.
WITH="$TMP/with-attest"
WITHOUT="$TMP/without-attest"
EMPTY="$TMP/empty-ci"
for r in "$WITH" "$WITHOUT" "$EMPTY"; do
    mkdir -p "$r" && git -C "$r" init -q 2>/dev/null
done
mkdir -p "$WITH/scripts/ci" "$EMPTY/scripts/ci"
for f in rust lint _env attest attest.test; do
    printf '#!/bin/bash\n' >"$WITH/scripts/ci/$f.sh"
    chmod +x "$WITH/scripts/ci/$f.sh"
done
# Not executable → not a check.
printf '#!/bin/bash\n' >"$WITH/scripts/ci/draft.sh"
# A scripts/ci/ that exists but holds only helpers and suites.
printf '#!/bin/bash\n' >"$EMPTY/scripts/ci/_env.sh"
chmod +x "$EMPTY/scripts/ci/_env.sh"

# The hook exits 0 when `th` isn't installed (a block whose fix can't be run is
# worse than no block), so these tests need one on PATH. A stub is enough — the
# hook only ever probes for it.
mkdir -p "$TMP/bin"
printf '#!/bin/bash\nexit 0\n' >"$TMP/bin/th"
chmod +x "$TMP/bin/th"
export PATH="$TMP/bin:$PATH"

# ELSEWHERE is the session cwd for the `cd`/`-C` cases: a real repo that is NOT the
# push target, so a hook resolving from .cwd alone would exit 0 and fail the test.
ELSEWHERE="$WITHOUT"

payload() { # command, cwd
    jq -cn --arg c "$1" --arg d "$2" \
        '{tool_name:"Bash", tool_input:{command:$c}, cwd:$d}'
}

pass=0
fail=0
check() { # description, expected_exit, command, cwd
    local desc=$1 want=$2 cmd=$3 cwd=$4 got
    payload "$cmd" "$cwd" | bash "$HOOK" >/dev/null 2>&1
    got=$?
    if [[ "$got" == "$want" ]]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        printf 'FAIL  %s\n      want exit %s, got %s\n      cmd: %s\n      cwd: %s\n' \
            "$desc" "$want" "$got" "$cmd" "$cwd"
    fi
}

# --- blocks (exit 2) -------------------------------------------------------
check 'bare git push in an attesting repo'   2 'git push'                              "$WITH"
check 'git push with args'                   2 'git push -u origin my-branch'          "$WITH"
check 'cd <repo> && git push from elsewhere' 2 "cd $WITH && git push"                  "$ELSEWHERE"
check 'git -C <repo> push from elsewhere'    2 "git -C $WITH push"                     "$ELSEWHERE"
check 'quoted cd path'                       2 "cd \"$WITH\" && git push"              "$ELSEWHERE"
check 'push at the end of a longer chain'    2 "cd $WITH && git add -A && git push"    "$ELSEWHERE"
check 'last cd wins'                         2 "cd $WITHOUT; cd $WITH && git push"     "$ELSEWHERE"

# --- allows (exit 0) -------------------------------------------------------
check 'repo with no scripts/ci at all'       0 'git push'                              "$WITHOUT"
check 'scripts/ci with no runnable checks'   0 'git push'                              "$EMPTY"
check 'no push at all'                       0 'git status'                            "$WITH"
check 'th pearls push is a Dolt sync'        0 'th pearls push'                        "$WITH"
check 'dry run publishes nothing'            0 'git push --dry-run'                    "$WITH"
check 'branch deletion'                      0 'git push --delete origin old-branch'   "$WITH"
check 'tags only'                            0 'git push origin --tags'                "$WITH"
check 'explicit ack'                         0 'git push # attest:ack reason=docs'     "$WITH"
check 'ack on a cd-prefixed push'            0 "cd $WITH && git push # attest:ack r=x"  "$ELSEWHERE"

# Non-Bash tools are ignored. This one is built by hand because `check` always emits
# tool_name:"Bash" — a `check` line here would silently test nothing.
echo '{"tool_name":"Read","tool_input":{"file_path":"/tmp/x"},"cwd":"/tmp"}' | bash "$HOOK" >/dev/null 2>&1
if [[ $? == 0 ]]; then pass=$((pass + 1)); else
    fail=$((fail + 1)); echo 'FAIL  Read tool should exit 0'
fi

# `th` missing → allow. Blocking on a fix the machine can't run is worse than
# letting the push through.
#
# The stripped PATH must still carry jq and git, or the hook exits 0 for the
# wrong reason (empty stdin / no repo) and this test proves nothing. Assert that
# rather than trusting it — this file exists because a suite once agreed with a bug.
BARE_PATH=$(dirname "$(command -v jq)"):$(dirname "$(command -v git)"):/bin
if PATH="$BARE_PATH" command -v jq >/dev/null && PATH="$BARE_PATH" command -v git >/dev/null && ! PATH="$BARE_PATH" command -v th >/dev/null; then
    payload 'git push' "$WITH" | PATH="$BARE_PATH" bash "$HOOK" >/dev/null 2>&1
    got=$?
    if [[ $got == 0 ]]; then pass=$((pass + 1)); else
        fail=$((fail + 1)); echo "FAIL  no th on PATH should exit 0, got $got"
    fi
else
    fail=$((fail + 1)); echo "FAIL  could not build a jq+git-but-no-th PATH — the no-th case did not run"
fi

# The stderr hint has to name the checks it found, and name them the way
# `th attest` will accept them, or the agent cannot act on it.
out=$(payload 'git push' "$WITH" | bash "$HOOK" 2>&1)
assert_hint() { # description, pattern, expect(present|absent)
    if grep -qE "$2" <<<"$out"; then found=yes; else found=no; fi
    if [[ ("$3" == present && "$found" == yes) || ("$3" == absent && "$found" == no) ]]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1)); printf 'FAIL  %s\n' "$1"
    fi
}
assert_hint 'hint lists discovered checks'          'available: .*rust'   present
assert_hint 'hint names th attest, not the bash runner' '`th attest'      present
assert_hint 'no stale scripts/ci/attest.sh advice'  'bash scripts/ci/attest\.sh' absent
# These three are the discovery rule. `attest.test` in particular is a real trap:
# a naive `ls *.sh | sed s/.sh//` offers it as a check named "attest.test".
assert_hint 'helpers (_env) are not checks'         'available:.*_env'    absent
assert_hint 'test suites are not checks'            'available:.*attest\.test' absent
assert_hint 'non-executable scripts are not checks' 'available:.*draft'   absent

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ $fail == 0 ]]
