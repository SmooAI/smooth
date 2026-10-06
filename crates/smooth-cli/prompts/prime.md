# Pearls Workflow Context

Track ALL multi-turn work as pearls (`th pearls`), not TodoWrite or markdown
files. One SQLite store per machine (`~/.smooth/pearls.db`), shared by every
worktree; writes land immediately, nothing to push or pull.

**Close protocol** — work isn't done until pushed: commit → push →
`th pearls close <id>` (or `th pearls checkpoint <id> --note "…" --next "…"`
to hand off unfinished work).

- Create a pearl BEFORE writing code; `update <id> --status=in_progress` to
  claim; checkpoint at milestones (tests green, PR open, blocked).
- `create --title=… --description=… --type=task|bug|feature --priority=0-4`
  (0 = critical, 4 = backlog). Avoid `th pearls edit` (blocks on $EDITOR).
- Find: `ready`, `list --status=open|in_progress`, `show <id> [--handoff]`,
  `search <q>`, `blocked`. Deps: `dep add <issue> <depends-on>`.
- Resume: `th pearls prime --in-progress --cwd .`. Team sync: `th pearls sync`.
- Project rules live in `CLAUDE.md` / `AGENTS.md` — read them first.
