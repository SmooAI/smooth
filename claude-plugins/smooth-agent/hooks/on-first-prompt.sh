#!/usr/bin/env bash
# smooth-agent UserPromptSubmit hook.
#
# Fires ONCE per session, after the first user prompt, to nudge a plain `claude`
# session — registered at startup under a placeholder handle by
# register-agent.sh — to rename itself to a task-meaningful handle now that the
# task is known. stdout from a UserPromptSubmit hook is injected as context.
#
# UserPromptSubmit delivers a JSON payload on stdin (session_id, prompt, cwd).
# We only need session_id. State lives at ~/.smooth/agent-sessions/<session_id>
# (the placeholder handle, written by register-agent.sh); a sibling
# `<session_id>.nudged` marker means we've already fired.
#
# Silent no-op (exit 0) when: no session_id, no state file (th absent at startup,
# or a `th claude run` worker whose handle came from SMOOTH_AGENT_HANDLE — those
# are already meaningful and must not be nudged), or the marker already exists.
set -euo pipefail

# Workers carry a meaningful handle already — never nudge them.
[ -n "${SMOOTH_AGENT_HANDLE:-}" ] && exit 0

command -v jq >/dev/null 2>&1 || exit 0

INPUT="$(cat 2>/dev/null || true)"
[ -n "$INPUT" ] || exit 0

session_id="$(printf '%s' "$INPUT" | jq -r '.session_id // empty' 2>/dev/null || true)"
[ -n "$session_id" ] || exit 0

state_file="$HOME/.smooth/agent-sessions/$session_id"
nudged_file="$state_file.nudged"

[ -f "$state_file" ] || exit 0
[ -f "$nudged_file" ] && exit 0

handle="$(cat "$state_file" 2>/dev/null || true)"
[ -n "$handle" ] || exit 0

# Fire exactly once — mark before echoing so a re-run can't double-nudge.
touch "$nudged_file" 2>/dev/null || true

echo "th-mail: you are registered on the bus under the placeholder handle '$handle'. Now that you know the task, pick a concise task-meaningful handle (lowercase-kebab, e.g. 'fix-auth-refresh') and run 'th agent rename --from $handle --to <new-handle>' — this carries your mail over and you won't be asked again."
exit 0
