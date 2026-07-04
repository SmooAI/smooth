---
'@smooai/smooth': minor
---

SEP Phase 4 (smooth) — attach extensions to the operative + `th ext reload`.

**Operative attach** (pearl th-70cd08). The dispatched worker (`smooth-operative`)
now discovers installed SEP extensions and loads the PRE-trusted ones into a
headless `ExtensionHost` attached to its `Agent`, so their tools, `tool_call`
hooks, and turn events run in real dispatched tasks and flow out on the existing
`AgentEvent` stdout stream. Trust is fail-safe: unattended, an unknown or
content-changed extension is silently skipped (never prompts). The delegate is
the engine's headless default (empty `ui_capabilities`, `-32001 NoUI` for two-way
`ui/request` until the daemon relay, Phase 6); extension tools ride the ordinary
`ToolRegistry`, so the NarcHook surveillance already installed applies to them.

**Trust store extraction.** `TrustStore` / `TrustRecord` / `hash_extension` /
`trust_path` moved from `smooth-code::sep_host` down into `smooth-policy`
(`ext_trust`) — a leaf crate both the TUI and the operative can depend on — and
are re-exported from `sep_host` so the `th ext` CLI is unchanged.

**Engine pin.** Bumped `smooth-operator-core` to the SEP Phase 4 engine rev
(command dispatch, session actions, hot reload).

**`th ext reload <name>`.** Re-validate an installed extension after editing it:
re-parse the manifest, re-hash it, and (when the manifest changed) re-confirm
trust so the next host start picks up the new version. In-session live reload
(the engine's `ExtensionHost::reload`) lands with the daemon relay (Phase 6).
