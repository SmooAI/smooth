#!/usr/bin/env bash
# smooth-agent — SmoothFlow state hook (pearl th-1b8e05, epic th-6ac036).
#
# One script for every harness lifecycle event. Forwards the hook payload to
# the daemon's flow engine so session state comes from hooks, not scrollback
# scraping:
#
#   POST http://<flow.addr or daemon.addr>/api/flow/hooks
#        X-Smooth-Flow-Hook-Token: <this launch's token>
#        {harness, event, session_id, cwd, payload}
#
# Auth (th-91d032): when SmoothFlow launched this harness, the pane has
# $SMOOTH_FLOW_HOOK_TOKEN_FILE, naming a 0600 file with a token issued to this
# one launch. The engine only lets a hook speak for the session its token was
# issued to. Without one (a harness started in a plain terminal) the hook still
# posts: the engine may adopt it (th-c103c1), but it can only report state,
# never open an approvable permission request.
#
# Usage (from hooks.json): flow-hook.sh <Event> [harness]
#   harness defaults to claude-code; Codex ≥ 0.153 reads the same hooks.json
#   schema from ~/.codex/hooks.json, so `th pkg` renders the package's
#   harness/codex/hooks.json overlay there with `flow-hook.sh <Event> codex`
#   (pearl th-4ad334). FLOW_HOOK_HARNESS overrides the default.
#
#   Other hook-capable harnesses (pearl th-b00115) pass their SmoothFlow
#   manifest name — gemini, qwen, droid, copilot, cursor-agent — and the
#   engine maps their native event names through that manifest's event_map.
#   Their payloads differ, so the envelope reads the first of:
#     session_id ← .session_id | .sessionId | .conversation_id
#     cwd        ← .cwd | .workspace_roots[0] | $PWD
#   and adds flow_id ← $SMOOTH_FLOW_ID, the flow row the engine launched the
#   pane as, so a harness with no pre-assigned id binds without cwd guessing.
#
#   Harnesses that parse a hook's stdout get a no-opinion answer printed
#   FIRST, before anything can exit early: gemini/copilot `{}`, cursor-agent
#   `{"continue":true}` for beforeSubmitPrompt and `{}` otherwise (Cursor fails
#   closed on empty stdout for its gate hooks, so the overlay subscribes to
#   none of the permission gates at all).
#
# Contract — this hook must NEVER block the harness:
#   * no address file, daemon down, curl/jq missing → exit 0, silent.
#   * PermissionRequest waits up to FLOW_HOOK_PERMISSION_TIMEOUT (120 s) for the
#     engine's decision and prints the reply body verbatim on stdout — it IS the
#     harness's decision JSON. Any non-2xx or non-decision reply → print nothing
#     (the harness falls back to asking the user).
#   * Every other event is fire-and-forget, detached, 2 s timeout.
#   * Exit code is always 0. (Only exit 2 blocks a PreToolUse; nothing here should.)
#
# Test: flow-hook.test.sh. Overrides for tests: SMOOTH_FLOW_ADDR,
# SMOOTH_FLOW_ADDR_FILE, SMOOTH_DAEMON_ADDR_FILE, SMOOTH_FLOW_HOOK_TOKEN_FILE,
# FLOW_HOOK_PERMISSION_TIMEOUT, FLOW_HOOK_TIMEOUT.
set -u

event="${1:-}"
harness="${2:-${FLOW_HOOK_HARNESS:-claude-code}}"
[ -n "$event" ] || exit 0
answered=0
case "$harness" in
    gemini | copilot) printf '{}\n'; answered=1 ;;
    cursor-agent) if [ "$event" = "beforeSubmitPrompt" ]; then printf '{"continue":true}\n'; else printf '{}\n'; fi; answered=1 ;;
esac
command -v curl >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0

# Discovery chain (th-c103c1): $SMOOTH_FLOW_ADDR → ~/.smooth/flow.addr →
# ~/.smooth/daemon.addr. flow.addr is claimed by whichever daemon hosts the
# live flow engine, which is how hooks reach the SmoothFlow app's child daemon
# — it deliberately does not write daemon.addr (PR #546).
flow_addr_file="${SMOOTH_FLOW_ADDR_FILE:-$HOME/.smooth/flow.addr}"
addr_file="${SMOOTH_DAEMON_ADDR_FILE:-$HOME/.smooth/daemon.addr}"
read_addr() { [ -r "$1" ] && tr -d '[:space:]' <"$1" 2>/dev/null; }
addr="${SMOOTH_FLOW_ADDR:-}"
[ -n "$addr" ] || addr="$(read_addr "$flow_addr_file" || true)"
[ -n "$addr" ] || addr="$(read_addr "$addr_file" || true)"
[ -n "$addr" ] || exit 0
case "$addr" in
    http://* | https://*) url="$addr/api/flow/hooks" ;;
    *) url="http://$addr/api/flow/hooks" ;;
esac

# The token reaches curl as a config line on its stdin (`-K -`), never as an
# argument, so it cannot be read off `ps`. Unreadable file → no token.
token=""
if [ -n "${SMOOTH_FLOW_HOOK_TOKEN_FILE:-}" ] && [ -r "$SMOOTH_FLOW_HOOK_TOKEN_FILE" ]; then
    token="$(tr -cd '0-9a-fA-F' <"$SMOOTH_FLOW_HOOK_TOKEN_FILE" 2>/dev/null | head -c 128)"
fi
curl_cfg() {
    printf 'header = "Content-Type: application/json"\n'
    [ -z "$token" ] || printf 'header = "X-Smooth-Flow-Hook-Token: %s"\n' "$token"
}

input="$(cat 2>/dev/null || true)"
[ -n "$input" ] || input='{}'
# A payload that isn't a JSON object is forwarded as {"raw": "..."} rather than dropped.
if ! printf '%s' "$input" | jq -e 'type == "object"' >/dev/null 2>&1; then
    input="$(jq -cn --arg raw "$input" '{raw: $raw}')"
fi

body="$(printf '%s' "$input" | jq -c --arg harness "$harness" --arg event "$event" --arg cwd "$PWD" --arg flow_id "${SMOOTH_FLOW_ID:-}" \
    '{harness: $harness, event: $event,
      session_id: ([.session_id, .sessionId, .conversation_id] | map(select(type == "string" and . != "")) | first // ""),
      cwd: ([.cwd, (.workspace_roots? | if type == "array" then .[0] else null end)] | map(select(type == "string" and . != "")) | first // $cwd),
      payload: .}
     + (if $flow_id == "" then {} else {flow_id: $flow_id} end)' 2>/dev/null)" || exit 0

# Only a harness whose stdout is still ours can carry a decision.
if [ "$event" = "PermissionRequest" ] && [ "$answered" = 0 ]; then
    reply="$(curl_cfg | curl -K - -fsS -m "${FLOW_HOOK_PERMISSION_TIMEOUT:-120}" -X POST \
        --data-binary "$body" "$url" 2>/dev/null)" || exit 0
    # Only a real decision object goes to stdout; `{}` or garbage means "no opinion".
    if printf '%s' "$reply" | jq -e '.hookSpecificOutput.decision.behavior? | type == "string"' >/dev/null 2>&1; then
        printf '%s\n' "$reply"
    fi
    exit 0
fi

# Fire-and-forget: detached so the hook returns immediately even if the daemon is slow.
(
    curl_cfg | curl -K - -sS -m "${FLOW_HOOK_TIMEOUT:-2}" -X POST \
        --data-binary "$body" "$url" >/dev/null 2>&1 || true
) </dev/null >/dev/null 2>&1 &
disown 2>/dev/null || true
exit 0
