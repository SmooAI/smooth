---
'@smooai/smooth': patch
---

The bench pins the headless permission posture explicitly.

The daemon now defaults to `AcceptEdits`, which asks a human before anything
that isn't a known-safe command. A bench has no human, so every bash call would
have parked and stalled for the approval timeout — measuring the gate instead of
the model. Both spawn paths now set `SMOOTH_AUTO_MODE=bypass`; circuit-breakers
and narc's destructive guard still apply.
