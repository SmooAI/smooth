#!/bin/bash
# smooth-agent plugin — enforce the worktree workflow.
#
# Blocks feature work on the main branch in the MAIN worktree. Runs on
# PreToolUse for Edit, Write, and Bash (git commit + shell-based file edits).
#
# EXIT CODES ARE THE WHOLE POINT: in Claude Code PreToolUse, ONLY exit 2 blocks
# the tool call. Every other non-zero exit is treated as a non-blocking hook
# error and the tool call PROCEEDS. This script shipped with `exit 1` on both
# deny paths, so it never blocked anything — 18 transcripts fired it and zero
# acted on it (the same bug attest-push-hint.sh already fixed). Do not "clean
# up" these to exit 1.
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
    # Block source code edits on main
    cat >&2 <<EOF
⚠️  BLOCKED: source code edit directly on the main branch of $REPO_NAME.

Create a worktree and redo the edit there:
  git worktree add ../$REPO_NAME-SMOODEV-XX-short-desc -b SMOODEV-XX-short-desc main

If the user explicitly wants this on main, they can bypass with:
  touch $BYPASS_FILE
EOF
    exit 2
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
⚠️  BLOCKED: commit directly to the main branch of $REPO_NAME.

Commits on main happen via merge (git merge BRANCH --no-ff). Do the work in a
worktree and commit there instead.
EOF
            exit 2
        fi
    fi

    # Shell-based edits to tracked files: `cat > f`, `sed -i`, `tee`, `python -c
    # open(...,'w')`, `rm`, `mv`. The Edit/Write arm never sees these, so main
    # stayed editable by simply spelling the edit as a shell command.
    #
    # Precision over breadth: we only block when a MUTATION TARGET (a redirect
    # destination, or an argument of an in-place/destructive command) is a file
    # git actually tracks in the main worktree. Scanning every token instead
    # would block `grep foo src/main.rs > /tmp/out`, which only reads.
    # A relative path is only resolved when the session itself is in the main
    # worktree — otherwise a feature-worktree session's `sed -i src/foo.rs`
    # would match the same tracked path and be blocked wrongly.
    TARGETS=$(
        echo "$COMMAND" | grep -oE '>>?[[:space:]]*[^[:space:]|&;)]+' | sed -E 's/^>>?[[:space:]]*//'
        echo "$COMMAND" | grep -oE '(^|[[:space:];&|])(sed|perl|ruby)[^;&|]*[[:space:]]-i[^;&|]*' | tr -c 'A-Za-z0-9_./-' ' '
        echo "$COMMAND" | grep -oE '(^|[[:space:];&|])(tee|truncate|dd|rm|mv)[[:space:]][^;&|]*' | tr -c 'A-Za-z0-9_./-' ' '
    )
    if [[ -n "$TARGETS" ]]; then
        BRANCH=$(git -C "$MAIN_WORKTREE" symbolic-ref --short HEAD 2>/dev/null)
        if [[ "$BRANCH" == "main" || "$BRANCH" == "master" ]] && [[ ! -f "$MAIN_WORKTREE/.git/MERGE_HEAD" ]]; then
            SESSION_IN_WORKTREE=0
            is_in_worktree "$DIR" && SESSION_IN_WORKTREE=1
            for TOKEN in $TARGETS; do
                case "$TOKEN" in
                    "$MAIN_WORKTREE"/*) REL="${TOKEN#"$MAIN_WORKTREE"/}" ;;
                    /*) continue ;;
                    *) [[ "$SESSION_IN_WORKTREE" == 1 ]] && continue; REL="$TOKEN" ;;
                esac
                # Tracker/config dirs are exempt, same as the Edit/Write arm.
                case "$REL" in
                    .claude/* | */.claude/* | .smooth/* | */.smooth/* | .beads/* | */.beads/* | .changeset/* | */.changeset/* | *CLAUDE.md | */memory/*) continue ;;
                esac
                git -C "$MAIN_WORKTREE" ls-files --error-unmatch -- "$REL" >/dev/null 2>&1 || continue
                cat >&2 <<EOF
⚠️  BLOCKED: this shell command edits '$REL', a tracked file on the main branch
of $REPO_NAME. Shell edits are source edits — the worktree rule applies.

  git worktree add ../$REPO_NAME-SMOODEV-XX-short-desc -b SMOODEV-XX-short-desc main
EOF
                exit 2
            done
        fi
    fi
fi

exit 0
