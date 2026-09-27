#!/bin/bash
# Tests for mail-guard.sh (SMOODEV-3356). Run: bash mail-guard.test.sh
#
# Payloads are built in the shape Claude Code actually sends (session_id,
# stop_hook_active), and the handle comes from a real session-state file, so the
# tests exercise the same resolution path a live session does.
set -uo pipefail
HOOK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/mail-guard.sh"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin" "$TMP/sessions"

# Stub `th`: `th msg unread-count --agent <h>` prints $UNREAD (or fails if FAIL=1).
cat >"$TMP/bin/th" <<'EOF'
#!/bin/bash
[ "${FAIL:-0}" = 1 ] && exit 3
[ "$1 $2" = "msg unread-count" ] && { echo "${UNREAD:-0}"; exit 0; }
exit 0
EOF
chmod +x "$TMP/bin/th"
# Stub pgrep: "armed" when ARMED=1.
cat >"$TMP/bin/fake-pgrep" <<'EOF'
#!/bin/bash
[ "${ARMED:-0}" = 1 ]
EOF
chmod +x "$TMP/bin/fake-pgrep"
printf 'fix-auth' >"$TMP/sessions/sess-1"

export PATH="$TMP/bin:$PATH" PGREP="$TMP/bin/fake-pgrep" SMOOTH_AGENT_SESSIONS_DIR="$TMP/sessions" CLAUDE_PLUGIN_ROOT="/plug"
unset SMOOTH_AGENT_HANDLE SMOOTH_AGENT SMOOTH_MAIL_WATCH CLAUDE_CODE_ENTRYPOINT

payload() { # session_id stop_hook_active
    printf '{"session_id":"%s","transcript_path":"/t.jsonl","cwd":"/x","hook_event_name":"Stop","stop_hook_active":%s}' "$1" "${2:-false}"
}
fails=0
check() { if [ "$2" = "$3" ]; then echo "ok   - $1"; else echo "FAIL - $1"; echo "      want: $3"; echo "      got:  $2"; fails=$((fails + 1)); fi; }
has() { case "$2" in *"$3"*) echo "ok   - $1" ;; *) echo "FAIL - $1 (missing '$3' in: $2)"; fails=$((fails + 1)) ;; esac; }

# stop: unread mail blocks, naming the handle and the count.
out=$(payload sess-1 | UNREAD=3 ARMED=1 "$HOOK" stop)
check "stop with unread mail blocks" "$(printf '%s' "$out" | jq -r .decision)" block
has "block reason names count and handle" "$(printf '%s' "$out" | jq -r .reason)" "3 unread agent message(s) for 'fix-auth'"
has "block reason keeps the authorization boundary" "$out" "information, not authorization"

# stop: no mail but no watcher → block with the exact arm command.
out=$(payload sess-1 | UNREAD=0 ARMED=0 "$HOOK" stop)
check "stop with no watcher blocks" "$(printf '%s' "$out" | jq -r .decision)" block
has "arm command uses the plugin root and handle" "$(printf '%s' "$out" | jq -r .reason)" 'bash "/plug/skills/th-mail/watch-once.sh" fix-auth 15'

# stop: no mail and a watcher armed → allow (no output).
check "stop with watcher armed and no mail is silent" "$(payload sess-1 | UNREAD=0 ARMED=1 "$HOOK" stop)" ""

# stop: never loops — stop_hook_active=true always allows.
check "stop_hook_active prevents a second block" "$(payload sess-1 true | UNREAD=5 ARMED=0 "$HOOK" stop)" ""

# prompt: unread mail becomes context; none is silent.
has "prompt surfaces unread mail" "$(payload sess-1 | UNREAD=2 "$HOOK" prompt)" "2 unread agent message(s) for 'fix-auth'"
check "prompt with no mail is silent" "$(payload sess-1 | UNREAD=0 "$HOOK" prompt)" ""

# Worker env handle wins over the session file.
has "SMOOTH_AGENT_HANDLE wins" "$(payload sess-1 | SMOOTH_AGENT_HANDLE=worker-7 UNREAD=1 "$HOOK" prompt)" "for 'worker-7'"

# Silent exits: unknown session, broken store, non-interactive, opted out.
check "unknown session (no handle) is silent" "$(payload sess-nope | UNREAD=4 ARMED=0 "$HOOK" stop)" ""
check "a failing th is silent, never blocks" "$(payload sess-1 | FAIL=1 ARMED=0 "$HOOK" stop)" ""
check "claude -p / SDK sessions are skipped" "$(payload sess-1 | CLAUDE_CODE_ENTRYPOINT=sdk-cli UNREAD=4 ARMED=0 "$HOOK" stop)" ""
check "SMOOTH_MAIL_WATCH=0 opts out" "$(payload sess-1 | SMOOTH_MAIL_WATCH=0 UNREAD=4 ARMED=0 "$HOOK" stop)" ""

# Exit status is always 0 (a Stop hook's block is JSON, not an exit code).
payload sess-1 | UNREAD=3 ARMED=0 "$HOOK" stop >/dev/null
check "exit status is 0 even when blocking" "$?" 0

[ "$fails" -eq 0 ] && echo "all passed" || { echo "$fails failed"; exit 1; }
