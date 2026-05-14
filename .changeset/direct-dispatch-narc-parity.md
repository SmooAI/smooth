---
'@smooai/smooth': patch
---

dispatch: non-sandbox path now gets Wonk/Narc parity. The "direct"
dispatch path (no microVM) spawns operator-runner natively; the
runner already brings up its own in-process Wonk via `spawn_cast`,
but the spawn never received `SMOOTH_NARC_URL`. Result: the
in-runner Wonk had no arbiter, hard-denied anything its local
policy couldn't auto-approve, and the agent never reached the
Claude-Code-style auto-mode prompts. Setting `SMOOTH_NARC_URL` on
the direct-dispatch subprocess wires the runner's Wonk to Big
Smooth's Boardroom Narc, so the same Decision::Ask → AccessStore
→ TUI → resolve loop now gates direct tool calls too. Pearl
th-e96aeb.
