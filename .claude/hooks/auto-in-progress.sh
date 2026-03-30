#!/usr/bin/env bash
# auto-in-progress.sh — Auto-transitions beads to in_progress when starting a session
# in a feature worktree. SessionStart hook.

set -euo pipefail

# Get current branch
CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")"
if [[ -z "$CURRENT_BRANCH" ]]; then
    exit 0
fi

# Check if on main — warn user
if [[ "$CURRENT_BRANCH" == "main" ]]; then
    REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo "")"
    MAIN_WORKTREE="$HOME/dev/smooai/smooth"
    if [[ "$REPO_ROOT" == "$MAIN_WORKTREE" ]]; then
        echo "INFO: You are in the main worktree on main."
        echo "For feature work, create a worktree:"
        echo "  git worktree add ../smooth-SMOODEV-XX-desc -b SMOODEV-XX-desc main"
    fi
    exit 0
fi

# Extract SMOODEV key from branch name
SMOODEV_KEY=$(echo "$CURRENT_BRANCH" | grep -oE 'SMOODEV-[0-9]+' | head -1)
if [[ -z "$SMOODEV_KEY" ]]; then
    exit 0
fi

# Check if bd is available
if ! command -v bd &>/dev/null; then
    exit 0
fi

# Find matching bead and transition to in_progress
MATCHING_BEAD=$(bd list --status=open --json 2>/dev/null | grep -o "\"id\":\"[^\"]*$SMOODEV_KEY[^\"]*\"" | head -1 | grep -o '"[^"]*"$' | tr -d '"' || echo "")
if [[ -n "$MATCHING_BEAD" ]]; then
    bd update "$MATCHING_BEAD" --status=in_progress 2>/dev/null || true
    echo "INFO: Transitioned bead $MATCHING_BEAD to in_progress"
fi

exit 0
