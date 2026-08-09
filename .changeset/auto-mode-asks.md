---
'@smooai/smooth': patch
---

Big Smooth's auto mode now asks instead of bypassing.

The daemon wires an approver into the engine's permission gate, so an `Ask`
verdict parks the turn and requests approval over the same WS the web UI already
renders approve/deny for. With that in place the default mode moves off
`Bypass` (allow-everything-but-circuit-breakers) to `AcceptEdits` — edits flow,
everything else is classified. `SMOOTH_AUTO_MODE=bypass` restores the old
behavior for headless hosts.
