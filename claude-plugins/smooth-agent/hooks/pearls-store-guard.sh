#!/bin/bash
# pearls-store-guard: PreToolUse Bash hook that catches the patterns that
# wedge the Dolt-backed pearl store into "database is read only" — the
# single most recurring pearl-store failure. Dolt is single-writer; a
# stray manual delete of its internals, a raw `dolt` write that bypasses
# smooth-dolt's serialized server, or a swarm of background watchers all
# pin the store read-only for every other agent on the machine.
#
# Exit codes: 0 allow silently, 1 nudge (stderr hint visible to Claude,
# override by re-running), 2 hard block. We use 1 — non-blocking nudge.
# Bypass any hit with ` # pearls-guard:ack reason=...` on the command.
#
# Background: docs/Operations/Troubleshooting.md, pearls th-20f330 /
# th-5f35a5, memory "Pearls Dolt single-writer under parallel agents".

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
    cat >&2 <<EOF
⚠️  pearls-store-guard: $1

$2

If you genuinely need this (recovering a truly dead store, debugging the
engine itself), append \` # pearls-guard:ack reason=...\` and re-run.
EOF
}

# --- manual deletion of Dolt internals ------------------------------------------
# The noms LOCK / manifest / git-remote-cache are managed by smooth-dolt.
# rm'ing them by hand to "unstick" a read-only store races the engine and
# can corrupt the manifest — `th pearls doctor` reaps stale holders safely.
if echo "$CMD" | grep -qE 'rm\s+(-[a-zA-Z]+\s+)*[^|;&]*\.smooth/dolt'; then
    emit "manual delete under .smooth/dolt (the pearl store)" \
        "Don't hand-remove Dolt internals (noms/LOCK, manifest, git-remote-cache) to
unwedge a read-only store — that races the engine and can corrupt the manifest.
Use \`th pearls doctor --reap\` to clear stale lock holders, or \`th pearls pull\`
to re-sync. The git-remote cache is now shared + self-healing (pearl th-20f330)."
    exit 1
fi

# --- raw `dolt` / `smooth-dolt` writes that bypass the serialized server --------
# Writing to the store outside `th pearls` skips smooth-dolt's single-writer
# queue and can collide with the running server on the noms lock.
if echo "$CMD" | grep -qE '(^|[|;&]|\s)(smooth-)?dolt\s+(sql|commit|push|pull|gc|reset|merge|branch)\b' \
   && echo "$CMD" | grep -qE '\.smooth/dolt|pearls'; then
    emit "raw dolt write against the pearl store" \
        "Go through \`th pearls …\` (create/update/close/push/pull) instead of raw
\`dolt\`/\`smooth-dolt\` writes. The CLI routes through smooth-dolt's single-writer
server so concurrent agents serialize instead of colliding on the noms lock."
    exit 1
fi

# --- background pearl/msg watchers (single-writer contention) -------------------
# Each long-lived `th msg watch` / backgrounded pearl sync holds a writer;
# several at once reproduce the "database is read only" wedge.
if echo "$CMD" | grep -qE 'th\s+(msg\s+watch|pearls\s+(push|pull))' \
   && echo "$CMD" | grep -qE '(&\s*$|nohup|--watch|while\s+true)'; then
    emit "backgrounding a pearl/msg watcher" \
        "Dolt is single-writer — multiple background \`th msg watch\` / pearl-sync loops
pin the store read-only for every other agent. Start at most one watcher (the
/th-mail skill manages this), and let the coordinator do pearl pushes sequentially."
    exit 1
fi

exit 0
