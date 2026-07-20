#!/bin/bash
# Warn when `th pearls create` is called without a label.
# Runs as PostToolUse on Bash — feedback after the command has already run.
# Exit 0 always (advisory only); stderr is surfaced to the agent.
#
# Hook input arrives as JSON on STDIN. The previous version read a
# `$TOOL_INPUT` environment variable that Claude Code never sets, so the
# grep matched nothing and the hook was a permanent no-op.

INPUT=$(cat)
CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)
# No jq / no command → nothing to advise on.
[[ -z "$CMD" ]] && exit 0

# Only check `th pearls create`.
echo "$CMD" | grep -q 'th pearls create' || exit 0

# `th pearls create` takes `--label <LABEL>` (singular). Already labelled → done.
echo "$CMD" | grep -qE '\-\-label(=|\s)' && exit 0

cat >&2 <<'EOF'
WARNING: `th pearls create` was called without a label. Label it now:
  th pearls label <id> add <label>
(or pass `--label <label>` at create time — note it is singular, not `--labels`.)
Available: ai, approval, bugfix, config, database, docs, frontend, game, infra,
integration, knowledge, marketing, pricing, realtime, sdk, security, setup,
sme-review, social-media, testing
EOF
exit 0
