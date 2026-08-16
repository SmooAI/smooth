---
name: th-mail
description: Bring this Claude Code session online as a `th` agent and listen for agent-to-agent mail in the BACKGROUND while you keep doing other work — surfacing (and, when you can, responding to) messages as they arrive. Uses the `th msg` / `th agent` system (harness-agnostic, machine-level SQLite mailbox). Invoke as `/th-mail` (start listening), `/th-mail send <to> <body>`, `/th-mail status`, or `/th-mail stop`. Use when the user wants you to be reachable by / coordinate with other agents while working.
---

# th-mail — be reachable by other agents while you work

`th` ships a harness-agnostic agent mailbox (`th agent` registry + `th msg` mail). Since pearl th-374f85 it lives in **one SQLite file per machine** (`~/.smooth/mail.db`), not the per-repo Dolt pearl store — so every agent on this host shares one mailbox no matter which repo or worktree it is standing in, sends are instant local writes, and concurrent agents no longer wedge each other. This skill registers the current session as a named agent and runs a **background watcher** so you can keep working and still pick up mail the moment it arrives.

**The core mechanism:** `th msg watch --once --json` blocks until unread mail appears, prints it, and **exits**. A Claude Code background Bash task re-invokes you when it exits — so an arriving message *pulls you back in* without busy-polling. You handle it, **re-arm the watcher**, and return to whatever you were doing. Listening is ancillary; it must never block your primary work.

**Start by finding out who you are — never assume a handle.** `th agent whoami` prints it, resolved as `$SMOOTH_AGENT_HANDLE` → `$SMOOTH_AGENT` → the handle the SessionStart hook recorded for this session → `user@host`. Every `th msg` / `th agent` command with no `--agent` / `--from` resolves it the same way, so **bare commands read your own mailbox** — you no longer have to thread `--agent` through every call. A handle taken from a doc, an example, or a previous session is a guess, and guessing is how a session reads an empty inbox while its real mail piles up under the name it actually registered (pearl th-fa9f40).

## `/th-mail` or `/th-mail start [handle]` — go online and listen

1. **Read your handle first:** `th agent whoami` (`--json` to parse). The SessionStart hook already registered you, under a placeholder like `cc-<repo>-<sid4>` unless you were launched with one.
2. **Claim a task-meaningful handle:** `th agent claim <handle>` — it renames you and carries your mail across. Use `claim`, never `th agent register --name <something-else>`: registering a different name gives this session a *second* mailbox that nobody watches, which `register` now refuses without `--force`.
3. **Show current mail** so nothing already waiting is missed:
   `th msg inbox`  (surface any unread to the user)
4. **Arm the background watcher** — run with `run_in_background: true`:
   `bash "${CLAUDE_PLUGIN_ROOT}/skills/th-mail/watch-once.sh" <handle> 15`
   (args: handle — pass the one from step 1/2, poll-interval-secs; optional 3rd = max lifetime secs, default 24h)
5. Tell the user: *online as `<handle>`, listening in the background — continuing with <current work>.* Then **go back to your primary task.**

### When the watcher background task completes (you get re-invoked)

1. Read the task's output. `[]` **with exit 0** → timed out, just **re-arm** and continue. A **non-zero exit** means the mail store failed (full disk, unreadable `~/.smooth/mail.db`): your mail state is *unknown*, not empty — say so and fix it rather than re-arming into the same failure.
2. Otherwise, for each message:
   - **Surface it** concisely: from, type, body, thread id.
   - **Triage & respond**: answer what you can (`th msg reply <msg-id> --from <handle> --body "..."`, or `th msg send <sender> "..." --from <handle>`); surface and ask when it needs the user's decision. `th msg thread <id>` gets the full conversation.
3. **Acknowledge:** `th msg ack --all --agent <handle>` (or `th msg ack <id>…`) so you don't re-raise it. Acks are **per-recipient** — acking a broadcast consumes only your copy, never anyone else's.
4. **Re-arm** the watcher and **return to your primary task.**

> Keep doing your real work between mail events. The watcher is a tap on the shoulder, not a foreground loop.

## `/th-mail send <to> <body>` — send / broadcast

`th msg send <to|all> <body...> --from <handle>`. Optional `--type note|request|result|handoff|cancel` (how the recipient should triage it), `--priority N` (higher sorts first in their inbox), `--re <id>` to reply within a thread. `--to`/`--body` flag forms still work.

## `/th-mail status` — who's around / what's waiting

`th agent list` (registered agents, presence, branch, current task) and `th msg inbox --agent <handle>`. Publish your own presence with `th agent status --status working --task "what I'm doing"` so other agents can see whether you're free.

## `/th-mail stop` — go offline

Kill the background watcher task (via the harness's background-task controls), then `th agent offline --name <handle>`.

## Notes & footguns

- **Identity:** the watcher and every `th msg` call must use the **same `--agent <handle>`**, or you'll watch the wrong mailbox. `th agent whoami` tells you what you resolve to right now.
- **`--pull` / `--no-pull` / `--no-push` are dead flags.** They still parse (so old scripts don't break) and print a deprecation note, but they do nothing: the mailbox is machine-local, there is no remote to sync and no Dolt write lock to contend for. The old advice about `Error 1105: database is read only` and avoiding concurrent `--pull` watchers no longer applies.
- **Don't double-arm:** keep exactly one watcher background task alive.
- **`th msg inbox` vs `th inbox`:** this skill is `th msg` (agent-to-agent mail). `th inbox` is the same mailbox for your default handle; operative review gates are a different thing.
- **MCP tools do the same thing without shelling out.** If `th mcp install --harness claude-code` has been run (or you're using this plugin's bundled `smooth` MCP server), `agent_identity` / `agent_status` / `agent_list` / `mail_inbox` / `mail_send` / `mail_ack` are available as tools. They hit the same `~/.smooth/mail.db`. Use whichever is at hand — but the **background watcher** has no MCP equivalent, so `/th-mail` still owns being *pushed* mail rather than polling for it.
- **Cloud sync is optional.** Everything above works with no Smoo account. `th agent backend set cloud` (see `th agent backend status`) moves the mailbox to api.smoo.ai so agents on *different machines* share it; that one is a paid feature with a 14-day trial. Nothing local depends on it.
