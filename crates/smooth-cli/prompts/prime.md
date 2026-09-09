# Pearls Workflow Context

> Pearl tracking via `th pearls`. One SQLite database per machine
> (`~/.smooth/pearls.db`), rows keyed by project; registry at `~/.smooth/registry.json`.

## 🚨 SESSION CLOSE PROTOCOL 🚨

**CRITICAL**: Before saying "done" or "complete", run this checklist:

```
[ ] 1. git status              (check what changed)
[ ] 2. git add <files>         (stage specific code changes)
[ ] 3. git commit -m "..."     (commit code)
[ ] 4. git push                (push to remote)
[ ] 5. th pearls close <id>    (or `th pearls checkpoint <id> --next "…"` if handing off unfinished work)
```

**NEVER skip this.** Work isn't done until pushed.

## Core Rules

- **Default**: use pearls for ALL task tracking (`th pearls create`,
  `th pearls ready`, `th pearls close`).
- **Prohibited**: do NOT use the TodoWrite tool or ad-hoc markdown
  files for multi-turn task tracking.
- **Workflow**: create pearl BEFORE writing code, mark in_progress
  when starting, checkpoint at milestones, close when work is pushed.
- **Memory**: durable project context lives in CLAUDE.md / AGENTS.md —
  read those before guessing conventions. For cross-session insights
  about you-the-user, auto-memory is loaded automatically.
- Persistence you don't need beats lost context.
- Every write lands in `~/.smooth/pearls.db` immediately; there is
  nothing to commit, push, or pull. `th pearls push` / `pull` only print
  a notice.
- Worktrees share the store: the project is the main checkout
  (`git rev-parse --git-common-dir`), so `th pearls` from any worktree
  sees and edits the same pearls — no need to hop to `main` first.

## Essential Commands

### Finding work

- `th pearls ready` — issues ready to work (no blockers)
- `th pearls list --status=open` — all open
- `th pearls list --status=in_progress` — your active work
- `th pearls show <id>` — full view with deps + history

### Creating & updating

- `th pearls create --title="Summary" --description="Why this exists and what needs doing" --type=task|bug|feature --priority=2`
    - Priority: 0–4 (0 = critical, 2 = medium, 4 = backlog). Not "high"/"low".
- `th pearls update <id> --status=in_progress` — claim work
- `th pearls update <id> --title/--description/--priority/--assign` — edit fields
- `th pearls close <id>` — mark complete
- `th pearls close <id1> <id2> ...` — batch close
- When creating many related pearls, run the creates in parallel.
- **WARNING**: avoid interactive editor flows (`th pearls edit`) — they
  block the agent on $EDITOR.

### Checkpoints & handoff (compaction-proof)

- `th pearls checkpoint <id> --note "what happened" --next "what to do first"` —
  at every milestone (tests green, PR open, blocked). Records the worktree,
  branch, HEAD, dirty files and session automatically; notes append, the
  handoff state is latest-wins.
- `th pearls show <id> --handoff [--json]` — resume one pearl: where the
  work is, what happened, what's next.
- `th pearls prime --in-progress [--cwd .]` — handoff packets for every
  in-progress pearl (only this worktree's with `--cwd`).

### Dependencies & blocking

- `th pearls dep add <issue> <depends-on>` — issue depends on depends-on
- `th pearls blocked` — show blocked issues
- `th pearls show <id>` — see what's blocking / blocked by

### Search & stats

- `th pearls search <query>` — full-text
- `th pearls stats` — project counts

### Team sync

- `th pearls sync [--project <KEY>] [--dry-run]` — reconcile this repo's
  pearls with Smoo Projects work items (pearl th-19cca5). Bind the repo
  once with `--project`; it is explicit and offline-first, never automatic.

## Common Workflows

**Starting work:**

```bash
th pearls ready
th pearls show <id>                 # add --handoff to resume someone's checkpoint
th pearls update <id> --status=in_progress
```

**Completing work:**

```bash
git add . && git commit -m "Pearl th-XXXX: ..."
git push
th pearls close <id1> <id2> ...
```

**Handing off unfinished work:**

```bash
th pearls checkpoint <id> --note "tests green, PR #123 open" --next "address review, then merge"
```

**Spawning dependent pearls:**

```bash
# Creates can run in parallel
th pearls create --title="Implement feature X" --type=feature
th pearls create --title="Write tests for X" --type=task
th pearls dep add <tests-id> <feature-id>   # tests depend on feature
```

## Project Context

- Workflow rules and commands specific to this project live in
  `CLAUDE.md` and `AGENTS.md` at the repo root — read those before
  making assumptions.
