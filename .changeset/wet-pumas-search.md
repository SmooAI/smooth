---
'@smooai/smooth': patch
---

th-7031ba: `web_search` is now self-contained — it no longer requires Smoo auth

The agent's `web_search` tool used to shell `th search`, which hits Smoo's in-house search service on api.smoo.ai. Logged out (or with that service unreachable) the tool degraded to a capped free tier or failed outright.

It now runs an in-process meta-search (`smooth_tools::search_native`) across public JSON APIs — Brave Search (`SMOOTH_BRAVE_API_KEY` / `BRAVE_API_KEY`), a SearXNG instance (`SMOOTH_SEARXNG_URL`), DuckDuckGo Instant Answer, Hacker News, and Wikipedia — merging, deduping (scheme/`www.`/slash-insensitive, with a corroboration bonus) and ranking the hits, capped so no one source floods the page. Every source is optional and failures are per-source, so the keyless sources still answer with no key configured; set a Brave key or a SearXNG URL for full web-index coverage. `answer: true` still prefers the Smoo service for synthesis and falls back to native results when it isn't available. No HTML scraping anywhere.
