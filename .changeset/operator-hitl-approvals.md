---
'smooai-smooth-code': patch
---

EPIC th-c89c2a (th-1ea4f6): write-tool approvals (HITL) on the operator path. When
the operator parks a turn on `write_confirmation_required` (opt-in via
`SMOOTH_AGENT_CONFIRM_TOOLS`), `th code` now surfaces a `⚠ Approve <tool>? — [y]es/
[n]o` prompt, and `y`/`n`/`Esc` resume or deny the parked turn via a new
`confirm_tool_action` reply. This is the "ask" half of the permission model atop the
operator's kernel-sandboxed tools — replacing the deleted bespoke `:4400` approval UI.
