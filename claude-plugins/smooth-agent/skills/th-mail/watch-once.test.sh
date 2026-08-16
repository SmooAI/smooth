#!/bin/bash
# Tests for watch-once.sh. Run: bash watch-once.test.sh
#
# The one case that matters is pearl th-ad0701: on a 100%-full disk `th msg
# watch` failed, and this wrapper reported "[]" and exited 0 — so a mail read
# that FAILED was indistinguishable from one that succeeded and found nothing.
# Every case below is really the same assertion: a failure must not wear the
# no-mail costume.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/watch-once.sh"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin"
export PATH="$TMP/bin:$PATH"

pass=0
fail=0

# Install a `th` stub. $1 = body of the stub.
stub_th() { printf '#!/bin/bash\n%s\n' "$1" >"$TMP/bin/th"; chmod +x "$TMP/bin/th"; }

check() { # description, want-exit, want-stdout, [args…]
    local desc=$1 want_status=$2 want_out=$3; shift 3
    local out status
    out=$(bash "$SCRIPT" "$@" 2>"$TMP/err"); status=$?
    if [[ "$status" == "$want_status" && "$out" == "$want_out" ]]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        printf 'FAIL  %s\n      want exit %s stdout %q\n      got  exit %s stdout %q (stderr: %s)\n' \
            "$desc" "$want_status" "$want_out" "$status" "$out" "$(cat "$TMP/err")"
    fi
}

# --- mail arrives ----------------------------------------------------------
stub_th 'echo "[{\"id\":\"msg-1\"}]"'
check 'mail is passed through, exit 0' 0 '[{"id":"msg-1"}]' agent 1 5

# --- the lifetime cap ------------------------------------------------------
# `th msg watch --once` blocks until mail arrives; the reaper kills it. A signal
# death (128+n) is a genuine timeout: "[]", exit 0.
stub_th 'sleep 30'
check 'timeout reports no mail, exit 0' 0 '[]' agent 1 1

# --- th itself fails (the th-ad0701 regression) ----------------------------
# Without the fix this printed "[]" and exited 0 — a broken store reported as an
# empty inbox.
stub_th 'echo "Error: apply mail schema" >&2; exit 1'
check 'store failure propagates, no "[]"' 1 '' agent 1 5

stub_th 'exit 101'
check 'any non-zero exit is preserved' 101 '' agent 1 5

# `th` missing from PATH (127) is a failure, not an empty inbox. Stubbed rather
# than deleted: removing our stub just uncovers the machine's real `th`.
stub_th 'exit 127'
check 'missing th is a failure' 127 '' agent 1 5

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ $fail == 0 ]]
