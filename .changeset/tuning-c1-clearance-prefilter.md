---
"@smooai/smooth": patch
---

C1: pre-filter the operator-runner's tool registry by the active role's clearance

The runner registers ~20 tools (file/bash/lsp/bg/network/etc.) and then
adds a `PermissionHook` that rejects calls to tools the active role isn't
allowed to use. That keeps the user safe but wastes a turn each time the
LLM calls a denied tool — the model picks the tool from the schema set,
gets a permission error, and has to retry.

Now the runner runs `tools.retain(|name| active_role.permissions.allows(name))`
before installing hooks, so denied tools are gone from the schema set the
LLM ever sees. PermissionHook stays as second-line defense in case a tool
is registered later in the lifecycle.

Adds `ToolRegistry::retain<F: Fn(&str) -> bool>` in
`crates/smooth-operator/src/tool.rs` so other call sites can do the same
filter without scraping `tools` directly.

One unit test (`retain_drops_unallowed_tools_only`) confirms the filter
drops disallowed tools while keeping hooks intact.
