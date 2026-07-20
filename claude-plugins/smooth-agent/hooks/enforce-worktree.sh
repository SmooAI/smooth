#!/bin/bash
# smooth-agent plugin — enforce the worktree workflow.
#
# Blocks feature work on the main branch in the MAIN worktree. Runs on
# PreToolUse for Edit, Write, and Bash (git commit). Exit 0 = allow,
# Exit 1 = ask the user, Exit 2 = hard block.
#
# Repo-agnostic: the main worktree, its parent, and the repo name are
# derived from git at runtime (via `git worktree list --porcelain`, whose
# first entry is always the main worktree), so the same script guards
# smooth, smooai, smooblue, and any repo that follows the sibling-worktree
# convention `<parent>/<repo>-<branch>/`. Consolidated from the per-repo
# copies that hardcoded a single path (pearl th-44bace).

# Resolve the MAIN worktree for whichever repo this session is in.
DIR="${CLAUDE_PROJECT_DIR:-$PWD}"
MAIN_WORKTREE=$(git -C "$DIR" worktree list --porcelain 2>/dev/null | awk '/^worktree /{print $2; exit}')
# Not a git repo (or no worktree) → nothing to enforce.
[[ -z "$MAIN_WORKTREE" ]] && exit 0
WORKTREE_PARENT=$(dirname "$MAIN_WORKTREE")
REPO_NAME=$(basename "$MAIN_WORKTREE")
BYPASS_FILE="$MAIN_WORKTREE/.claude/worktree-bypass"

# Session bypass: if the bypass file exists, allow everything.
if [[ -f "$BYPASS_FILE" ]]; then
    exit 0
fi

# Read the event from stdin
INPUT=$(cat)
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
TOOL_INPUT=$(echo "$INPUT" | jq -r '.tool_input // empty' 2>/dev/null)

# Helper: check if a path is inside a feature worktree (not the main worktree)
is_in_worktree() {
    local path="$1"
    if [[ "$path" == "$WORKTREE_PARENT/$REPO_NAME-"* ]]; then
        return 0
    fi
    return 1
}

# For Edit/Write: block source code changes targeting the main worktree
if [[ "$TOOL_NAME" == "Edit" || "$TOOL_NAME" == "Write" ]]; then
    FILE_PATH=$(echo "$TOOL_INPUT" | jq -r '.file_path // empty' 2>/dev/null)
    # Allow if the file is in a feature worktree
    if is_in_worktree "$FILE_PATH"; then
        exit 0
    fi
    # Allow changes to the tracker/config dirs that are not source code:
    # .claude/, .smooth/ (pearl store — gitignored; .beads/ is its dead
    # predecessor, kept for repos that haven't migrated), .changeset/,
    # CLAUDE.md, memory files.
    if [[ "$FILE_PATH" == *"/.claude/"* || "$FILE_PATH" == *"/.smooth/"* || "$FILE_PATH" == *"/.beads/"* || "$FILE_PATH" == *"/.changeset/"* || "$FILE_PATH" == *"CLAUDE.md"* || "$FILE_PATH" == *"/memory/"* ]]; then
        exit 0
    fi
    # Allow edits to files outside this repo entirely
    if [[ "$FILE_PATH" != "$MAIN_WORKTREE/"* ]]; then
        exit 0
    fi
    # Only block if we're actually on main in the main worktree
    BRANCH=$(git -C "$MAIN_WORKTREE" symbolic-ref --short HEAD 2>/dev/null)
    if [[ "$BRANCH" != "main" && "$BRANCH" != "master" ]]; then
        exit 0
    fi
    # Allow edits during an active merge (conflict resolution)
    if [[ -f "$MAIN_WORKTREE/.git/MERGE_HEAD" ]]; then
        exit 0
    fi
    # Ask permission for source code edits on main
    cat >&2 <<EOF
⚠️  You are about to edit source code directly on the main branch of $REPO_NAME.

ASK THE USER: "Should I make this change directly on main, or create a worktree?"

If they say worktree, create one:
  git worktree add ../$REPO_NAME-SMOODEV-XX-short-desc -b SMOODEV-XX-short-desc main
EOF
    exit 1
fi

# For Bash: block git commit on main (but allow merges, pulls, pushes, and worktree commits)
if [[ "$TOOL_NAME" == "Bash" ]]; then
    COMMAND=$(echo "$TOOL_INPUT" | jq -r '.command // empty' 2>/dev/null)

    # Allow if the command targets a worktree via git -C or cd
    if echo "$COMMAND" | grep -qE "git\s+-C\s+.*/$REPO_NAME-"; then
        exit 0
    fi
    if echo "$COMMAND" | grep -qE "cd\s+.*/$REPO_NAME-.*&&.*git\s+commit"; then
        exit 0
    fi

    # Block git commit on main (unless it's a merge --no-ff or we're resolving a merge)
    if echo "$COMMAND" | grep -qE 'git\s+commit' && ! echo "$COMMAND" | grep -q '\-\-no-ff'; then
        # Allow commits during an active merge (conflict resolution)
        if [[ -f "$MAIN_WORKTREE/.git/MERGE_HEAD" ]]; then
            exit 0
        fi
        # Check if we're on main
        BRANCH=$(git -C "$MAIN_WORKTREE" symbolic-ref --short HEAD 2>/dev/null)
        if [[ "$BRANCH" == "main" || "$BRANCH" == "master" ]]; then
            cat >&2 <<EOF
⚠️  You are about to commit directly to the main branch of $REPO_NAME.

ASK THE USER: "Should I commit this directly on main, or use a worktree?"

Commits on main typically happen via merge (git merge BRANCH --no-ff).
EOF
            exit 1
        fi
    fi
fi

exit 0
