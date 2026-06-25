---
'smooai-smooth-daemon': patch
---

EPIC th-c89c2a: the daemon now feeds its kernel-sandboxed tools to the operator
local flavor through the operator's `#68` `ToolProvider` seam instead of the
earlier bespoke `extra_tools` list (which collided with `#68` and has been
dropped upstream). `SandboxedToolProvider::tools_for` returns
`default_tools_with_proxy(workspace, proxy)` per turn; `serve_local_flavor`
installs it via `LocalServerBuilder::tools(provider)`. Tracks the rebase of the
`smooth-local-flavor` operator branch onto `main` (post-HITL, post-#68).
