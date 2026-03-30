#!/bin/bash
# Warn when bd create is called without labels.
# Runs as PostToolUse on Bash — provides feedback after the command runs.
# Exit 0 = allow (with optional stderr feedback)

TOOL_INPUT="$TOOL_INPUT"

# Only check bd create commands
if ! echo "$TOOL_INPUT" | grep -q 'bd create'; then
    exit 0
fi

# Check if labels were included (-l or --labels)
if echo "$TOOL_INPUT" | grep -qE '\-l\s|\-\-labels'; then
    exit 0
fi

# Warn (exit 0 so it doesn't block, but stderr gives feedback)
echo "WARNING: 'bd create' was called without labels. Please add labels: bd update <id> -l <labels>. Available: backend, cli, db, frontend, hooks, infra, leader, operator, security, testing, tools, tui, web, websocket" >&2
exit 0
