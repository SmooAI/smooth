---
'@smooai/smooth': patch
---

Big Smooth can now delegate subtasks to a sidekick (pearl th-1adf55).

The engine already shipped `DispatchSubagentTool` (`send_sidekick`) — the
daemon just never registered it. Now `tools_for` builds it from the engine's
built-in cast plus a snapshot of THIS turn's tool set, so a sidekick inherits
the same kernel-sandboxed fs/grep/bash instances filtered to its role's
clearance (`scout` read-only, `runner` full), and runs in its own isolated
conversation returning only a summary — keeping an expensive investigation out
of the parent's context window (the win behind Claude Code's Task tool).

Registered last so the snapshot never contains `send_sidekick` itself (no
recursive dispatch), and only when a gateway is resolvable (a sidekick with no
model would just error). Sidekick sub-calls still cross the load-bearing kernel
sandbox, but NOT the daemon's userspace deny-policy/narc hooks — documented
in-code as a known defense-in-depth gap for this first cut.
