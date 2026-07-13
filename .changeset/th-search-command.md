---
"@smooai/smooth": minor
---

`th search <query>` — promote agentic web search to a clean top-level command.

The old form was `th web-search search <query>` — a redundant noun-then-verb. Search is now `th search "<query>"` (same flags: `--answer`, `--depth`, `--max`, `--json`, `--org-id`). `th web-search search <query>` still works as a hidden back-compat alias for the form shipped in v0.18.0.

Also fixes the top-level `--help` copy, which had three bugs: the `crawl` command's description was empty, the `widgets` command had the crawler's description copy-pasted onto the front of it, and the search description still credited Tavily (retired in SMOODEV-2592 — search is now our own SearXNG + in-house crawler + LLM-synthesis stack, ADR-088).
