---
'@smooai/smooth': minor
---

th-cc50cd: smooth-agent OpenCode lifecycle plugin — every OpenCode session now registers on the th-mail bus (placeholder handle `oc-<dir>-<sid4>`, pid-reaped), publishes working presence from tool activity (throttled), goes idle on session.idle and offline on session.deleted, degrading to a silent no-op without `th`. `th harness enable opencode` links it into `~/.config/opencode/plugins/` from the smooth-agent plugin checkout (same never-clobber/ownership rules as skills; `disable` removes it), `status` reports it, and the node smoke test is wired into `pnpm test:hooks`.
