#!/usr/bin/env bash
# smooth-agent — SmoothFlow state hook (pearl th-1b8e05, epic th-6ac036).
#
# One script for every harness lifecycle event. Forwards the hook payload to
# the daemon's flow engine so session state comes from hooks, not scrollback
# scraping:
#
#   POST http://<daemon.addr>/api/flow/hooks
#        {harness:"claude-code", event, session_id, cwd, payload}
#
# Usage (from hooks.json): flow-hook.sh <Event>
#
# Contract — this hook must NEVER block the harness:
#   * daemon.addr missing/empty, daemon down, curl/jq missing → exit 0, silent.
#   * PermissionRequest waits up to FLOW_HOOK_PERMISSION_TIMEOUT (120 s) for the
#     engine's decision and prints the reply body verbatim on stdout — it IS the
#     harness's decision JSON. Any non-2xx or non-decision reply → print nothing
#     (the harness falls back to asking the user).
#   * Every other event is fire-and-forget, detached, 2 s timeout.
#   * Exit code is always 0. (Only exit 2 blocks a PreToolUse; nothing here should.)
#
# Test: flow-hook.test.sh. Overrides for tests: SMOOTH_DAEMON_ADDR_FILE,
# FLOW_HOOK_PERMISSION_TIMEOUT, FLOW_HOOK_TIMEOUT.
set -u

event="${1:-}"
[ -n "$event" ] || exit 0
command -v curl >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0

addr_file="${SMOOTH_DAEMON_ADDR_FILE:-$HOME/.smooth/daemon.addr}"
[ -r "$addr_file" ] || exit 0
addr="$(tr -d '[:space:]' <"$addr_file" 2>/dev/null || true)"
[ -n "$addr" ] || exit 0
case "$addr" in
    http://* | https://*) url="$addr/api/flow/hooks" ;;
    *) url="http://$addr/api/flow/hooks" ;;
esac

input="$(cat 2>/dev/null || true)"
[ -n "$input" ] || input='{}'
# A payload that isn't a JSON object is forwarded as {"raw": "..."} rather than dropped.
if ! printf '%s' "$input" | jq -e 'type == "object"' >/dev/null 2>&1; then
    input="$(jq -cn --arg raw "$input" '{raw: $raw}')"
fi

body="$(printf '%s' "$input" | jq -c --arg event "$event" --arg cwd "$PWD" \
    '{harness: "claude-code", event: $event, session_id: (.session_id // ""), cwd: (.cwd // $cwd), payload: .}' 2>/dev/null)" || exit 0

if [ "$event" = "PermissionRequest" ]; then
    reply="$(curl -fsS -m "${FLOW_HOOK_PERMISSION_TIMEOUT:-120}" -X POST -H 'Content-Type: application/json' \
        --data-binary "$body" "$url" 2>/dev/null)" || exit 0
    # Only a real decision object goes to stdout; `{}` or garbage means "no opinion".
    if printf '%s' "$reply" | jq -e '.hookSpecificOutput.decision.behavior? | type == "string"' >/dev/null 2>&1; then
        printf '%s\n' "$reply"
    fi
    exit 0
fi

# Fire-and-forget: detached so the hook returns immediately even if the daemon is slow.
(
    curl -sS -m "${FLOW_HOOK_TIMEOUT:-2}" -X POST -H 'Content-Type: application/json' \
        --data-binary "$body" "$url" >/dev/null 2>&1 || true
) </dev/null >/dev/null 2>&1 &
disown 2>/dev/null || true
exit 0
