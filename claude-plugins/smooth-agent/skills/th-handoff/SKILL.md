---
name: th-handoff
description: Hand this agent session's work (Claude Code, Codex, OpenCode or th code) off to a fresh session without losing anything. Writes a durable handoff into a pearl (`th pearls checkpoint`), makes every live worker (subagent or other session) push its unpushed work and checkpoint the same pearl, re-reads the result, and tells the user the exact prompt to resume with. Invoke as `/th-handoff` (new handoff pearl), `/th-handoff <pearl-id>` (append to an existing one), or `/th-handoff resume <pearl-id>` (the receiving side). Use when the user says "hand off", "new session", "context is full", "switch accounts", or "pick this up later".
---

# th-handoff — move a session's work to a new session

A session ends in three ways that lose work: context runs out, the account switches, or the user closes the terminal. Workers that live inside the session die with it. Uncommitted edits stay stranded in worktrees nobody remembers. The plan lives only in scrollback.

A handoff fixes all three. It writes the state into a **pearl** (one machine-global SQLite store, readable from any worktree and any later session), gets every worker to **push** before the session goes away, and leaves the user **one line** to paste into the next session.

`th pearls checkpoint` is the primitive. It records a note (appends) plus auto-collected state: worktree, branch, HEAD, dirty files, session id, next step (latest wins). `th pearls show <id> --handoff` renders it back. This skill is the discipline around that primitive.

## `/th-handoff [pearl-id]` — hand off

### 1. Pick the pearl

- If the user named a pearl, or the work already has one (`th pearls list --status=in_progress`), use it.
- Otherwise create one and mark it in progress:
  `th pearls create --type=task --priority=1 --title="HANDOFF: <work in a few words>" --description="Handoff from session <id> (<date>). Resume: th pearls show <pearl> --handoff."`
  `th pearls update <pearl> --status=in_progress`
- One pearl per handoff. Workers append to it; they don't make their own.

### 2. Gather the facts first. Do not write from memory.

Check live state, because the conversation is already stale:

- `git worktree list` and `git status --short` in each worktree you touched. Dirty or unpushed work is the thing a handoff exists to save.
- Every open PR you own or are shepherding: `gh pr view <n> --json state,mergeable,headRefOid`, and `gh pr checks <n>`.
- Your mail identity and unread mail: `th agent whoami`, `th msg inbox`. Anything unread goes into the note or gets answered now.
- Running background tasks and live workers (see [Workers, per harness](#workers-per-harness)). Anything in-process stops when this session ends.

### 3. Get workers to push, before you write the final note

For each live worker (in-process subagent, or another session via `th msg send` / `th flow send`):

> "This session is being handed off. Commit and push your branch now, even if CI isn't green yet, and don't push throwaway or copied files. Then run `th pearls checkpoint <pearl> --note '<your state, the PR, what's left, in order>'` and reply when pushed."

Wait for each reply, then verify with `git -C <worktree> status` and `git log origin/<branch>..HEAD`. A worker that can't push (permission denial, failing hook) is a blocker: put its worktree path and branch in the note so the next session can finish it. **Never discard a worker's uncommitted changes to tidy up.**

### 4. Write the handoff note

Use `th pearls checkpoint <pearl> --note "<NOTE>" --next "<first command the next session runs>"`. Build the note in a quoted heredoc (`NOTE=$(cat <<'EOF' … EOF)`). Never put backticks inside a double-quoted string, because the shell executes them.

The note has these sections, in this order, with facts and no narration:

1. **Done**: what merged or shipped, with PR numbers and how it was verified (prod check, CI on the head SHA). Don't list what only "should" work.
2. **Open, in order**: each remaining item with its PR, branch, owner, exact state, the next action, and the condition that gates it. Include any ordering rules (merge queues, migration slots, stacked PRs).
3. **Blocked on the user**: logins, approvals, manual console steps, flags only they may flip. Make each one copy-pasteable.
4. **Rules of the road**: approvals the user has already given in this session that the next session may rely on, quoted with their scope ("approved merging green PRs in this epic"). Never widen them. Also include coordination contacts, such as who issues "go" in a queue and which mail handle you used.
5. **Where things are**: worktree paths, artifacts (builds, files), follow-up tickets.
6. **What does NOT carry over**: name the in-process workers that die with this session, and say whether the next session should respawn them or redo their remaining steps itself. Worker sessions in SmoothFlow or another terminal survive; give their handles instead.

### 5. Read it back, then tell the user

Run `th pearls show <pearl> --handoff` and read the output. If the next session would get it wrong from that text alone, fix the note.

Then give the user:

- the pearl id;
- the exact line to paste into a new session: `Resume from th pearls show <pearl> --handoff and continue.` (or `/th-handoff resume <pearl>`);
- anything they must do before or right after switching, such as an expired login;
- whether it is safe to close this session yet, which it isn't until every worker has confirmed its push.

## `/th-handoff resume <pearl-id>` — pick up

1. `th pearls show <pearl> --handoff` and read all of it, including the notes that workers appended.
2. **Re-verify before acting.** The note is a snapshot. Re-check every PR, branch and worktree it names (`gh pr view`, `git status`); things merged, conflicted or moved while no one was watching.
3. Claim the mail identity the note names (`th agent claim <handle>`) if it was used for coordination, read `th msg inbox`, and arm `/th-mail`.
4. Re-establish nothing on trust. Approvals carry over only as they were quoted in the note. If a step needs a permission this session lacks, ask the user; don't route it through another agent.
5. Respawn workers for the open items, or do them yourself, then `th pearls checkpoint <pearl> --note "resumed by <session>; <what changed>"`.
6. Close the pearl (`th pearls close <pearl>`) when the last open item lands.

## Workers, per harness

The core of this skill is `th pearls`, `th msg`, `th agent`, `th flow`, git and gh, so it runs the same in every harness. Only how you reach a worker differs:

- **In Claude Code**: subagents started with the Agent tool are in-process. Reach them with SendMessage. They die with this session, so they must push before you hand off.
- **In Codex, OpenCode and th code**: there are no in-process subagents. Workers are other sessions: reach them with `th msg send <handle>` (find them with `th agent list`) or, for SmoothFlow sessions, `th flow send <id>`. They outlive this session, so record their handles in the note.

## Don'ts

- Don't write the handoff before the workers push. A note that points at unpushed work is a note pointing at nothing.
- Don't paste secrets, tokens or credential values into the note. Name the config key and say where it lives.
- Don't create one pearl per worker. One handoff pearl; workers append to it.
- Don't summarise "everything is basically done". List exactly what is open and what gates it.
