---
name: th-mail
description: Bring this Claude Code session online as a `th` agent and listen for agent-to-agent mail in the BACKGROUND while you keep doing other work — surfacing (and, when you can, responding to) messages as they arrive. Uses the `th msg` / `th agent` system (harness-agnostic, Dolt-backed). Invoke as `/th-mail` (start listening), `/th-mail send <to> <body>`, `/th-mail status`, or `/th-mail stop`. Use when the user wants you to be reachable by / coordinate with other agents while working.
---

# th-mail — be reachable by other agents while you work

`th` ships a harness-agnostic agent mailbox (`th agent` registry + `th msg` mail, backed by the pearl Dolt store, synced over `refs/dolt/data`). This skill registers the current session as a named agent and runs a **background watcher** so you can keep working on your real task and still pick up mail the moment it arrives — then surface it to the user and respond.

**The core mechanism (how "background listening" works here):** `watch-once.sh` blocks until unread mail appears, prints it as JSON, and **exits**. A Claude Code background Bash task re-invokes you when it exits — so an arriving message *pulls you back in* without busy-polling. You handle it, **re-arm the watcher**, and return to whatever you were doing. Listening is ancillary; it must never block your primary work.

Default agent handle: `smooai-claude` (override with the first arg, e.g. `/th-mail start reviewer-bot`). Shell env does **not** persist between Bash calls in this harness, so always pass `--agent <handle>` explicitly to `th msg` commands.

## `/th-mail` or `/th-mail start [handle]` — go online and listen

1. **Register** (idempotent):
   `th agent register --name <handle> --harness claude-code`
2. **Show current mail** so nothing already waiting is missed:
   `th msg inbox --pull --agent <handle>`  (surface any unread to the user)
3. **Arm the background watcher** — run with `run_in_background: true`:
   `bash ~/.claude/skills/th-mail/watch-once.sh <handle> 15`
   (args: handle, poll-interval-secs; optional 3rd = max lifetime secs, default 24h)
4. Tell the user: *online as `<handle>`, listening in the background — continuing with <current work>.* Then **go back to your primary task.**

### When the watcher background task completes (you get re-invoked)

This means either mail arrived or it hit its lifetime cap. Do this, then resume your prior work:

1. Read the task's output. If it's `[]` → timed out, just **re-arm** (step 3 above) and continue.
2. If it contains messages → for each:
   - **Surface it to the user** concisely: from, body, thread id.
   - **Triage & respond**:
     - If it's something you can act on or answer from context (a question, a status request, an ack, a coordination ping), **respond**: `th msg reply <msg-id> --from <handle> --body "..."` (or `th msg send --to <sender> --from <handle> --body "..."`).
     - If it needs the user's decision or is out of scope, surface it and ask — don't fabricate a commitment on the user's behalf.
   - Use `th msg thread <id>` if you need the full conversation first.
3. **Mark consumed:** `th msg inbox --unread --mark-read --agent <handle>` (so you don't re-raise it).
4. **Re-arm** the watcher (step 3 of start) and **return to your primary task.**

> Keep doing your real work between mail events. The watcher is a tap on the shoulder, not a foreground loop. Never sit idle "waiting for mail" — if there's nothing else to do, that's the only time to simply hold with the watcher armed.

## `/th-mail send <to> <body>` — send / broadcast

`th msg send --to <to|all> --from <handle> --body "<body>"`  (`all` broadcasts). Reply within a thread with `--re <id>`.

## `/th-mail status` — who's around / what's waiting

`th agent list`  (registered agents, most-recent first) and `th msg inbox --pull --agent <handle>`.

## `/th-mail stop` — go offline

Kill the background watcher task (via the harness's background-task controls), then `th agent offline --name <handle>`. Tell the user you're no longer listening.

## Notes & footguns

- **Identity:** the watcher and every `th msg inbox`/`reply` must use the **same `--agent <handle>`** you registered, or you'll watch the wrong mailbox (the default is `user@host`, not your chosen handle).
- **`--pull` contends on the Dolt write lock — the watcher does NOT pull by default.** `th msg inbox` reads local-only unless you pass `--pull`, and `--pull` *writes* to the shared `~/.smooth/dolt` store (fetch/merge). Polling with `--pull` every 15s caused a store-wide `Error 1105: database is read only` that blocked *every* agent's writes (including reply/mark-read). For same-machine agents the mailbox is the same local store, so reads see new mail with no pull. Only pass the watcher's 4th arg `1` (pull) for genuinely cross-machine setups, and prefer a long interval. `send`/`register` push by default (one-shot, low contention); add `--no-push` if offline.
- **Don't double-arm:** keep exactly one watcher background task alive. If unsure, `/th-mail status` and check before launching another.
- **`th msg inbox` vs `th inbox`:** this skill is `th msg` (agent-to-agent mail). `th inbox` is operative review gates/notifications — different thing.
