---
name: th-clear
description: Hand this session off and continue in a fresh context in the SAME terminal — the th-handoff checkpoint, then `th harness handoff arm`, then the user types `/clear` and the next session resumes automatically (no resume command). Invoke as `/th-clear [pearl-id]`. Use when the context budget hook says the session is over budget, or the user says "clear and continue", "fresh context", "th-clear", or "context is getting big".
---

# th-clear — hand off, clear, keep going

`/th-handoff` moves work to a session the user starts by hand. `/th-clear` is the same handoff for the common case: **this terminal, fresh context, keep going.** You can't clear your own context — only the user's `/clear` can — so this skill does everything up to that keystroke and arms a one-shot resume. The next session's SessionStart hook (`th harness handoff claim`) finds it and injects the handoff. A `/clear` with nothing armed stays a plain `/clear`.

The `th harness budget` hook invokes this for you when the session crosses `harness.context_budget` (default 250k tokens; `th settings set harness.context_budget <n>`, 0 disables).

## Steps

1. **Pick the pearl.** The user-named one; else the in-progress pearl this work is on (`th pearls list --status=in_progress`, the id in the branch name); else create one exactly as `/th-handoff` step 1 does. Several pearls in flight → arm each (`--pearl` repeats, max 3).

2. **Do the th-handoff steps 2–4** (gather facts, get workers to push, write the note). Same discipline, same note sections. Be brief where the work is simple; the next session is you with an empty context, so `--next` must be the exact first action.
   `th pearls checkpoint <pearl> --note "$NOTE" --next "<first action>"`
   Build `NOTE` in a quoted heredoc (`NOTE=$(cat <<'EOF' … EOF)`) — never backticks in a double-quoted string.

3. **Read it back:** `th pearls show <pearl> --handoff`. If the fresh session would get it wrong from that text alone, checkpoint again.

4. **Arm:** `th harness handoff arm --pearl <pearl> --harness claude-code` (in Codex/OpenCode/others, pass that harness's name). It warns if the pearl has no checkpoint — fix that, don't ignore it. The token expires in 15 minutes.

5. **Stop and tell the user, in one line:** `Armed <pearl>. Type /clear, then any message (e.g. "go") — the fresh session picks up from: <next>.` Then end your turn. Do no more work: anything after the checkpoint is lost to the next session.

## Notes

- **In-process subagents die on `/clear`.** They must push and checkpoint before step 4, exactly as in `/th-handoff`.
- **Why "then any message":** a SessionStart hook can add context but can't start a turn; the first message after `/clear` is what sets the fresh session working.
- **Changed your mind?** `th harness handoff disarm` drops the token; `th harness handoff list` shows what is armed.
- **Relaunch instead of `/clear`** (quit, then `claude` in the same directory within 15 minutes) also resumes, when exactly one token is armed there.
- **Other harnesses:** the same two commands work anywhere; the harness's clear is `/clear` or `/new`. Where no SessionStart hook claims the token, the fresh session can claim by hand: `th harness handoff claim --format text`.
