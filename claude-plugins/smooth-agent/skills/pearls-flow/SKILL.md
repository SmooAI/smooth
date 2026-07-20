---
name: pearls-flow
description: Track work as pearls (th pearls) — the dependency-graph work tracker shared across smooth/smooai. Create a pearl before starting work, claim it, close it when pushed. Use whenever you start a unit of work, are asked what to work on, or finish a task. Invoke for "track this", "what's ready", "file a pearl", "close it out".
---

# pearls-flow — track work as pearls

`th pearls` is the work tracker (Dolt-backed, dependency-aware) used across the
SmooAI repos. As a Big Smooth worker, wrap each unit of work in a pearl so the
orchestrator and teammates can see what's in flight and what's done.

## The loop

```bash
th pearls ready                              # what's ready (open, no blockers)
th pearls show <id>                          # details + dependencies + history
th pearls update <id> --status=in_progress   # claim it before you start
# … do the work …
th pearls close <id>                         # when the work is committed/pushed
```

## Create work

```bash
th pearls create --title="<summary>" --description="<why + what>" --type=task|bug|feature --priority=2 --label=<label>
```

Priority is **0–4** (0 = critical, 1 = high, 2 = medium, 3 = low, 4 = backlog) —
not "high"/"low". `--label` is **singular** — one label per flag. To label an
existing pearl use `th pearls label <id> add <label>`; `th pearls update` has no
label flag. Add dependencies with `th pearls dep add <issue> <depends-on>`.

## Rules

- **Create the pearl before writing code**; mark `in_progress` when you start;
  close when pushed. Work isn't done until it's committed and pushed.
- Don't use ad-hoc TODO lists for multi-step work — pearls are the tracker.
- When you finish, report to the orchestrator over th-mail (see the
  `agent-comms` skill): `th msg send --to big-smooth --from "$SMOOTH_AGENT_HANDLE"
  --body "closed pearl <id>: <what>"`.
- Every flag above is non-interactive by design — pass `--title` / `--description`
  / `--status` rather than looking for an `$EDITOR` flow (there isn't one).
- `th pearls prime` prints open/in-progress pearls plus recent project memories —
  load it at session start to pick up where the last session left off.
