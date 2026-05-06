---
"@smooai/smooth": patch
---

Tighten SMOOTH banner gradient boundary + clear all build warnings

- Banner boundary: switch the Smoo→th split from 3/4 to 17/25 so
  teal lands at the T's left edge (col 38 in the 55-char ANSI-Shadow
  banner) instead of bisecting the letter.
- `smooth-operator` `Activity::Planning` / `Activity::Thinking` are
  deprecated aliases for `Activity::Reasoning`; the `mapper` and
  `oracle` lead roles still referenced the old names. Updated both
  + the slot-routing test that asserted on the deprecated variants.
- `smooth-bigsmooth/server.rs`: `if let Some(ref diver_client) = diver`
  bound a name that wasn't used; switch to `diver.is_some()`. Comment
  notes the binding pattern to restore when a real Diver client call
  is wired.
- `smooth-bigsmooth/server.rs`: dead `SharedNarcHook` struct +
  `ToolHook` impl removed (never constructed). Dropped the now-
  dangling `async_trait`, `smooth_narc::NarcHook`, and
  `smooth_operator::tool::{ToolCall, ToolHook, ToolResult}` imports.
- `smooth-bigsmooth/server.rs`: dead `chat_system_prompt()`
  function removed (no callers).
- `smooth-operator-runner/lsp.rs`: drop the deprecated
  `InitializeParams::root_uri` field; we already pass
  `workspace_folders`, which is the LSP 3.6+ replacement.

Build finishes with zero warnings; 468 tests still pass.
