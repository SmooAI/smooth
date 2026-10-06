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
            f.write(json.dumps({"path": self.path, "token": self.headers.get("X-Smooth-Flow-Hook-Token"), "ctype": self.headers.get("Content-Type"), "raw": body.decode("utf-8", "replace"), "body": json.loads(body)}) + "\n")
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
# Never the real ~/.smooth/flow.addr: on a machine running SmoothFlow it wins
# over daemon.addr and every test below would post to the live engine.
export SMOOTH_FLOW_ADDR_FILE="$TMP/no-flow.addr"
unset SMOOTH_FLOW_ADDR
# Wait for a fire-and-forget request to land (they are detached).
wait_log() { for _ in $(seq 1 50); do [ "$(wc -l <"$LOG" 2>/dev/null | tr -d ' ')" -ge "$1" ] && return 0; sleep 0.1; done; return 1; }

HOOK="$HERE/flow-hook.sh"
# The curl path below is the shim's fallback; pin it so a `th` on this
# machine's PATH that knows `flow hook` doesn't take over (th-f97a27).
export FLOW_HOOK_NATIVE=0
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

# --- other hook-capable harnesses (th-b00115) ------------------------------------
# Payload variants: Copilot camelCase sessionId, Cursor conversation_id + workspace_roots.
: >"$LOG"
out=$(echo '{"sessionId":"cop-1","cwd":"/cop"}' | bash "$HOOK" Stop copilot 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '[.body.session_id, .body.cwd] | @tsv')" = "$(printf 'cop-1\t/cop')" ] && [ "$rc" = 0 ] && [ "$out" = "{}" ]; then ok "copilot: sessionId is the session id; stdout answers {}"; else bad "copilot envelope (rc=$rc out='$out')"; fi
: >"$LOG"
out=$(echo '{"conversation_id":"cur-1","workspace_roots":["/cur/root","/other"]}' | bash "$HOOK" beforeSubmitPrompt cursor-agent 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '[.body.harness, .body.session_id, .body.cwd] | @tsv')" = "$(printf 'cursor-agent\tcur-1\t/cur/root')" ] && [ "$rc" = 0 ] && [ "$out" = '{"continue":true}' ]; then ok "cursor-agent: conversation_id + workspace_roots[0]; beforeSubmitPrompt answers continue"; else bad "cursor-agent envelope (rc=$rc out='$out')"; fi
out=$(echo '{}' | SMOOTH_DAEMON_ADDR_FILE="$TMP/nope" bash "$HOOK" stop cursor-agent 2>&1); expect "cursor-agent answers {} even with no daemon" 0 $? "$out" '{}'
out=$(echo '{}' | SMOOTH_DAEMON_ADDR_FILE="$TMP/nope" bash "$HOOK" beforeSubmitPrompt cursor-agent 2>&1); expect "cursor-agent gate answers continue even with no daemon" 0 $? "$out" '{"continue":true}'
out=$(echo '{}' | SMOOTH_DAEMON_ADDR_FILE="$TMP/nope" bash "$HOOK" AfterAgent gemini 2>&1); expect "gemini answers {} even with no daemon" 0 $? "$out" '{}'
out=$(echo '{}' | SMOOTH_DAEMON_ADDR_FILE="$TMP/nope" bash "$HOOK" Stop droid 2>&1); expect "droid (Claude-style stdout) stays silent" 0 $? "$out"
# A pre-answered harness never prints a second JSON document, even for a decision.
echo decide >"$MODE"
out=$(echo "$PAYLOAD" | bash "$HOOK" PermissionRequest copilot 2>/dev/null); expect "copilot PermissionRequest: only the {} answer, no decision passthrough" 0 $? "$out" '{}'
# qwen speaks Claude's decision protocol: its PermissionRequest is held for the decision.
out=$(echo "$PAYLOAD" | bash "$HOOK" PermissionRequest qwen 2>/dev/null); expect "qwen PermissionRequest passes the decision through" 0 $? "$out" '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'
# SMOOTH_FLOW_ID rides along as flow_id; absent ⇒ no flow_id key at all.
: >"$LOG"
out=$(echo '{"session_id":""}' | SMOOTH_FLOW_ID=fs-42 bash "$HOOK" BeforeAgent gemini 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '[.body.flow_id, .body.session_id] | @tsv')" = "$(printf 'fs-42\t')" ] && [ "$rc" = 0 ]; then ok "SMOOTH_FLOW_ID is posted as flow_id"; else bad "flow_id (rc=$rc)"; fi
: >"$LOG"
out=$(echo "$PAYLOAD" | env -u SMOOTH_FLOW_ID bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '.body | has("flow_id")')" = "false" ] && [ "$rc" = 0 ]; then ok "no SMOOTH_FLOW_ID ⇒ no flow_id key"; else bad "flow_id absent (rc=$rc)"; fi

# --- hook token (th-91d032) ---------------------------------------------------------
TOK=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
printf '%s\n' "$TOK" >"$TMP/hook.token"; chmod 600 "$TMP/hook.token"
: >"$LOG"
out=$(echo "$PAYLOAD" | SMOOTH_FLOW_HOOK_TOKEN_FILE="$TMP/hook.token" bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '.token')" = "$TOK" ] && [ "$rc" = 0 ] && [ -z "$out" ]; then ok "the launch's token rides X-Smooth-Flow-Hook-Token"; else bad "token header (rc=$rc out='$out' log=$(tail -1 "$LOG"))"; fi
: >"$LOG"
out=$(echo "$PAYLOAD" | bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '.token')" = "null" ] && [ "$rc" = 0 ]; then ok "no token file → no header (an adopted harness still reports)"; else bad "tokenless post (rc=$rc)"; fi
: >"$LOG"
out=$(echo "$PAYLOAD" | SMOOTH_FLOW_HOOK_TOKEN_FILE="$TMP/no-such.token" bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '.token')" = "null" ] && [ "$rc" = 0 ] && [ -z "$out" ]; then ok "a missing token file never blocks the hook"; else bad "missing token file (rc=$rc out='$out')"; fi
# A hostile token file cannot smuggle curl config (a second url, an output file).
printf 'abc"\nurl = "http://127.0.0.1:1/x"\noutput = "%s/pwned"\n' "$TMP" >"$TMP/evil.token"
: >"$LOG"
out=$(echo "$PAYLOAD" | SMOOTH_FLOW_HOOK_TOKEN_FILE="$TMP/evil.token" bash "$HOOK" Stop 2>&1); rc=$?
if wait_log 1 && [ "$(tail -1 "$LOG" | jq -r '.path')" = "/api/flow/hooks" ] && tail -1 "$LOG" | jq -r '.token' | grep -Eq '^abc[0-9a-fA-F]*$' && [ ! -e "$TMP/pwned" ] && [ "$rc" = 0 ]; then ok "token file is reduced to hex — no curl-config injection"; else bad "token injection (rc=$rc log=$(tail -1 "$LOG"))"; fi
# The token is never an argument: a curl shim records argv.
mkdir -p "$TMP/shim"
REAL_CURL="$(command -v curl)"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >>"%s/curl-argv"\nexec "%s" "$@"\n' "$TMP" "$REAL_CURL" >"$TMP/shim/curl"
chmod +x "$TMP/shim/curl"
: >"$TMP/curl-argv"
echo decide >"$MODE"
out=$(echo "$PAYLOAD" | PATH="$TMP/shim:$PATH" SMOOTH_FLOW_HOOK_TOKEN_FILE="$TMP/hook.token" bash "$HOOK" PermissionRequest 2>/dev/null); rc=$?
if [ "$rc" = 0 ] && [ -s "$TMP/curl-argv" ] && ! grep -q "$TOK" "$TMP/curl-argv" && [ "$(tail -1 "$LOG" | jq -r '.token')" = "$TOK" ] && [ -n "$out" ]; then ok "the token never appears in curl's argv (ps-safe)"; else bad "token in argv? rc=$rc argv=$(cat "$TMP/curl-argv")"; fi

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

# --- the shim hands off to `th flow hook` when th knows it (th-f97a27) -------------
echo "flow-hook.sh → th flow hook:"
NATIVE="$TMP/native"; mkdir -p "$NATIVE"
cat >"$NATIVE/th" <<'SH'
#!/usr/bin/env bash
[ "$*" = "flow hook --help" ] && exit 0
printf '%s|%s\n' "$*" "$(cat)" >>"$NATIVE_LOG"
printf 'native-out\n'
SH
cat >"$NATIVE/old-th" <<'SH'
#!/usr/bin/env bash
echo "error: unrecognized subcommand 'hook'" >&2
exit 2
SH
chmod +x "$NATIVE/th" "$NATIVE/old-th"
export NATIVE_LOG="$TMP/native.log"
: >"$NATIVE_LOG"
out=$(echo "$PAYLOAD" | FLOW_HOOK_NATIVE=1 TH="$NATIVE/th" bash "$HOOK" Stop codex 2>&1); rc=$?
if [ "$rc" = 0 ] && [ "$out" = "native-out" ] && [ "$(cat "$NATIVE_LOG")" = "flow hook codex Stop|$PAYLOAD" ]; then
    ok "execs th flow hook <harness> <Event> with stdin and stdout passed through"
else
    bad "shim exec — rc=$rc out='$out' log='$(cat "$NATIVE_LOG")'"
fi
: >"$NATIVE_LOG"
out=$(echo "$PAYLOAD" | FLOW_HOOK_NATIVE=1 TH="$NATIVE/th" bash "$HOOK" PermissionRequest 2>&1)
if [ "$(cut -d'|' -f1 "$NATIVE_LOG")" = "flow hook claude-code PermissionRequest" ]; then ok "the harness still defaults to claude-code"; else bad "default harness — log='$(cat "$NATIVE_LOG")'"; fi
: >"$LOG"
echo decide >"$MODE"
out=$(echo "$PAYLOAD" | FLOW_HOOK_NATIVE=1 TH="$NATIVE/old-th" bash "$HOOK" PermissionRequest 2>/dev/null); rc=$?
expect "a th without flow hook falls back to the curl path" 0 $rc "$out" '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'

# --- parity: the native hook sends the same bytes as the script -------------------
# FLOW_HOOK_TH=<path to a th with `flow hook`> (CI builds one) replays a matrix
# of payloads through both and diffs what the mock daemon received + stdout.
if [ -n "${FLOW_HOOK_TH:-}" ] && [ -x "$FLOW_HOOK_TH" ]; then
    echo "th flow hook parity (FLOW_HOOK_TH=$FLOW_HOOK_TH):"
    printf 'abc123DEF\n' >"$TMP/parity.token"
    parity() { # <name> <harness> <event> <payload> [VAR=value …]
        local name="$1" h="$2" ev="$3" payload="$4"; shift 4
        : >"$LOG"
        local a b la lb
        a=$(printf '%s' "$payload" | env "$@" bash "$HOOK" "$ev" "$h" 2>/dev/null); wait_log 1; la=$(cat "$LOG"); : >"$LOG"
        b=$(printf '%s' "$payload" | env "$@" "$FLOW_HOOK_TH" flow hook "$h" "$ev" 2>/dev/null); wait_log 1; lb=$(cat "$LOG")
        if [ -n "$la" ] && [ "$la" = "$lb" ] && [ "$a" = "$b" ]; then ok "parity: $name"; else bad "parity: $name"$'\n'"       script: $la | $a"$'\n'"       native: $lb | $b"; fi
    }
    echo decide >"$MODE"
    parity "Stop, Claude shape" claude-code Stop "$PAYLOAD"
    parity "token + flow_id" codex PostToolUse "$PAYLOAD" SMOOTH_FLOW_HOOK_TOKEN_FILE="$TMP/parity.token" SMOOTH_FLOW_ID=flow-7
    parity "PermissionRequest decision" claude-code PermissionRequest "$PAYLOAD" SMOOTH_FLOW_HOOK_TOKEN_FILE="$TMP/parity.token"
    parity "copilot sessionId + preface" copilot Stop '{"sessionId":"cp-1","toolName":"bash"}'
    parity "cursor conversation_id + workspace_roots" cursor-agent beforeSubmitPrompt '{"conversation_id":"cu-1","workspace_roots":["/r1","/r2"]}'
    parity "gemini PermissionRequest never long-polls" gemini PermissionRequest '{"session_id":"g-1"}'
    parity "non-JSON stdin → raw" qwen Stop 'not json at all'
    parity "empty stdin → {}" droid Stop ''
    parity "unicode + nesting" claude-code UserPromptSubmit '{"session_id":"u","prompt":"héllo \"q\" \\ ✓","n":[1,2.5,{"a":null}],"t":true}'
    echo empty >"$MODE"
    parity "PermissionRequest no opinion" claude-code PermissionRequest "$PAYLOAD"
    echo decide >"$MODE"
else
    echo "th flow hook parity: skipped (set FLOW_HOOK_TH to a th with \`flow hook\`)"
fi

# ── th stub: records argv; canned answers for prime/checkpoint ────────────────────
STUB="$TMP/bin"; mkdir -p "$STUB"
cat >"$STUB/th" <<'SH'
#!/usr/bin/env bash
echo "$*" >>"$TH_LOG"
case "$*" in
    *"prime --in-progress"*"--json"*) cat "$TH_PACKETS_JSON" ;;
    *"prime --in-progress"*) cat "$TH_PACKETS_TXT" ;;
    "pearls show "*" --handoff") printf '## %s — T\nnext: ship\n%s\n' "$3" "${TH_PACKET_PAD:-}" ;;
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
echo '[{"pearl":{"id":"th-aaaaaa"}}]' >"$TH_PACKETS_JSON"
out=$(printf '{"cwd":"%s","source":"compact"}' "$WT" | bash "$HERE/handoff-context.sh" 2>/dev/null); rc=$?
ctx=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext' 2>/dev/null)
if [ "$rc" = 0 ] && [ "$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName')" = "SessionStart" ] && printf '%s' "$ctx" | grep -q 'th-aaaaaa' && printf '%s' "$ctx" | grep -q 'next: ship'; then
    ok "SessionStart(compact) wraps the packet as additionalContext"
else
    bad "SessionStart packet — rc=$rc out='$out'"
fi
echo '[]' >"$TH_PACKETS_JSON"
out=$(printf '{"cwd":"%s"}' "$WT" | bash "$HERE/handoff-context.sh" 2>&1); expect "SessionStart with no matching pearls prints nothing" 0 $? "$out"
out=$(printf '{"cwd":"%s"}' "$WT" | TH="$TMP/no-such-th" bash "$HERE/handoff-context.sh" 2>&1); expect "SessionStart without th → silent exit 0" 0 $? "$out"

# More than SMOOTH_HANDOFF_MAX relevant pearls → capped, with a "+N more" line;
# each packet is cut to SMOOTH_HANDOFF_PACKET_CHARS.
echo '[{"pearl":{"id":"th-000001"}},{"pearl":{"id":"th-000002"}},{"pearl":{"id":"th-000003"}},{"pearl":{"id":"th-000004"}},{"pearl":{"id":"th-000005"}}]' >"$TH_PACKETS_JSON"
ctx=$(printf '{"cwd":"%s"}' "$WT" | TH_PACKET_PAD="$(printf 'x%.0s' $(seq 1 3000))" bash "$HERE/handoff-context.sh" 2>/dev/null | jq -r '.hookSpecificOutput.additionalContext')
if printf '%s' "$ctx" | grep -q 'th-000003' && ! printf '%s' "$ctx" | grep -q '## th-000004' && printf '%s' "$ctx" | grep -q '+2 more' \
    && printf '%s' "$ctx" | grep -q 'truncated — th pearls show th-000001 --handoff' && [ "${#ctx}" -lt 6000 ]; then
    ok "SessionStart caps packets (3) and packet size, naming the rest"
else
    bad "SessionStart cap — ${#ctx} chars"
fi

# PRIMARY checkout: matching by worktree alone pulls in every pearl ever
# checkpointed there, so only this session's pearls (or the branch's) count.
PRIMARY="$TMP/primary"; mkdir -p "$PRIMARY"; git -C "$PRIMARY" init -q -b main
LINKED="$TMP/linked"; git -C "$PRIMARY" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init && git -C "$PRIMARY" worktree add -q -b th-cccccc-fix "$LINKED"
echo '[{"pearl":{"id":"th-aaaaaa"},"handoff":{"agent_session_id":"sid-mine"}},{"pearl":{"id":"th-bbbbbb"},"handoff":{"agent_session_id":"sid-other"}},{"pearl":{"id":"th-cccccc"},"handoff":{}}]' >"$TH_PACKETS_JSON"
ctx=$(printf '{"cwd":"%s","session_id":"sid-mine"}' "$PRIMARY" | bash "$HERE/handoff-context.sh" 2>/dev/null | jq -r '.hookSpecificOutput.additionalContext')
if printf '%s' "$ctx" | grep -q '## th-aaaaaa' && ! printf '%s' "$ctx" | grep -q 'th-bbbbbb' && ! printf '%s' "$ctx" | grep -q 'th-cccccc'; then
    ok "primary checkout: only the pearl this session checkpointed"
else
    bad "primary checkout filter — ctx='$ctx'"
fi
out=$(printf '{"cwd":"%s","session_id":"sid-new"}' "$PRIMARY" | bash "$HERE/handoff-context.sh" 2>&1); expect "primary checkout, fresh session → nothing" 0 $? "$out"
: >"$TH_LOG"
printf '{"cwd":"%s","session_id":"sid-new"}' "$PRIMARY" | bash "$HERE/precompact-checkpoint.sh" >/dev/null 2>&1
if ! grep -q checkpoint "$TH_LOG"; then ok "primary checkout, fresh session → PreCompact stamps nothing"; else bad "PreCompact stamped foreign pearls:"; sed 's/^/       /' "$TH_LOG"; fi
ctx=$(printf '{"cwd":"%s","session_id":"sid-x"}' "$LINKED" | bash "$HERE/handoff-context.sh" 2>/dev/null | jq -r '.hookSpecificOutput.additionalContext')
if printf '%s' "$ctx" | grep -q '## th-aaaaaa' && printf '%s' "$ctx" | grep -q '## th-cccccc'; then
    ok "linked worktree: every pearl matched for it is relevant"
else
    bad "linked worktree — ctx='$ctx'"
fi

echo "th-curl-hint.sh:"
out=$(printf '{"tool_name":"Bash","tool_input":{"command":"curl -s -X POST http://127.0.0.1:8899/api/flow/hooks -d {}"}}' | bash "$HERE/th-curl-hint.sh" 2>&1); expect "loopback daemon URL is not nagged" 0 $? "$out"

echo
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
