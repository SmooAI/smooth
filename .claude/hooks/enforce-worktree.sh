#!/usr/bin/env bash
# enforce-worktree.sh — Prevents source code edits and commits on main in the main worktree.
# Exit codes: 0 = allow, 1 = ask permission, 2 = hard block
#
# The main worktree (~/dev/smooai/smooth/) must ALWAYS stay on main.
# All feature work goes in worktrees: ~/dev/smooai/smooth-SMOODEV-XX-*/

set -euo pipefail

TOOL_INPUT="${1:-}"

# Get repo root
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo "")"
if [[ -z "$REPO_ROOT" ]]; then
    exit 0
fi

# Get current branch
CURRENT_BRANCH="$(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")"

# Only enforce on main branch
if [[ "$CURRENT_BRANCH" != "main" ]]; then
    exit 0
fi

# Check if this is the main worktree (~/dev/smooai/smooth/)
MAIN_WORKTREE="$HOME/dev/smooai/smooth"
if [[ "$REPO_ROOT" != "$MAIN_WORKTREE" ]]; then
    exit 0
fi

# Check for bypass file
if [[ -f "$REPO_ROOT/.claude/worktree-bypass" ]]; then
    exit 0
fi

# Allow edits to .claude/, .beads/, .changeset/, CLAUDE.md, memory files
if echo "$TOOL_INPUT" | grep -qE '"\.(claude|beads|changeset)/|"CLAUDE\.md"|"README\.md"|memory/'; then
    exit 0
fi

# Allow git merge operations
if [[ -f "$REPO_ROOT/.git/MERGE_HEAD" ]]; then
    exit 0
fi

# Check if this is a git commit (allow on merge)
if echo "$TOOL_INPUT" | grep -qE '"command".*git\s+(commit|merge|pull|push|checkout|branch|worktree|log|status|diff|fetch|remote|tag|stash)'; then
    exit 0
fi

# Check if editing source files
if echo "$TOOL_INPUT" | grep -qE '"file_path".*\.(ts|tsx|js|jsx|json|yaml|yml|sql|sh|css|html|md|mts|mjs)'; then
    # Allow package.json edits at root only during bootstrap
    if echo "$TOOL_INPUT" | grep -qE '"file_path".*smooth/(package\.json|tsconfig\.json|turbo\.json)'; then
        exit 0
    fi

    echo "BLOCKED: You are on the main branch in the main worktree."
    echo "All feature work must happen in a git worktree."
    echo ""
    echo "Create a worktree:"
    echo "  git worktree add ../smooth-SMOODEV-XX-description -b SMOODEV-XX-description main"
    echo ""
    echo "Or create a bypass: touch .claude/worktree-bypass"
    exit 2
fi

exit 0
