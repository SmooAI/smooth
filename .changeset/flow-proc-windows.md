---
'@smooai/smooth': patch
---

SmoothFlow's engine can tell whether a process is alive on Windows (th-64d4ab, the first step of the cross-platform plan). Process liveness called `ps`, which Windows doesn't have, so every agent read as dead and supervision would have kept resuming the whole fleet. On Windows the engine now reads the process table with `sysinfo`, still matching the start time so a recycled pid never passes for the agent that died, and ends a process tree with `taskkill /T`, then `/F` after the grace period. macOS and Linux are unchanged.
