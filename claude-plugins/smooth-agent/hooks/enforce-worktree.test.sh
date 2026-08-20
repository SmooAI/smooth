#!/usr/bin/env bash
# Self-check for enforce-worktree.sh.
#
# This hook is a PreToolUse guard, and PreToolUse has exactly one blocking exit
# code: 2. It shipped with `exit 1` on both deny paths, which Claude Code treats
# as a non-blocking hook error — the tool call proceeds. Nothing in the repo
# caught that, so the exit codes are what this test pins.
#
# Uses a throwaway repo + worktree in a tempdir, so results do not depend on the
# state of anyone's real checkout.
#
# Usage: bash claude-plugins/smooth-agent/hooks/enforce-worktree.test.sh

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK="$HERE/enforce-worktree.sh"
# `pwd -P` because on macOS mktemp hands back /var/... while git reports the
# canonical /private/var/... — the hook compares paths as strings.
TMP="$(cd "$(mktemp -d)" && pwd -P)"
trap 'rm -rf "$TMP"' EXIT

REPO="$TMP/myrepo"
WT="$TMP/myrepo-feature"
mkdir -p "$REPO/src" "$REPO/.claude"
git -C "$REPO" init -q -b main
git -C "$REPO" config user.email t@t.t
git -C "$REPO" config user.name t
echo "fn main() {}" > "$REPO/src/main.rs"
echo "# readme" > "$REPO/README.md"
git -C "$REPO" add -A
git -C "$REPO" commit -qm init
git -C "$REPO" worktree add -q "$WT" -b feature >/dev/null 2>&1

export CLAUDE_PROJECT_DIR="$REPO"

pass=0
fail=0
# check <name> <expected-exit> <hook-json>
check() {
    local name="$1" expected="$2" rc
    echo "$3" | bash "$HOOK" >/dev/null 2>&1
    rc=$?
    if [ "$expected" = "$rc" ]; then
        echo "  ok   $name (exit $rc)"
        pass=$((pass + 1))
    else
        echo "  FAIL $name — expected exit $expected, got $rc"
        fail=$((fail + 1))
    fi
}

echo "enforce-worktree:"

# --- Edit/Write on main: must BLOCK with 2, not 1. ---
check "Write to source on main blocks" 2 "{\"tool_name\":\"Write\",\"tool_input\":{\"file_path\":\"$REPO/src/main.rs\"}}"
check "Edit of source on main blocks" 2 "{\"tool_name\":\"Edit\",\"tool_input\":{\"file_path\":\"$REPO/README.md\"}}"

# --- Allow paths: a fail-shut hook is as broken as a fail-open one. ---
check "Write inside a feature worktree allowed" 0 "{\"tool_name\":\"Write\",\"tool_input\":{\"file_path\":\"$WT/src/main.rs\"}}"
check "Write to .claude/ allowed" 0 "{\"tool_name\":\"Write\",\"tool_input\":{\"file_path\":\"$REPO/.claude/settings.json\"}}"
check "Write outside the repo allowed" 0 '{"tool_name":"Write","tool_input":{"file_path":"/tmp/scratch.txt"}}'
check "Read is never blocked" 0 "{\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$REPO/src/main.rs\"}}"

# --- Bash: git commit. ---
check "git commit on main blocks" 2 '{"tool_name":"Bash","tool_input":{"command":"git commit -m x"}}'
check "git commit -C worktree allowed" 0 "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"git -C $WT commit -m x\"}}"
check "git merge --no-ff allowed" 0 '{"tool_name":"Bash","tool_input":{"command":"git merge feature --no-ff"}}'

# --- Bash: shell-spelled edits. The hole that let `sed -i` edit main freely. ---
check "sed -i on a tracked file blocks" 2 "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"sed -i '' s/a/b/ $REPO/src/main.rs\"}}"
check "redirect over a tracked file blocks" 2 "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"echo x > $REPO/README.md\"}}"
check "append to a tracked file blocks" 2 "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"echo x >> $REPO/README.md\"}}"
check "tee into a tracked file blocks" 2 "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"echo x | tee $REPO/src/main.rs\"}}"
check "rm of a tracked file blocks" 2 "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"rm $REPO/src/main.rs\"}}"
check "relative-path sed -i blocks" 2 '{"tool_name":"Bash","tool_input":{"command":"sed -i '' s/a/b/ src/main.rs"}}'

# --- Bash: things that only READ, or write elsewhere, must still run. ---
check "grep of a tracked file redirected to /tmp allowed" 0 "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"grep foo $REPO/src/main.rs > /tmp/out\"}}"
check "2>&1 redirect allowed" 0 '{"tool_name":"Bash","tool_input":{"command":"cargo build 2>&1 | tail -5"}}'
check "rm of an untracked dir allowed" 0 "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"rm -rf $REPO/target\"}}"
check "write to /tmp allowed" 0 '{"tool_name":"Bash","tool_input":{"command":"echo x > /tmp/x.txt"}}'
check "sed -i inside a feature worktree allowed" 0 "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"sed -i '' s/a/b/ $WT/src/main.rs\"}}"

# --- The bypass file still disables everything. ---
touch "$REPO/.claude/worktree-bypass"
check "bypass file allows an otherwise-blocked write" 0 "{\"tool_name\":\"Write\",\"tool_input\":{\"file_path\":\"$REPO/src/main.rs\"}}"
rm -f "$REPO/.claude/worktree-bypass"

echo
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
