---
'@smooai/smooth': patch
---

`th run` dispatches again, and fails loudly when it cannot. It used to POST a legacy `/api/tasks` route on a hard-wired `:4400` that no daemon has served since the microVM stack went; the SPA fallback answered `200 text/html`, and `th run` exited 0 having dispatched nothing (th-9d4b09). It now reads the pearl from the local store and runs one headless turn over the daemon's canonical WebSocket, discovered via `$SMOOTH_URL` → `~/.smooth/daemon.addr` → `:4400`. The headless SSE fallback also refuses a non-event-stream reply and an empty stream instead of reporting success.
