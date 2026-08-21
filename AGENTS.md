# Agent Instructions

All project context, build commands, coding style, testing requirements, and
workflow instructions are in [CLAUDE.md](CLAUDE.md).

<!-- th:agent-messaging:begin -->

## Agent Messaging (`th agent` / `th msg`)

You can talk to every other agent on this machine — other sessions, other
harnesses, other repos — through a shared mailbox at `~/.smooth/mail.db`. It's
all plain `th` calls, so it works the same whether you're Claude Code, opencode,
pi, or a shell loop.

**On session start:**

```bash
th agent whoami                            # who am I already — ALWAYS ask first
th agent claim <your-handle>               # take a stable name, carrying your mail
```

**Continuously check for messages** (do this every few turns, or run it in the
background of your session):

```bash
th msg inbox --unread           # what's waiting for me
th msg watch                    # blocking poll loop — prints messages as they land
th msg watch --once --json      # block until mail arrives, print it, exit
th msg ack --all                # done with them (per-recipient: only your copy)
```

**Send / reply:**

```bash
th agent list                   # who can I reach (presence, branch, current task)
th msg send <name|all> "…" [--type request|result|handoff|cancel] [--priority N]
th msg reply <message-id> --body "…"   # threads automatically
th msg thread <message-id>      # read a whole conversation
th agent status --status working --task "…"   # tell others what you're up to
```

Identity resolves `$SMOOTH_AGENT_HANDLE` → `$SMOOTH_AGENT` → this session's
recorded handle → `user@host`; set `$SMOOTH_HARNESS` so others can see what tool
you are, and `th agent claim <handle>` to take a durable name (your mail comes
with it). The mailbox is machine-local, so there is nothing to push or pull —
`--no-push`/`--pull` still parse but do nothing.
<!-- th:agent-messaging:end -->
