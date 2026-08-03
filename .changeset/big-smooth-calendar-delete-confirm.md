---
'@smooai/smooth': patch
---

Cancelling a calendar event now asks you first (pearl th-94cc4a).

Big Smooth's default posture is `AutoMode::Bypass` — mutations run unprompted.
Deleting a calendar event is the one mutation the agent can't walk back on the
next turn, so it's now the exception.

- **`calendar_delete`** — `delete` moved off the `calendar` tool onto its own
  tool, which the daemon always lists in `ServerConfig::confirm_tools`. A call
  parks the turn on `write_confirmation_required`; the web UI renders the
  approve/deny prompt and the delete runs only on `confirm_tool_action`
  `approved: true`. Deny, a 5-minute timeout, or a client that never answers all
  fail **closed**.
- **`calendar` keeps reads plus `add`/`update`, all unprompted** — behavior
  unchanged there. `delete` is off its allowlist and its schema enum entirely, so
  the gate can't be sidestepped by calling the other tool.
- The split is forced by the mechanism: the engine's `ConfirmationHook` matches
  on tool **name** (`contains`), not on arguments — "this verb confirms" is only
  expressible as "this tool confirms".
- The confirm list is a **floor**: `SMOOTH_AGENT_CONFIRM_TOOLS` can widen it but
  can't shrink it, so an unset env var can't disarm the gate.

Known gap: `th code`'s WS client doesn't render `write_confirmation_required`
yet, so a delete driven from there fails closed on the timeout instead of
prompting.
