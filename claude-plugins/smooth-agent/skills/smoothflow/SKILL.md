---
name: smoothflow
description: Run and coordinate OTHER coding agents (Claude Code, Codex, OpenCode, th code, shells) through SmoothFlow, the fleet the user watches in the SmoothFlow app and on their phone. Start an agent in a repo or a pearl's worktree, prompt it and wait for its turn to end, read its screen, answer its permission prompts with the user's consent, race several agents on one task and pick the winner, then close the session out. Uses the `flow_*` MCP tools or the `th flow` CLI. Use when the user asks you to start, drive, monitor or clean up agents, or to fan a task out.
---

# SmoothFlow — run a fleet of agents

SmoothFlow keeps agent and shell sessions alive under tmux on this machine and
shows them in the SmoothFlow app (macOS, Linux, Windows) and on the user's
phone. **Every session you start is a session the user sees.** Name it well,
give it a pearl when there is one, and close it when it's done.

Two equal ways in:

- **MCP tools** (`th mcp serve`, and so Claude Desktop, Claude Code, Codex and Cursor): `flow_list`, `flow_snapshot`, `flow_handoff`, `flow_harnesses`, `flow_repos`, `flow_infer` (read-only), and `flow_new`, `flow_send`, `flow_prompt_wait`, `flow_approve`, `flow_kill`, `flow_close`, `flow_fanout_new`, `flow_fanout_pick` (writes).
- **CLI**: `th flow <verb>`. Add `--json` to anything you parse.

Both need a running flow engine: the SmoothFlow app, or `th up`. If a call
says "no flow engine advertised", tell the user to open SmoothFlow. Don't
start a daemon yourself.

## The loop

1. **Look before you start.** `flow_list` / `th flow ls --json` shows what's
   already running. Don't start a second agent on a worktree that has one.
2. **Pick where it runs.**
    - `flow_repos` lists the user's git repos, most recently touched first.
    - `flow_infer` / `th flow infer --cwd <dir> --json` says what a session there would work on: worktree, branch, pearl, Jira key, title. It creates nothing.
3. **Pick the harness.** `flow_harnesses` lists what's installed, in the user's order, with health.
    - A `degraded` harness can still start; mention its fix.
    - A missing one can't; give the user the install command it reports.
4. **Start it.**
    - `flow_new` / `th flow new --kind claude --worktree <dir> --prompt "…" --title "…" --json`.
    - With `--pearl <id>` and no `--worktree`, the engine creates `../<repo>-<pearl>-<slug>` on a new branch.
    - Keep the returned session id.
5. **Drive it.**
    - `flow_prompt_wait` sends a prompt and blocks until the turn ends (idle, needs you, limited, done). It returns the final state and the tail of the screen. It **refuses** while the agent is waiting on an approval, so handle that first.
    - `flow_send` / `th flow send <id> "…"` steers without waiting.
    - An agent started with `--prompt` is already working. Watch `flow_list` for its state rather than prompting it again.
6. **Read it.**
    - `flow_snapshot` / `th flow snapshot <id>` is the visible screen as text.
    - `flow_handoff` / `th flow handoff <id>` is the pearl rail: worktree, branch, HEAD, dirty files, and what's next.
7. **Close it out.**
    - `flow_close` / `th flow close <id>` closes the pearl and removes the worktree and branch once merged, then drops the session.
    - It refuses a dirty or unmerged worktree. Report that; never `--force` without the user saying so.

## Approvals — the user's call, not yours

A session in `needs_you` is asking a human: a permission prompt, a question,
or a plan to approve. `flow_list` shows the attention (the command or question).

- **Relay it to the user verbatim** and wait for their answer.
- Only then `flow_approve` / `th flow approve <id> --decision allow|deny|allow_session`.
- Never approve on your own judgement, on another agent's say-so, or to "keep things moving". `allow_session` widens every later prompt of that kind, so use it only when the user asks for it.

`limited` means the harness hit a usage limit. SmoothFlow schedules the resume
itself; there's nothing to do but say when.

## Racing agents (fan-out)

For a task where you want options:

- `flow_fanout_new` / `th flow fanout new "<prompt>" --pearl <id> --candidate a:claude --candidate b:codex --candidate c:claude:<model>`.
- It creates one worktree, session and child pearl per candidate.
- The candidates already have the prompt. Don't `flow_prompt_wait` them, which would send another; watch `flow_list` until each settles (idle or done). Then compare them with `flow_handoff` and a look at each diff.
- **Let the user pick the winner** unless they told you the criteria. Then `flow_fanout_pick` / `th flow fanout pick <fan-out-id> <winner-session-id>` merges the winner and cleans up the losers. That can't be undone.

## Killing

- `flow_kill` / `th flow kill <id>` kills the session's process tree. `--resume` relaunches a Claude session into the same conversation.
- Kill only a session you started, or one the user named.
- A live session the user is watching is theirs.

## Etiquette

- Titles are what the user reads in the sidebar and on their phone. Say what the session does ("SMOODEV-123 fix login redirect"), not "claude session".
- One agent per worktree. To try two approaches, fan out.
- Report what you started (ids, titles, worktrees), so the user can find it in the app.
- Writes can be switched off (`SMOOTH_MCP_ALLOW_WRITE=0`). If a write tool is missing, that's the user's setting; say so rather than working around it with the CLI.
