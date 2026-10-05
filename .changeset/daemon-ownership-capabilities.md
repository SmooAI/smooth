---
'@smooai/smooth': minor
---

Big Smooth.app now owns the daemon when it is installed. `th up` and `th code` launch the app instead of starting a competing daemon (opt out with `th settings set daemon.prefer_own true`). The daemon reports `GET /api/capabilities`, and `th code` feature-detects per-session workspaces, warning once with the version needed and where to update instead of silently doing nothing against an older daemon. Unknown `/api/*` routes now return a JSON 404 instead of the web UI. `/status` shows the daemon's version and capabilities. See ADR-012.
