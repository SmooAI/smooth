---
name: th-clear
description: Hand this session off and continue in a fresh context in the SAME terminal — the th-handoff checkpoint, then `th harness handoff arm`, then the user types `/clear` and `/th-handoff resume <pearl>` (the armed token usually resumes on its own). Invoke as `/th-clear [pearl-id]`. Use when the user accepts the budget hook's /th-clear suggestion, when the hook says the session did not auto-compact, or the user says "clear and continue", "fresh context", "th-clear", or "context is getting big".
---

# th-clear — hand off, clear, keep going

`/th-handoff` moves work to a session the user starts by hand. `/th-clear` is the same handoff for the common case: **this terminal, fresh context, keep going.** You can't clear your own context — only the user's `/clear` can — so this skill does everything up to that keystroke and arms a one-shot resume. The next session's SessionStart hook (`th harness handoff claim`) finds it and injects the handoff. A `/clear` with nothing armed stays a plain `/clear`.

**When it comes up.** The `th harness budget` hook never blocks. Past `harness.context_suggest` (150k), each milestone — a commit, push, `th attest`, PR, pearl checkpoint, or green test run — asks you to add one line to your report suggesting `/th-clear`; do that, and run this skill only when the user says so. Otherwise the harness auto-compacts at `harness.context_budget` (300k; `th harness enable` sets it) and the pearl handoff is re-injected after the compaction. The hook asks you to run this yourself only with `harness.context_reset = clear`, or when the harness did not compact at the budget.

## Steps

1. **Pick the pearl.** The user-named one; else the in-progress pearl this work is on (`th pearls list --status=in_progress`, the id in the branch name); else create one exactly as `/th-handoff` step 1 does. Several pearls in flight → arm each (`--pearl` repeats, max 3).

2. **Do the th-handoff steps 2–4** (gather facts, get workers to push, write the note). Same discipline, same note sections. Be brief where the work is simple; the next session is you with an empty context, so `--next` must be the exact first action.
   `th pearls checkpoint <pearl> --note "$NOTE" --next "<first action>"`
   Build `NOTE` in a quoted heredoc (`NOTE=$(cat <<'EOF' … EOF)`) — never backticks in a double-quoted string.

3. **Read it back:** `th pearls show <pearl> --handoff`. If the fresh session would get it wrong from that text alone, checkpoint again.

4. **Arm:** `th harness handoff arm --pearl <pearl> --harness claude-code` (in Codex/OpenCode/others, pass that harness's name). It warns if the pearl has no checkpoint — fix that, don't ignore it. This terminal can claim the token for 24 hours. Arming prints the `/clear` + resume lines and copies the resume line to the clipboard.

5. **Stop and give the user the two lines `arm` printed**, copy-pasteable, plus where it picks up:
    ```
    /clear
    /th-handoff resume <pearl>
    ```
    `Picks up from: <next>.` Then end your turn. Do no more work: anything after the checkpoint is lost to the next session.

## Notes

- **In-process subagents die on `/clear`.** They must push and checkpoint before step 4, exactly as in `/th-handoff`.
- **Why the explicit resume line:** the armed token usually resumes the fresh session by itself (the SessionStart hook injects the handoff after `/clear`), but a hook can add context, not start a turn, and an expired or missed token leaves nothing injected. `/th-handoff resume <pearl>` works either way; if the handoff was already injected, it just re-reads the same pearl.
- **Changed your mind?** `th harness handoff disarm` drops the token; `th harness handoff list` shows what is armed.
- **Relaunch instead of `/clear`** (quit, then `claude` in the same directory within the hour) also resumes, when exactly one token is armed there.
- **Missed claim?** A fresh session that finds a token it can't take (expired, another terminal's) gets a note naming the pearl instead of silence. The resume line always works.
- **Other harnesses:** the same two commands work anywhere; the harness's clear is `/clear` or `/new`. Where no SessionStart hook claims the token, the fresh session can claim by hand: `th harness handoff claim --format text`.
