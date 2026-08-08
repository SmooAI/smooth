---
'@smooai/smooth': patch
---

bench: capture engine stdout/stderr instead of discarding it

`spawn_engine` sent the engine's output to `Stdio::null`, so a failing polyglot
engine left nothing to diagnose from. Host runs now write
`<run>/<scenario>/trial-N/log/<engine>.log`. Losing the log degrades the run
rather than failing it.

This immediately paid for itself: running the agentic suite across all five engines
found the TS server completing turns while calling zero tools (reads as a model FAIL,
not an infra error — th-11284c) and the .NET server returning INTERNAL_ERROR on every
turn while logging no exception at all (th-df7007, th-e7ef23).
