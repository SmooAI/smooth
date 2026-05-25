#!/bin/bash
# th-curl-hint: PreToolUse Bash hook that nudges the agent toward the `th` CLI
# whenever the command about to run is a raw curl against a Smoo platform endpoint
# that already has a `th` wrapper. Also flags two well-known footguns (sst secret
# list, gh secret set with stdin echo) toward their scripts/secret-helpers wrappers.
#
# Exit codes: 0 allow silently, 1 ask the user (with stderr hint visible to Claude),
# 2 hard block. We use 1 — non-blocking nudge with a clear hint, override by confirming.
#
# Background: docs/Engineering/Using-th-CLI.md  ·  pearl th-500495 / th-8b3d79

INPUT=$(cat)
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
[[ "$TOOL_NAME" != "Bash" ]] && exit 0

CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)
[[ -z "$CMD" ]] && exit 0

# Bypass: if the message body itself mentions `th-curl-hint:ack` (e.g. the agent
# explained why curl is the right call), let it through.
if echo "$CMD" | grep -q 'th-curl-hint:ack'; then
    exit 0
fi

emit() {
    cat >&2 <<EOF
⚠️  th-curl-hint: $1

$2

If you really need raw curl here (e.g. testing the wrapper itself, or hitting a
verb that has no \`th\` equivalent yet), append \` # th-curl-hint:ack reason=...\`
to the command and re-run. If this is the second time you're overriding for the
same reason, file a pearl: a missing \`th\` subcommand is the actual fix.

Reference: docs/Engineering/Using-th-CLI.md
EOF
}

# --- auth.smoo.ai token endpoint -----------------------------------------------
if echo "$CMD" | grep -qE 'curl[^|;&]+auth\.smoo\.ai/token'; then
    emit "raw curl against auth.smoo.ai/token" \
        "Use \`th api login\` (or \`SMOOAI_CLIENT_ID=… SMOOAI_CLIENT_SECRET=… th api login\`).
It exchanges client_credentials for a JWT and stores it at ~/.smooth/auth/smooai.json
so subsequent \`th api …\` calls just work."
    exit 1
fi

# --- api.smoo.ai - the big one --------------------------------------------------
if echo "$CMD" | grep -qE 'curl[^|;&]+api\.smoo\.ai'; then
    emit "raw curl against api.smoo.ai" \
        "Use \`th api …\` — it handles auth-header injection, JWT refresh, JSON pretty-printing,
and pagination. Quick map:
  /organizations/<id>/agents       → th api agents list [--org <id>]
  /organizations/<id>/knowledge    → th api knowledge list
  /organizations/<id>/config/…     → th api config (schemas|environments|values|feature-flag)
  /organizations/<id>/jobs         → th api jobs list
  /organizations/<id>/members      → th api members list
  /organizations/<id>/auth-clients → th api keys list      (dashboard auth required)
  /admin/…                         → th admin … (planned — see pearl th-feebd2)
Full surface: th api help"
    exit 1
fi

# --- atlassian.net Jira REST ----------------------------------------------------
if echo "$CMD" | grep -qE 'curl[^|;&]+atlassian\.net/rest/api'; then
    emit "raw curl against Jira REST" \
        "For read paths use \`th jira sync --pull\` followed by \`th pearls list / show\`.
Write verbs (create issue, transition status) aren't wrapped yet — if that's what
you need, this is the case where the override is fine. File a pearl on the smooth
repo so the next person doesn't have to curl."
    exit 1
fi

# --- gh secret set with stdin echo (newline corruption — SMOODEV-879/909) -------
if echo "$CMD" | grep -qE '(echo|printf)[^|]+\|\s*gh\s+secret\s+set'; then
    emit "gh secret set with stdin echo — trailing-newline footgun" \
        "Use scripts/secret-helpers/gh-secret-set instead. The echo/printf pipeline
stores \"value\\n\" and silently breaks byte-comparing consumers (OAuth client_secret,
argon2 hashes — SMOODEV-879 burned us twice). The wrapper strips trailing whitespace
and refuses empty or mid-string newlines."
    exit 1
fi

# --- pnpm sst secret list (plaintext leakage — SMOODEV-908) ---------------------
if echo "$CMD" | grep -qE 'pnpm\s+sst\s+secret\s+list' && ! echo "$CMD" | grep -q 'sst-secret-list'; then
    emit "raw \`pnpm sst secret list\` leaks every secret as plaintext" \
        "Use scripts/secret-helpers/sst-secret-list --stage <env> instead. It redacts
values by default; pass --reveal only when you need them. The raw command prints
Name=value pairs that leak hard in screenshares / Slack / transcripts (SMOODEV-908)."
    exit 1
fi

exit 0
