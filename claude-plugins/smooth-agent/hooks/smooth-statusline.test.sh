#!/bin/bash
# Tests for smooth-statusline.sh + smooth-statusline-with-ponytail.sh.
# Run: bash smooth-statusline.test.sh
#
# A statusline runs on EVERY render, so the cases that matter are the silent
# ones: any error, stray newline, or hang here is a permanent artifact on the
# user's screen. Every "nothing to say" path is asserted to print exactly "".
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LINE="$HERE/smooth-statusline.sh"
WRAPPER="$HERE/smooth-statusline-with-ponytail.sh"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

STATE="$TMP/agent-sessions"
mkdir -p "$STATE"
printf 'fix-auth' >"$STATE/sess-1"
export SMOOTH_AGENT_SESSIONS_DIR="$STATE"

# A `th` stub, so the unread count is deterministic instead of depending on this
# machine's real mailbox. Shadows the real one by being first on PATH.
mkdir -p "$TMP/bin"
cat >"$TMP/bin/th" <<'EOF'
#!/bin/bash
# th msg unread-count --agent <handle>
[[ "$1 $2" == "msg unread-count" ]] || exit 1
echo "${TH_FAKE_UNREAD:-0}"
EOF
chmod +x "$TMP/bin/th"
export PATH="$TMP/bin:$PATH"

pass=0
fail=0
# `$(...)` strips trailing newlines, which is exactly what we must NOT do here —
# a stray newline breaks the statusline and would be invisible to the test.
# Capture raw bytes instead and compare those.
run() { printf '%s' "$1" | bash "$2" >"$TMP/out" 2>/dev/null; cat "$TMP/out"; }

check() { # description, expected(literal, plain text after stripping ANSI), payload, script
    local desc=$1 want=$2 payload=$3 script=$4 raw got
    raw=$(run "$payload" "$script")
    got=$(printf '%s' "$raw" | sed $'s/\033\\[[0-9;]*m//g')
    if [[ "$got" == "$want" ]]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        printf 'FAIL  %s\n      want %q\n      got  %q\n' "$desc" "$want" "$got"
    fi
}

payload() { jq -cn --arg s "$1" '{session_id:$s, cwd:"/tmp", model:{display_name:"x"}}'; }

# --- the happy path --------------------------------------------------------
check 'handle renders'            '⚙ th:fix-auth'    "$(payload sess-1)" "$LINE"
TH_FAKE_UNREAD=3 check 'unread count appended' '⚙ th:fix-auth ✉3' "$(payload sess-1)" "$LINE"
TH_FAKE_UNREAD=0 check 'zero unread is not shown' '⚙ th:fix-auth' "$(payload sess-1)" "$LINE"

# No trailing newline — Claude Code renders the raw bytes.
raw=$(run "$(payload sess-1)" "$LINE"; printf X)
if [[ "$raw" != *$'\n'X ]]; then pass=$((pass + 1)); else
    fail=$((fail + 1)); echo 'FAIL  statusline must not emit a trailing newline'
fi

# --- every silent path -----------------------------------------------------
check 'unknown session id'        '' "$(payload sess-missing)" "$LINE"
check 'payload without session_id' '' '{"cwd":"/tmp"}'         "$LINE"
check 'empty stdin'               '' ''                        "$LINE"
check 'malformed json'            '' 'not json at all'         "$LINE"

# An empty/whitespace state file is not a handle.
printf '  \n' >"$STATE/sess-blank"
check 'blank state file'          '' "$(payload sess-blank)"   "$LINE"

# A handle written with a trailing newline (any `echo >` would) must not drag it
# into the rendered line.
printf 'nl-handle\n' >"$STATE/sess-nl"
check 'trailing newline in state file is stripped' '⚙ th:nl-handle' "$(payload sess-nl)" "$LINE"

# th absent → still render the handle, just no count.
out=$(printf '%s' "$(payload sess-1)" | PATH="$(dirname "$(command -v jq)"):/bin" bash "$LINE" 2>/dev/null | sed $'s/\033\\[[0-9;]*m//g')
if [[ "$out" == '⚙ th:fix-auth' ]]; then pass=$((pass + 1)); else
    fail=$((fail + 1)); printf 'FAIL  no th on PATH should still show the handle, got %q\n' "$out"
fi

# th failing (stale binary, locked db, garbage output) → handle only, no count.
cat >"$TMP/bin/th" <<'EOF'
#!/bin/bash
echo "error: something went wrong" >&2
exit 1
EOF
chmod +x "$TMP/bin/th"
check 'failing th degrades to handle only' '⚙ th:fix-auth' "$(payload sess-1)" "$LINE"
cat >"$TMP/bin/th" <<'EOF'
#!/bin/bash
echo "not a number"
EOF
chmod +x "$TMP/bin/th"
check 'non-numeric count is ignored' '⚙ th:fix-auth' "$(payload sess-1)" "$LINE"

# --- the ponytail wrapper --------------------------------------------------
# With no ponytail installed the wrapper is just our segment, with no stray
# leading space.
HOME_BAK="$HOME"
export HOME="$TMP/home"
mkdir -p "$HOME"
check 'wrapper alone == our segment' '⚙ th:fix-auth' "$(payload sess-1)" "$WRAPPER"

PONY="$HOME/.claude/plugins/cache/ponytail/ponytail/1.0.0/hooks"
mkdir -p "$PONY"
printf '#!/bin/bash\ncat >/dev/null\nprintf "[PONYTAIL]"\n' >"$PONY/ponytail-statusline.sh"
check 'wrapper joins both, in order' '[PONYTAIL] ⚙ th:fix-auth' "$(payload sess-1)" "$WRAPPER"

# Newest version wins — the cache keeps every version ever installed, and a plain
# glob would pick by name.
PONY2="$HOME/.claude/plugins/cache/ponytail/ponytail/0.9.0/hooks"
mkdir -p "$PONY2"
printf '#!/bin/bash\ncat >/dev/null\nprintf "[OLD]"\n' >"$PONY2/ponytail-statusline.sh"
touch "$PONY2/ponytail-statusline.sh"   # older name, newer mtime
check 'newest ponytail wins over name order' '[OLD] ⚙ th:fix-auth' "$(payload sess-1)" "$WRAPPER"

# A ponytail that blows up must not take our segment with it.
printf '#!/bin/bash\nexit 7\n' >"$PONY2/ponytail-statusline.sh"
touch "$PONY2/ponytail-statusline.sh"
check 'broken ponytail degrades to our segment' '⚙ th:fix-auth' "$(payload sess-1)" "$WRAPPER"
export HOME="$HOME_BAK"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ $fail == 0 ]]
