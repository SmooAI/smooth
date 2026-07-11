#!/bin/bash
# smooth-agent plugin — SessionStart warning when a session opens in the
# MAIN worktree on the main branch (where feature work is forbidden).
#
# Repo-agnostic: derives the main worktree + repo name from git, so it
# warns correctly in smooth, smooai, smooblue, etc. (pearl th-44bace).
# stdout from a SessionStart hook is injected into the session as context.

DIR="${CLAUDE_PROJECT_DIR:-$PWD}"
MAIN=$(git -C "$DIR" worktree list --porcelain 2>/dev/null | awk '/^worktree /{print $2; exit}')
[[ -z "$MAIN" ]] && exit 0
BRANCH=$(git -C "$MAIN" symbolic-ref --short HEAD 2>/dev/null)
REPO=$(basename "$MAIN")

if [[ ( "$DIR" == "$MAIN" || "$DIR" == "$MAIN/"* ) && ( "$BRANCH" == "main" || "$BRANCH" == "master" ) ]]; then
    echo "⚠️  You are in the MAIN worktree (${MAIN}/) on the ${BRANCH} branch. Do NOT do feature work here. Create a worktree first: git worktree add ../${REPO}-SMOODEV-XX-desc -b SMOODEV-XX-desc ${BRANCH}"
fi
exit 0
