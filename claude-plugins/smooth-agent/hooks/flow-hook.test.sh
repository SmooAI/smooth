#!/usr/bin/env bash
# Self-check for the SmoothFlow hooks (pearl th-9483e8 / th-1b8e05):
#   flow-hook.sh, precompact-checkpoint.sh, handoff-context.sh.
#
# The contract these pin: every script exits 0 and prints nothing when the
# daemon / th is unreachable (a hook that fails or blocks the harness is worse
# than no hook), fire-and-forget events reach the daemon with the right
# envelope, PermissionRequest passes the daemon's decision through verbatim
# (and only a real decision), PreCompact checkpoints the matching pearls, and
# SessionStart(compact|resume) wraps the packet as additionalContext.
#
# A tiny python http.server stands in for the daemon; a bash stub stands in
# for `th` (records argv, prints canned packets). Nothing touches a real store.
#
# Usage: bash claude-plugins/smooth-agent/hooks/flow-hook.test.sh

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"; [ -n "${SERVER_PID:-}" ] && kill "$SERVER_PID" 2>/dev/null' EXIT

pass=0
fail=0
ok() { echo "  ok   $1"; pass=$((pass + 1)); }
bad() { echo "  FAIL $1"; fail=$((fail + 1)); }
# expect <name> <expected-exit> <actual-exit> <stdout> (stdout must be empty unless a 5th arg gives the expected)
expect() {
    local name="$1" want_rc="$2" rc="$3" out="$4" want_out="${5-}"
    if [ "$want_rc" = "$rc" ] && [ "$out" = "$want_out" ]; then ok "$name (exit $rc)"; else bad "$name — exit $rc (want $want_rc), stdout: '$out' (want '$want_out')"; fi
}

for dep in python3 curl jq; do
    if ! command -v "$dep" >/dev/null 2>&1; then
        echo "flow hooks: skipped ($dep not installed)"
        exit 0
    fi
done

# ── mock daemon: records every POST to $TMP/requests.jsonl, replies per path ──
cat >"$TMP/server.py" <<'PY'
import json, os, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer
LOG = os.environ["LOG"]
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(n)
        with open(LOG, "a") as f:
            f.write(json.dumps({"path": self.path, "body": json.loads(body)}) + "\n")
        ev = json.loads(body).get("event")
        if self.path != "/api/flow/hooks":
            self.send_response(404); self.end_headers(); return
        if ev == "PermissionRequest":
            mode = open(os.environ["MODE"]).read().strip()
            if mode == "slow":
                time.sleep(5)
            if mode == "decide":
                out = b'{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'
            elif mode == "empty":
                out = b'{}'
            elif mode == "error":
                self.send_response(500); self.end_headers(); return
            else:
                out = b'{}'
        else:
            out = b'{}'
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.end_headers(); self.wfile.write(out)
srv = HTTPServer(("127.0.0.1", 0), H)
open(os.environ["ADDR"], "w").write("127.0.0.1:%d" % srv.server_address[1])
srv.serve_forever()
PY
export LOG="$TMP/requests.jsonl" MODE="$TMP/mode" ADDR="$TMP/daemon.addr"
echo decide >"$MODE"
python3 "$TMP/server.py" &
SERVER_PID=$!
for _ in $(seq 1 50); do [ -s "$ADDR" ] && break; sleep 0.1; done
[ -s "$ADDR" ] || { echo "mock daemon did not start"; exit 1; }
export SMOOTH_DAEMON_ADDR_FILE="$ADDR"
# Wait for a fire-and-forget request to land (they are detached).
wait_log() { for _ in $(seq 1 50); do [ "$(wc -l <"$LOG" 2>/dev/null | tr -d ' ')" -ge "$1" ] && return 0; sleep 0.1; done; return 1; }

HOOK="$HERE/flow-hook.sh"
PAYLOAD='{"session_id":"sid-123","cwd":"/some/where","hook_event_name":"Stop","stop_hook_active":false}'

echo "flow-hook.sh:"

# --- unreachable / misconfigured → exit 0, silent -------------------------------
out=$(echo "$PAYLOAD" | SMOOTH_DAEMON_ADDR_FILE="$TMP/nope" SMOOTH_FLOW_ADDR_FILE="$TMP/nope" bash "$HOOK" Stop 2>&1); expect "no daemon.addr file → silent exit 0" 0 $? "$out"
: >"$TMP/empty.addr"
out=$(echo "$PAYLOAD" | SMOOTH_DAEMON_ADDR_FILE="$TMP/empty.addr" SMOOTH_FLOW_ADDR_FILE="$TMP/nope" bash "$HOOK" Stop 2>&1); expect "empty daemon.addr → silent exit 0" 0 $? "$out"
echo "127.0.0.1:1" >"$TMP/dead.addr"
out=$(echo "$PAYLOAD" | SMOOTH_DAEMON_ADDR_FILE="$TMP/dead.addr" SMOOTH_FLOW_ADDR_FILE="$TMP/nope" bash "$HOOK" Stop 2>&1); expect "daemon down (fire-and-forget) → silent exit 0" 0 $? "$out"
out=$(echo "$PAYLOAD" | SMOOTH_DAEMON_ADDR_FILE="$TMP/dead.addr" SMOOTH_FLOW_ADDR_FILE="$TMP/nope" bash "$HOOK" PermissionRequest 2>&1); expect "daemon down (PermissionRequest) → silent exit 0" 0 $? "$out"
out=$(echo "$PAYLOAD" | bash "$HOOK" 2>&1); expect "no event argument → silent exit 0" 0 $? "$out"

# --- discovery chain: $SMOOTH_FLOW_ADDR → flow.addr → daemon.addr (th-c103c1) ---
# flow.addr is how a hook reaches the SmoothFlow app's child daemon, which
# deliberately does not write daemon.addr (PR #546).
: >"$LOG"
echo "127.0.0.1:1" >"$TMP/dead.addr"
out=$(echo "$PAYLOAD" | SMOOTH_DAEMON_ADDR_FILE="$TMP/dead.addr" SMOOTH_FLOW_ADDR_FILE="$ADDR" bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1 && [ "$rc" = 0 ]; then ok "flow.addr wins over daemon.addr"; else bad "flow.addr was not preferred — rc=$rc out='$out'"; fi
: >"$LOG"
out=$(echo "$PAYLOAD" | SMOOTH_DAEMON_ADDR_FILE="$TMP/dead.addr" SMOOTH_FLOW_ADDR_FILE="$TMP/nope" SMOOTH_FLOW_ADDR="$(cat "$ADDR")" bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1 && [ "$rc" = 0 ]; then ok "\$SMOOTH_FLOW_ADDR wins over both files"; else bad "the env override did not win — rc=$rc out='$out'"; fi
: >"$LOG"
out=$(echo "$PAYLOAD" | SMOOTH_DAEMON_ADDR_FILE="$ADDR" SMOOTH_FLOW_ADDR_FILE="$TMP/nope" bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1 && [ "$rc" = 0 ]; then ok "no flow.addr → daemon.addr still serves the hook"; else bad "the daemon.addr fallback broke — rc=$rc out='$out'"; fi
: >"$LOG"
: >"$TMP/empty-flow.addr"
out=$(echo "$PAYLOAD" | SMOOTH_DAEMON_ADDR_FILE="$ADDR" SMOOTH_FLOW_ADDR_FILE="$TMP/empty-flow.addr" bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1 && [ "$rc" = 0 ]; then ok "an empty flow.addr falls through to daemon.addr"; else bad "an empty flow.addr blocked the fallback — rc=$rc out='$out'"; fi
: >"$LOG"
out=$(echo "$PAYLOAD" | SMOOTH_DAEMON_ADDR_FILE="$TMP/nope" SMOOTH_FLOW_ADDR_FILE="$TMP/nope2" bash "$HOOK" Stop 2>&1); expect "neither file → silent exit 0" 0 $? "$out"

# --- fire-and-forget envelope ---------------------------------------------------
: >"$LOG"
out=$(echo "$PAYLOAD" | bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1; then
    req=$(tail -1 "$LOG")
    got=$(printf '%s' "$req" | jq -r '[.path, .body.harness, .body.event, .body.session_id, .body.cwd, .body.payload.stop_hook_active] | @tsv')
    want=$(printf '/api/flow/hooks\tclaude-code\tStop\tsid-123\t/some/where\tfalse')
    if [ "$rc" = 0 ] && [ -z "$out" ] && [ "$got" = "$want" ]; then ok "Stop posts the {harness,event,session_id,cwd,payload} envelope"; else bad "Stop envelope — rc=$rc out='$out' got='$got'"; fi
else
    bad "Stop never reached the mock daemon"
fi

: >"$LOG"
out=$(printf 'not json at all' | bash "$HOOK" Notification 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '.body.payload.raw')" = "not json at all" ] && [ "$rc" = 0 ] && [ -z "$out" ]; then ok "non-JSON stdin is forwarded as payload.raw"; else bad "non-JSON stdin handling (rc=$rc out='$out')"; fi

: >"$LOG"
out=$(printf '' | bash "$HOOK" SessionEnd 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '.body.event')" = "SessionEnd" ] && [ "$(tail -1 "$LOG" | jq -r '.body.cwd')" = "$PWD" ] && [ "$rc" = 0 ]; then ok "empty stdin still posts (cwd falls back to \$PWD)"; else bad "empty stdin (rc=$rc)"; fi

# --- harness argument (th-4ad334: Codex reads the same hooks.json schema) ----------
: >"$LOG"
out=$(echo "$PAYLOAD" | bash "$HOOK" Stop codex 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '.body.harness')" = "codex" ] && [ "$rc" = 0 ] && [ -z "$out" ]; then ok "second argument sets the harness (codex)"; else bad "harness argument (rc=$rc out='$out')"; fi
: >"$LOG"
out=$(echo "$PAYLOAD" | FLOW_HOOK_HARNESS=opencode bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '.body.harness')" = "opencode" ] && [ "$rc" = 0 ]; then ok "FLOW_HOOK_HARNESS overrides the default"; else bad "FLOW_HOOK_HARNESS (rc=$rc)"; fi

# --- PermissionRequest: decision passthrough --------------------------------------
echo decide >"$MODE"
out=$(echo "$PAYLOAD" | bash "$HOOK" PermissionRequest 2>/dev/null); rc=$?
expect "PermissionRequest passes the daemon's decision through verbatim" 0 $rc "$out" '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'
echo empty >"$MODE"
out=$(echo "$PAYLOAD" | bash "$HOOK" PermissionRequest 2>/dev/null); expect "PermissionRequest: '{}' reply prints nothing (harness asks the user)" 0 $? "$out"
echo error >"$MODE"
out=$(echo "$PAYLOAD" | bash "$HOOK" PermissionRequest 2>/dev/null); expect "PermissionRequest: 500 reply prints nothing" 0 $? "$out"
echo slow >"$MODE"
start=$(date +%s)
out=$(echo "$PAYLOAD" | FLOW_HOOK_PERMISSION_TIMEOUT=1 bash "$HOOK" PermissionRequest 2>/dev/null); rc=$?
elapsed=$(( $(date +%s) - start ))
if [ "$rc" = 0 ] && [ -z "$out" ] && [ "$elapsed" -le 3 ]; then ok "PermissionRequest honours the wait timeout (${elapsed}s) and stays silent"; else bad "PermissionRequest timeout — rc=$rc out='$out' elapsed=${elapsed}s"; fi
echo decide >"$MODE"

# ── th stub: records argv; canned answers for prime/checkpoint ────────────────────
STUB="$TMP/bin"; mkdir -p "$STUB"
cat >"$STUB/th" <<'SH'
#!/usr/bin/env bash
echo "$*" >>"$TH_LOG"
case "$*" in
    *"prime --in-progress"*"--json"*) cat "$TH_PACKETS_JSON" ;;
    *"prime --in-progress"*) cat "$TH_PACKETS_TXT" ;;
    *"checkpoint"*) exit 0 ;;
esac
exit 0
SH
chmod +x "$STUB/th"
export TH="$STUB/th" TH_LOG="$TMP/th.log" TH_PACKETS_JSON="$TMP/packets.json" TH_PACKETS_TXT="$TMP/packets.txt"
WT="$TMP/wt"; mkdir -p "$WT"
echo '[{"pearl":{"id":"th-aaaaaa"}},{"pearl":{"id":"th-bbbbbb"}}]' >"$TH_PACKETS_JSON"
printf '# In-progress handoff\n\n## th-aaaaaa — T\nnext: ship\n' >"$TH_PACKETS_TXT"

echo "precompact-checkpoint.sh:"
: >"$TH_LOG"
out=$(printf '{"session_id":"sid-9","cwd":"%s","trigger":"auto"}' "$WT" | bash "$HERE/precompact-checkpoint.sh" 2>&1); rc=$?
if [ "$rc" = 0 ] && [ -z "$out" ] \
    && grep -q "^pearls prime --in-progress --cwd $WT --json$" "$TH_LOG" \
    && grep -q "^pearls checkpoint th-aaaaaa --auto --cwd $WT --session-id sid-9$" "$TH_LOG" \
    && grep -q "^pearls checkpoint th-bbbbbb --auto --cwd $WT --session-id sid-9$" "$TH_LOG"; then
    ok "PreCompact checkpoints every matching in_progress pearl with --auto + session id"
else
    bad "PreCompact — rc=$rc out='$out' th calls:"; sed 's/^/       /' "$TH_LOG"
fi
: >"$TH_LOG"; echo '[]' >"$TH_PACKETS_JSON"
out=$(printf '{"cwd":"%s"}' "$WT" | bash "$HERE/precompact-checkpoint.sh" 2>&1); rc=$?
if [ "$rc" = 0 ] && [ -z "$out" ] && ! grep -q checkpoint "$TH_LOG"; then ok "PreCompact with no matching pearls checkpoints nothing"; else bad "PreCompact no-match (rc=$rc out='$out')"; fi
out=$(printf '{"cwd":"%s"}' "$WT" | TH="$TMP/no-such-th" bash "$HERE/precompact-checkpoint.sh" 2>&1); expect "PreCompact without th → silent exit 0" 0 $? "$out"

echo "handoff-context.sh:"
out=$(printf '{"cwd":"%s","source":"compact"}' "$WT" | bash "$HERE/handoff-context.sh" 2>/dev/null); rc=$?
ctx=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext' 2>/dev/null)
if [ "$rc" = 0 ] && [ "$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName')" = "SessionStart" ] && printf '%s' "$ctx" | grep -q 'th-aaaaaa' && printf '%s' "$ctx" | grep -q 'next: ship'; then
    ok "SessionStart(compact) wraps the packet as additionalContext"
else
    bad "SessionStart packet — rc=$rc out='$out'"
fi
printf 'No in-progress pearls for this worktree.\n' >"$TH_PACKETS_TXT"
out=$(printf '{"cwd":"%s"}' "$WT" | bash "$HERE/handoff-context.sh" 2>&1); expect "SessionStart with no matching pearls prints nothing" 0 $? "$out"
out=$(printf '{"cwd":"%s"}' "$WT" | TH="$TMP/no-such-th" bash "$HERE/handoff-context.sh" 2>&1); expect "SessionStart without th → silent exit 0" 0 $? "$out"

echo "th-curl-hint.sh:"
out=$(printf '{"tool_name":"Bash","tool_input":{"command":"curl -s -X POST http://127.0.0.1:8899/api/flow/hooks -d {}"}}' | bash "$HERE/th-curl-hint.sh" 2>&1); expect "loopback daemon URL is not nagged" 0 $? "$out"

echo
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
