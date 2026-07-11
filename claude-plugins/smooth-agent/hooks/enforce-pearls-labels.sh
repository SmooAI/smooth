#!/bin/bash
# Warn when th pearls create is called without labels.
# Runs as PostToolUse on Bash — provides feedback after the command runs.
# Exit 0 = allow (with optional stderr feedback)

TOOL_INPUT="$TOOL_INPUT"

# Only check th pearls create commands
if ! echo "$TOOL_INPUT" | grep -q 'th pearls create'; then
    exit 0
fi

# Check if --add-label was included
if echo "$TOOL_INPUT" | grep -qE '\-l\s|\-\-labels|\-\-add-label'; then
    exit 0
fi

# Warn (exit 0 so it doesn't block, but stderr gives feedback)
echo "WARNING: 'th pearls create' was called without labels. Please add labels: th pearls update <id> -l <labels>. Available: ai, approval, bugfix, config, database, docs, frontend, game, infra, integration, knowledge, marketing, pricing, realtime, sdk, security, setup, sme-review, social-media, testing" >&2
exit 0
