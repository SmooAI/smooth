# smooth-agent

A Claude Code plugin for **Big Smooth orchestration** — drive supervised Claude
Code worker sessions in tmux, coordinate them over
**th-mail**, and track work as **pearls**. Part of the `smooth` marketplace
(`SmooAI/smooth`).

## What it gives you

- **`/smooth`** command — the orchestrator surface. `run`, `add-agent`, `drive`/
  `manual`, `mail`, `status`, `ls`, `attach`. Drives the `th claude` engine.
- **`agent-comms`** skill — teaches a worker session to report status, answer
  pings, and hand off work over `th msg`/`th agent`.
- **`th-mail`** skill — `/th-mail` arms a background `th msg watch --once`
  watcher so an arriving message _pulls the session back in_ instead of it
  polling. The only surface that gets you _pushed_ mail rather than checking for
  it.
- **`pearls-flow`** skill — teaches a worker to track work as pearls
  (`th pearls`).
- **`smooth-operator`** skill — drives the org's dashboard agent from the CLI
  (`th api smooth-operator chat|confirm|history`) for org actions (email, CRM,
  analytics, knowledge) rather than code changes. Needs a `th auth login` user
  session; it 401s under an M2M client.
- **The `smooth` MCP server** — the plugin manifest registers `th mcp serve`,
  so every session gets the agent bus as _tools_ rather than shell calls:
  `agent_identity`, `agent_status`, `agent_list`, `mail_inbox`, `mail_send`,
  `mail_ack` (plus `pearls_ready`/`pearls_create`, `remember`/`recall`, and the
  org tier behind `th auth login`). Codex and OpenCode reach the same mailbox
  via `th mcp install --harness codex|opencode|all`.
- **`smooth-statusline.sh`** — shows which agent this session is and how much
  mail is waiting: `⚙ th:fix-auth ✉3`. Not wired automatically (Claude Code
  allows exactly one `statusLine` and clobbering yours would be rude) — run
  **`th doctor --setup-statusline`**, which installs it only if the slot is free
  and otherwise offers `smooth-statusline-with-ponytail.sh`, which renders
  ponytail's statusline and ours on one line.
- **Auto-onboarding to th-mail** — **every** Claude Code session lands on the
  bus so Big Smooth and other agents can reach it, via two hooks:
    - `register-agent.sh` (SessionStart) registers the session. `th claude run`
      workers register under their (already-meaningful) `SMOOTH_AGENT_HANDLE`; a
      plain `claude` session registers under a **placeholder** handle derived
      from the session (`cc-<cwd-basename>-<sid4>`, e.g.
      `cc-smooth-th-e651bc-agent-onboard-a21c`) with `--no-push` (registration
      fires on every start; skipping the Dolt remote push keeps it cheap).
    - `on-first-prompt.sh` (UserPromptSubmit) fires **once**, after the first
      prompt, nudging a placeholder session to rename itself to a task-meaningful
      handle: `th agent rename --from <placeholder> --to <new>` (carries its mail
      over). Workers are never nudged.
      Registration is always-on and safe — since pearl th-374f85 the mailbox is a
      machine-level SQLite file, so a register is a millisecond-scale local write.
      The hooks still do **not** auto-start a background `th msg watch`: a watcher
      per session is a lot of processes for something most sessions never need.
      Background mail-watching stays **opt-in** via the `/th-mail` skill. No `th` on PATH → the hooks are a no-op and the rest of
      the plugin still works.
- **Shared repo guardrail hooks** — the SmooAI worktree/pearls guardrails that
  used to be hand-copied into every repo's `.claude/hooks/`, now one source of
  truth (pearl th-44bace). All derive the repo/main-worktree from git at runtime,
  so the same scripts guard smooth·smooai·smooblue:
    - `enforce-worktree.sh` (PreToolUse Edit/Write/Bash) — asks before editing
      source or committing on `main` in the main worktree.
    - `session-worktree-warning.sh` (SessionStart) — warns when a session opens
      in the main worktree on `main`.
    - `th-curl-hint.sh` (PreToolUse Bash) — nudges raw curl against
      api/auth.smoo.ai / Jira toward the `th` CLI, and flags two secret-handling
      footguns.
    - `enforce-pearls-labels.sh` (PostToolUse Bash) — reminds to label a
      `th pearls create`.
    - `attest-push-hint.sh` (PreToolUse Bash) — in any repo with executable
      `scripts/ci/<check>.sh` files, asks for `th attest <checks>` INSTEAD of a
      bare `git push` (it runs the checks, THEN pushes, THEN credits the
      `ci-attest/*` statuses, so each credited CI row skips in ~8s). Override
      with `# attest:ack reason=...`.
    - `pearls-store-guard.sh` (PreToolUse Bash) — nudges away from the patterns
      that wedge the Dolt pearl store read-only (hand-deleting `.smooth/dolt`
      internals, raw `dolt` writes that bypass the single-writer server,
      backgrounded pearl/msg watchers). Override with `# pearls-guard:ack`.

    Skills:
    - `windows-build-box` — spin up a throwaway Windows EC2 build box over SSM
      (no RDP) to build/test on Windows faster than CI round-trips, then tear it
      down. Self-contained (`winrun.sh` ships beside the SKILL).

    Enable per-repo in `.claude/settings.json` (`enabledPlugins`) and delete the
    local `.claude/hooks/` copies — see the repo's own settings for the pattern.

## Requires

The `th` CLI (built from `SmooAI/smooth`) with the `th claude` engine, plus
`tmux` on `PATH`. The plugin is a thin recipe layer; the supervision and session
control live in `th claude` (the binary).

## Install

```
/plugin marketplace add SmooAI/smooth      # or: /plugin marketplace add ./ from a local checkout
/plugin install smooth-agent@smooth
```

Then `th claude run "<task>"` launches a supervised, plugin-active worker, and
`/smooth status` shows the farm.

**Codex users:** Codex consumes this plugin from the same `smooth` marketplace
but pins its own copy, and that pin has been sitting at **0.4.0** — run Codex's
plugin update to pick up anything newer, including the MCP server and the
`th attest` hint. Codex has no statusline surface, so that piece is Claude Code
only; `th mcp install --harness codex` gets it the mail tools regardless of the
plugin pin.

## How control works

Each worker runs in a tmux session shared between Big Smooth and you. A per-session
**mode** arbitrates who types:

- `driving` — Big Smooth sends input.
- `manual` — you drive (`th claude attach <id>`); the supervisor sends nothing.
- `paused` — the supervisor stands down and only watches.

Flip with `/smooth drive <id>` / `/smooth manual <id>` or `th claude mode <id> <mode>`.
`th claude tui` is the live dashboard — every session's pane plus keys to flip
mode and attach.

## Note on scale (subscription ToS)

This drives Claude Code **subscription** auth. The supervisor **stops** when the
account hits a real usage/quota limit — it does not wait it out or auto-resume.
As of `th` 0.22 it no longer retries the transient 429 throttle either (pearl
th-2d5c45): Claude Code retries that internally, so a supervisor-side
backoff-and-resend only risked double-sending a prompt. A large unattended fleet
to maximize a flat-rate plan is the gray zone — keep the worker count tasteful.
True fleet scale belongs on the metered API + smooth-operator.
