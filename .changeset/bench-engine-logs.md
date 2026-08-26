---
'@smooai/smooth': patch
---

bench: capture engine stdout/stderr in host score mode. `EngineTaskRunner`
passed `log_dir: None`, so every host-mode engine's output went to /dev/null —
which made a run of generic `INTERNAL_ERROR` turns impossible to diagnose
without re-running. Now logs land at `<run_dir>/engine.log`, a sibling of the
agent's sandbox (th-34af94).
