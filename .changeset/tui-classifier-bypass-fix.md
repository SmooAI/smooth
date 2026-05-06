---
"@smooai/smooth": patch
---

th smooth TUI: fix intent classifier bypass + drop dead Ctrl+B hint

The intent classifier (added to route questions to oracle and work
to fixer) never fired for fresh `th` invocations — every session
was silently pinned to fixer, so questions like "how do I run dev
mode" still ended up in the coding workflow with file writes and
hallucinated test counts.

Root cause: `cmd_code` in `smooth-cli/src/main.rs:2204` always
passed `Some(agent_name)` to `app::run_with_session`, where
`agent_name` was unconditionally resolved to `"fixer"` via
`resolve_primary_agent(None)`. `app::run` saw `Some(_)` and set
`agent_pinned = true`, which bypassed the classifier branch in
`handle_input_mode`.

Fix: pass the **original** `agent: Option<String>` (the unresolved
CLI flag) to `run_with_session`. `agent_name` stays around for the
typo-validation call and the headless path. Now when the user
runs plain `th` (no `--agent`), `agent` is `None`,
`agent_pinned` stays `false`, and the classifier runs per
message. Explicit `--agent foo` still pins as designed.

Bundled cleanup: dropped the dead `Ctrl+B sidebar` hint from the
status bar. The keybinding was removed when sidebar rendering went
away in the inline-viewport pearl, but the status bar text never
got updated.
