# Pearls Workflow Context

> Pearl tracking via `th pearls`. Dolt-backed per-project, optional
> global registry at `~/.smooth/registry.json`.

## 🚨 SESSION CLOSE PROTOCOL 🚨

**CRITICAL**: Before saying "done" or "complete", run this checklist:

```
[ ] 1. git status              (check what changed)
[ ] 2. git add <files>         (stage specific code changes)
[ ] 3. git commit -m "..."     (commit code)
[ ] 4. git push                (push to remote)
```

**NEVER skip this.** Work isn't done until pushed.

## Core Rules

- **Default**: use pearls for ALL task tracking (`th pearls create`,
  `th pearls ready`, `th pearls close`).
- **Prohibited**: do NOT use the TodoWrite tool or ad-hoc markdown
  files for multi-turn task tracking.
- **Workflow**: create pearl BEFORE writing code, mark in_progress
  when starting, close when work is pushed.
- **Memory**: durable project context lives in CLAUDE.md / AGENTS.md —
  read those before guessing conventions. For cross-session insights
  about you-the-user, auto-memory is loaded automatically.
- Persistence you don't need beats lost context.
- Data is auto-committed to the Dolt pearls DB (`.smooth/dolt/`).
  Run `th pearls push` at session end so teammates can pull.

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
- `th pearls update <id> --title/--description/--notes` — edit fields
- `th pearls close <id>` — mark complete
- `th pearls close <id1> <id2> ...` — batch close
- When creating many related pearls, run the creates in parallel.
- **WARNING**: avoid interactive editor flows (`th pearls edit`) — they
  block the agent on $EDITOR.

### Dependencies & blocking
- `th pearls dep add <issue> <depends-on>` — issue depends on depends-on
- `th pearls blocked` — show blocked issues
- `th pearls show <id>` — see what's blocking / blocked by

### Sync
- `th pearls push` / `th pearls pull` — push or pull the Dolt DB
- `th pearls search <query>` — full-text
- `th pearls stats` — project counts

## Common Workflows

**Starting work:**
```bash
th pearls ready
th pearls show <id>
th pearls update <id> --status=in_progress
```

**Completing work:**
```bash
th pearls close <id1> <id2> ...
git add . && git commit -m "Pearl th-XXXX: ..."
git push
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
