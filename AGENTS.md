# Agent Instructions

This project uses **th pearls** for issue tracking. Pearls are Dolt-backed
work items with dependency tracking, labels, and Jira sync.

## File References

<!-- Agents should read these files for full project context -->
- [CLAUDE.md](CLAUDE.md) — Project overview, build commands, coding style, testing requirements
- [CLAUDE.md#6-pearl-tracking](CLAUDE.md#6-pearl-tracking--dolt-backed--jira-integration) — Pearl workflow details
- [CLAUDE.md#8-testing](CLAUDE.md#8-testing--mandatory) — Testing requirements (mandatory)
- [CLAUDE.md#9-landing-the-plane](CLAUDE.md#9-landing-the-plane-session-completion) — Session completion checklist

## Pearl Quick Reference

```bash
th pearls ready                       # Find available work
th pearls show <id>                   # View pearl details + dependencies
th pearls update <id> --status=in_progress  # Claim work
th pearls close <id>                  # Complete work
th pearls create --title="..." --description="..."  # File new work
th pearls list --status=open          # All open pearls
th pearls blocked                     # Show blocked pearls
th pearls push                        # Push pearl data to remote
```

## Rules

- Use `th pearls` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Every session should start with `th pearls ready` to see available work
- Pearl IDs follow the pattern `th-XXXXXX` (6 hex chars)
- Branch names should start with the pearl ID: `th-XXXXXX-short-desc`

## Git Workflow

All feature work MUST happen in a git worktree. Never edit source code or
commit directly on `main`.

```bash
th worktree create th-XXXXXX-short-desc   # Create worktree for a pearl
th worktree list                           # List active worktrees
th worktree merge th-XXXXXX-short-desc     # Merge to main when done
th worktree remove th-XXXXXX-short-desc    # Clean up
```

## Quality Gates

Run before every commit (enforced by git hooks):

```bash
cargo fmt -- --check    # Format check
cargo clippy --workspace --all-targets -- -D warnings  # Lint
cargo test --workspace  # Tests (enforced pre-push)
```

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

1. **Run quality gates** — `cargo fmt -- --check && cargo clippy && cargo test && cargo build`
2. **Close pearls** — `th pearls close <id1> <id2> ...`
3. **File remaining work** — `th pearls create --title="..." --description="..."`
4. **Merge to main** — `cd ~/dev/smooai/smooth && git merge <branch> --no-ff`
5. **Push** — `git push` (MANDATORY — work is NOT complete until this succeeds)
6. **Clean up** — remove worktrees, delete merged branches
7. **Verify** — `git status` must show "up to date with origin"

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing — that leaves work stranded locally
- NEVER say "ready to push when you are" — YOU must push
- If push fails, resolve and retry until it succeeds
