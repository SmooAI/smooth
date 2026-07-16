---
"@smooai/smooth": minor
---

Big Smooth: promote the demo-critical `th` capabilities to first-class named
tools so the model reaches for them reliably (it selects tools by name + schema,
not buried prose).

- **`web_search { query, answer? }`** → `th search` — open-web search.
- **`knowledge_search { query }`** → `th knowledge search` — the org's own KB.
- **`crawl { url }`** → `th crawl scrape` — read a specific page as markdown.

Each is a thin typed wrapper over the shared `th` resolver (argv only, no shell)
— they reach nothing new, they're just findable. The general `th` tool stays as
the catch-all for the long tail (`th api …`, `th pearls …`, config).
