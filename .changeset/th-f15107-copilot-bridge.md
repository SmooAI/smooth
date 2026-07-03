---
'@smooai/smooth': patch
---

`th api copilot` — CLI bridge to the org's always-on dashboard Copilot (smooai PR #2383, pearl th-f15107).

Three subcommands mirror the org-authed copilot routes on `api.smoo.ai`:

- `th api copilot chat "<message>" [--conversation <id>] [--json]` — runs a turn, prints the reply plus a compact `ran <tool>` line per tool call. Continues an existing conversation with `--conversation`.
- `th api copilot confirm <conversation-id> --approve|--decline` — resolves the destructive action a turn paused on, without resending the message.
- `th api copilot history <conversation-id>` — prints the conversation's message history.

Destructive tools (e.g. `email.send`) never auto-run: a turn that triggers one returns a `pendingAction` and pauses. `chat` resolves it with a y/N prompt on a TTY, or the up-front `--confirm` / `--no-confirm` flag for non-interactive/agent use. With **no flag on a non-TTY** it prints the pending action and stops rather than guessing — `--no-confirm` is never a silent default. Authenticates as the logged-in user (`th auth login`), like `th api crm`, so every tool run is audit-logged against the real person.

Ships an `org-copilot` marketplace skill (`claude-plugins/smooth-agent`) teaching Claude Code when and how to drive the copilot (including the confirm-flow safety rules), and documents the surface in `docs/Engineering/Using-th-CLI.md`.
