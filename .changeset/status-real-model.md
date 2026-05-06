---
"@smooai/smooth": patch
---

Status bar: show the resolved active model, not a hardcoded "claude-sonnet-4" default

`AppState::new` defaulted `state.model_name` to `"claude-sonnet-4"`
and the status bar printed it verbatim — never updated, so the
label was wrong for any session running through Gemini, DeepSeek,
or anything other than Claude.

Status now derives the label live:

- **In-flight**: prefer `current_phase_alias` (e.g. `smooth-reasoning`)
  with `current_phase_upstream` appended when known
  (`smooth-reasoning → claude-opus-4-5`). Both are populated by the
  runner's `PhaseStart` events.
- **Idle**: synthesize the alias from the active role's slot —
  `smooth-{slot}` (`smooth-coding`, `smooth-reasoning`, etc.) —
  matching the convention in `~/.smooth/providers.json`.
- **Unknown role** (typo / custom role): fall back to the role name.

`state.model_name` is left in place since the model picker + session
save path still use it; just no longer driving the status bar.
