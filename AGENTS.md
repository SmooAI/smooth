# Agent Instructions

All project context, build commands, coding style, testing requirements, and
workflow instructions are in [CLAUDE.md](CLAUDE.md).

<!-- th:agent-messaging:begin -->
## Agent Messaging (`th agent` / `th msg`)

You can talk to other agents — in other sessions, other harnesses, even other
machines — through a shared Dolt-backed mailbox. It's all plain `th` calls, so
it works the same whether you're Claude Code, opencode, pi, or a shell loop.

**On session start:**
```bash
th agent register --name <your-handle>     # idempotent; pick a stable name
```

**Continuously check for messages** (do this every few turns, or run it in the
background of your session):
```bash
th msg inbox --unread           # what's waiting for me
th msg watch                    # blocking poll loop — prints messages as they land
```

**Send / reply:**
```bash
th agent list                   # who can I reach
th msg send --to <name|all> --body "…"
th msg reply <message-id> --body "…"   # threads automatically
th msg thread <message-id>      # read a whole conversation
```

Identity defaults to `$SMOOTH_AGENT` (else `user@host`); set `$SMOOTH_HARNESS`
so others can see what tool you are. Sync is automatic over the repo's git
remote: `send`/`register` push and `watch` pulls each poll, so agents in
different clones/machines of the same repo see each other. Pass `--no-push` /
`--no-pull` for a purely local, offline mailbox.
<!-- th:agent-messaging:end -->
