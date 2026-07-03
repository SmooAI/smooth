---
'@smooai/smooth': minor
---

SEP Phase 8 (smooth) — render-block v2 DSL parity in both frontends.

**Web (`UiRelay.tsx`).** The daemon `ui/*` relay's `RenderBlock` now renders the
Phase 8 interactive `widget` kind (its `body` block plus a legend of the declared
keybindings) and aligns the `table`/`diff`/`stack` field names to the formalized
DSL (`columns`/`patch`/`children`), accepting the pre-Phase-8 aliases
(`headers`/`diff`/`items`) as a fallback.

**TUI (`smooth_code::sep_host::RenderBlock`).** The terminal render-block
substrate gains reduced-fidelity `table` (aligned columns), `diff`, `stack`
(recursive), and `widget` (body + keybinding legend) kinds, matching the web and
the SDK DSL, so a `widget`-driven extension degrades cleanly to the terminal.

**Deferred (out of Phase 8 scope, follow-ups filed).** Live interactive
`widget/key` routing from the TUI needs the engine-pin cutover to the Phase 8
`smooth-operator-core` (which adds `dispatch_widget_key`) plus a live `UiSink`
wired through the daemon relay (the daemon/auto-mode epic). Discovered
`[resources] themes` application is deferred too: it needs either the compile-time
`theme.rs` palette refactored to a runtime state (TUI) or theme colors plumbed
through the daemon to the web SPA — discovery without either is dead code.
