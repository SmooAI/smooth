---
'@smooai/smooth': minor
---

Two TUI polish pearls landed together (`th-91d8af` + `th-a10c2d`):

**Pearl th-91d8af — bare `th` shows a friendly explainer.**
Running `th` with no subcommand used to drop new users straight
into the smooth-code TUI cold. Now it prints a one-screen
explainer covering what `th` is for, what the main subcommand
families do (`th code`, `th up`, `th pearls`, `th api`,
`th cast`, `th mcp`), and the most useful starter commands.
`th code` (and the existing top-level shortcuts `th --resume`,
`th --list`, `th --agent <name>` from pearl
`th-resume-top-level`) continue to launch the TUI directly —
the explainer only triggers when no subcommand and no code-mode
flags are present.

**Pearl th-a10c2d — TUI shows the upstream model behind a smooth-\* alias.**
When the user routes through an alias like `smooth-coding`, the
gateway resolves it to a concrete upstream (e.g.
`qwen3-coder-flash`). Previously the TUI only ever showed the
alias. The agent loop now captures the `model` field from chat
completion / Anthropic responses (and from streaming chunks)
into a new `LlmResponse.resolved_model` field, emits a one-shot
`AgentEvent::ModelResolved { alias, upstream }` per session when
the alias differs (idempotent — only re-emits if the upstream
changes mid-run), and the smooth-code status bar renders
`smooth-coding → qwen3-coder-flash`. Concrete-model selections
where alias == upstream stay quiet so the status bar doesn't
clutter.

Both behaviours are forward-compatible: the new
`AgentEvent::ModelResolved` variant slots into the existing
`#[serde(tag = "type")]` enum, so old clients silently skip it
and new clients connected to old runners just don't see it.
