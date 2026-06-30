---
'smooai-smooth-daemon': minor
'smooai-smooth-web': minor
---

`@` mention search in the smooth-web composer, mirroring the th code TUI (th-58b5fe).
Typing `@` opens an autocomplete popup (files + path expansion from the workspace),
backed by a new ungated `GET /search?q=` endpoint on the daemon (workspace files via
the pruned ripgrep walk, bounded by a walk budget). The endpoint is mounted via a new
`LocalServer.serve_routes()` seam. `@` and the `/smooth-mode` slash popup coexist —
exactly one shows, decided by which token the caret sits in. Pearls in mentions are a
documented follow-up (the `kind` field already accepts "pearl").
