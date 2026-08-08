---
'@smooai/smooth': minor
---

Skills are a daemon surface (pearl th-a5952d, epic th-d7366d). New ungated `GET /api/skills` route serves the one skill catalog (`smooth_cast::skills::discover` — project, user, Claude Code, and opencode skills) with the same guarded `?cwd=` override as `/search`, giving the web SPA (which has no disk access) a skills menu for the first time. `th code`'s `/skill` command, `/` popup, and chief skill-composition now read that daemon catalog (fetched once at startup), falling back to a local discover walk only when the daemon is unreachable. Server-side composition via a `skill` field on `send_message` is the follow-up (pearl th-b30a6a — needs an engine change).
