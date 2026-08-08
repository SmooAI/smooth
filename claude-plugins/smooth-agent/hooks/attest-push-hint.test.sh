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

# A repo that opts into attestation, and one that doesn't. The hook is
# repo-agnostic: it fires only where scripts/ci/attest.sh exists and is executable.
WITH="$TMP/with-attest"
WITHOUT="$TMP/without-attest"
for r in "$WITH" "$WITHOUT"; do
    mkdir -p "$r" && git -C "$r" init -q 2>/dev/null
done
mkdir -p "$WITH/scripts/ci"
printf '#!/bin/bash\n' >"$WITH/scripts/ci/attest.sh"
printf '#!/bin/bash\n' >"$WITH/scripts/ci/rust.sh"
chmod +x "$WITH/scripts/ci/attest.sh"

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
check 'repo without scripts/ci/attest.sh'    0 'git push'                              "$WITHOUT"
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

# The stderr hint has to name the checks it found, or the agent cannot act on it.
out=$(payload 'git push' "$WITH" | bash "$HOOK" 2>&1)
if grep -q 'available: .*rust' <<<"$out"; then pass=$((pass + 1)); else
    fail=$((fail + 1)); echo 'FAIL  hint should list discovered checks (rust)'
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ $fail == 0 ]]
