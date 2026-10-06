---
'@smooai/smooth': minor
---

`th code`: your turns are now a distinct block in the transcript — a coral `▌` accent bar, a `❯` prompt glyph and bold text — instead of a "You:" label that looked like the agent's. Tool-call lines are indented and dim so the answer stands out. While a turn runs, a small animated Big Smooth (from the splash avatar) sits above the status bar with the current activity and elapsed time; it hides when the turn ends, never takes rows from the input box, and is off under `NO_COLOR` or `TH_REDUCED_MOTION=1` / `SMOOTH_REDUCED_MOTION=1`.
