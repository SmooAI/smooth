---
"@smooai/smooth": patch
---

Pearl fixes:

- `/api/pearls` + `/api/projects/pearls` default to unbounded
  (`?limit=0`). The dashboard was silently capped at 100 — a repo
  with 150+ pearls showed "100 closed, 0 open" when the open ones
  were past the limit. LLM tool callers still get a 100-row
  default via `list_pearls()`.
- `PearlStore::close` is now invoked on every error-path exit of
  the sandboxed dispatch handler (runner not found, workspace
  create failed, LLM provider missing, runner exited non-zero).
  Previously only exit-0 closed the pearl; leaked `Task: ...`
  pearls accumulated from E2E runs.
- `install:th` now re-adhoc-signs a neighbor `smooth-dolt` binary
  in `~/.cargo/bin/` so `scp`'d copies work under `launchd`
  without a manual `codesign`.
