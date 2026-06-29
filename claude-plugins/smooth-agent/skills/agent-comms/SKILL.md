---
name: agent-comms
description: Coordinate with Big Smooth and other agents over th-mail (the th msg / th agent bus) — report status, answer pings, and hand off work. Use when this session is a Big Smooth worker (launched via `th claude run`, with SMOOTH_AGENT_HANDLE set) or whenever you need to reach another agent. Invoke for "message the orchestrator", "tell big smooth", "reply to the agent", "who else is working".
---

# agent-comms — talk to Big Smooth and other agents over th-mail

`th` ships a harness-agnostic agent mailbox: `th agent` (registry) + `th msg`
(mail), backed by the pearl Dolt store and synced over `refs/dolt/data`. When
this session was launched by `th claude run`, the `smooth-agent` SessionStart
hook already **registered** it under the handle in `$SMOOTH_AGENT_HANDLE` (your
session id), so Big Smooth can reach you. Your job is to **send** status and
**answer** pings — not to sit in a foreground poll.

Your handle: `$SMOOTH_AGENT_HANDLE` (fall back to a name you pick if unset).
**Pass `--agent`/`--from <handle>` on every command** — shell env doesn't persist
between Bash calls in this harness, and the default handle is `user@host`, not
yours.

## Report status / hand off to the orchestrator

```bash
th msg send --to big-smooth --from "$SMOOTH_AGENT_HANDLE" --body "done: <what>; pearl <id> closed"
th msg send --to big-smooth --from "$SMOOTH_AGENT_HANDLE" --body "blocked: <why>; need <decision>"
```

Broadcast to everyone with `--to all`. Reply within a thread with `--re <id>`.

## Check for and answer pings

```bash
th msg inbox --agent "$SMOOTH_AGENT_HANDLE"          # local read (no lock contention)
th msg thread <id>                                    # full conversation if needed
th msg reply <msg-id> --from "$SMOOTH_AGENT_HANDLE" --body "…"
th msg inbox --unread --mark-read --agent "$SMOOTH_AGENT_HANDLE"   # mark consumed
```

Check the inbox at natural breakpoints (finishing a step, before going idle).
Answer anything you can from context — a status request, an ack, a coordination
ping. Surface decisions that aren't yours to make to the user instead of
committing on their behalf.

## See who's around

```bash
th agent list                                         # registered agents, most-recent first
```

## Footguns

- **`--pull` writes to the shared Dolt store** and contends on its lock —
  polling with `--pull` every few seconds once wedged *every* agent's mailbox
  (`Error 1105: database is read only`). For same-machine agents the store is
  local, so plain `th msg inbox` already sees new mail with **no** pull. Only
  `--pull` occasionally, for genuinely cross-machine setups.
- **`th msg` (agent mail) ≠ `th inbox`** (operative review gates). Different
  thing.
- One identity: always the same `--agent <handle>`, or you'll watch the wrong
  mailbox.
