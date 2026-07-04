---
'@smooai/smooth': minor
---

Big Smooth's own chat loop now hosts SEP extensions (pearl th-6d8606). The daemon loads pre-trusted extensions once at startup into a shared ExtensionHost; every chat turn registers their tools alongside the pearl/teammate tools (gated by the same AutoMode permission hook and a newly-added Narc surveillance hook on the chat registry), routes their ui/* requests onto the existing UiRelay machinery in-process (task_id `big-smooth-chat` — no HTTP-to-self), and intercepts `/cmd args` chat messages as extension slash commands. New routes: `GET /api/ext` (loaded extensions + commands) and `POST /api/ext/reload`; `th ext reload` now hot-reloads the running daemon's host best-effort instead of always deferring to the next session.
