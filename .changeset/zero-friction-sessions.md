---
'smooth': minor
---

SmoothFlow zero-friction sessions (th-c103c1): the New Session dialog no longer demands a pearl id. A new `smooth_flow::infer` derives worktree, project (the main checkout, even inside a linked worktree), branch, pearl id, Jira key and title from a directory, served at `GET /api/flow/infer` and `th flow infer`; `flow.new` runs the same inference, so any client gets the context without sending it, and `th flow new` with no arguments starts a session in the current directory.

Plain harness sessions can now be adopted into the fleet: a `claude` or `codex` started in an ordinary terminal joins it on its first hook, with its pearl/branch/worktree attached. Off by default (`th flow adopt on`), guarded on a known harness, a git worktree and a project the fleet already works in, and explicitly not engine-owned (no attach, no kill, no resume).

Harness hooks now discover the flow engine through `$SMOOTH_FLOW_ADDR` → `~/.smooth/flow.addr` → `~/.smooth/daemon.addr`. `flow.addr` is claimed by whichever daemon hosts a live flow engine, so hooks reach the SmoothFlow app's child daemon, which deliberately does not write `daemon.addr`.
