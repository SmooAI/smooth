---
'@smooai/smooth': patch
---

Add the skills invocation runtime — Claude-Code parity for `~/.smooth/skills/` (pearl th-e0f812).

Discovery and the `Skill` data model already existed; what was missing was
invocation. This wires skills into the agent loop:

- **`skill_use` tool** registered into the operative's ToolRegistry. It returns
  a skill's markdown body (prefixed with a constraints header derived from
  `scope` / `allowed_tools` / `allowed_hosts`) into the conversation as
  instructions to follow. A skill is a prompt, not code — the recipe drives the
  ordinary bash/file/edit tools. Missing/empty names error with the available
  list.
- **System-prompt catalog** injected at dispatch: names + descriptions +
  triggers only (bodies load on demand), budget-capped so a large skill library
  can't crowd out the context window.
- New `smooth_cast::skills::render_catalog` / `render_invocation` helpers (with
  `DEFAULT_CATALOG_BUDGET`) so chief and other callers can reuse the same
  rendering.

`allowed_tools` / `allowed_hosts` are surfaced to the model as advisory
constraints only; hard enforcement lands with the auto-mode permission model
(pearl th-515a13). `th skills list` / `show` and discovery precedence are
unchanged.
