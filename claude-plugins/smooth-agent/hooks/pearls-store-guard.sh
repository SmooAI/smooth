#!/bin/bash
# pearls-store-guard: PreToolUse Bash hook for the pearl store.
#
# Pearls live in ONE machine-global SQLite file, `~/.smooth/pearls.db`
# (pearl th-d3e842) — every project on this box is in it. Deleting or
# hand-editing that file loses every project's pearls at once, so the
# guard nudges on the two patterns that do that.
#
# Exit codes: 0 allow silently, 1 nudge (stderr hint visible to Claude,
# override by re-running), 2 hard block. We use 1 — non-blocking nudge.
# Bypass any hit with ` # pearls-guard:ack reason=...` on the command.

INPUT=$(cat)
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
[[ "$TOOL_NAME" != "Bash" ]] && exit 0

CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)
[[ -z "$CMD" ]] && exit 0

# Explicit ack escape hatch.
if echo "$CMD" | grep -q 'pearls-guard:ack'; then
    exit 0
fi

emit() {
    cat >&2 <<MSG
⚠️  pearls-store-guard: $1

$2

If you genuinely need this, append \` # pearls-guard:ack reason=...\` and re-run.
MSG
}

# --- deleting the pearl database (every project's pearls) ----------------------
if echo "$CMD" | grep -qE 'rm\s+(-[a-zA-Z]+\s+)*[^|;&]*\.smooth/pearls\.db'; then
    emit "delete of the pearl store" \
        "~/.smooth/pearls.db holds EVERY project's pearls on this machine; it is not safe to rm by hand.
Copy the .db aside first (\`th db path\`) if you must."
    exit 1
fi

# --- raw sqlite writes that bypass `th pearls` ---------------------------------
# Hand-rolled UPDATE/DELETE skips history rows and the project scoping.
if echo "$CMD" | grep -qE '(^|[|;&]|\s)sqlite3?\s+[^|;&]*pearls\.db' \
   && echo "$CMD" | grep -qiE '\b(insert|update|delete|drop|alter)\b'; then
    emit "raw sqlite write against the pearl store" \
        "Go through \`th pearls …\` (create/update/close/dep/label/comment) instead of raw
sqlite writes — the CLI records history and scopes every row to its project."
    exit 1
fi

exit 0
