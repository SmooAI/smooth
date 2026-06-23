---
'smooai-smooth-daemon': patch
'smooai-smooth-web': patch
---

Phase 4 (EPIC th-c89c2a): surface daemon uptime. `AppState` records a
`started_at` instant at construction, and `GET /api/status` now reports
`uptime_seconds`. The control-surface header shows a compact "up 2h 14m"
indicator next to the version — useful at-a-glance signal for an always-on
daemon. Tested that status carries uptime; verified live.
