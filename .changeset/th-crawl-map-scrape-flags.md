---
"@smooai/smooth": minor
---

Extend the `th` agentic-tooling commands against the api-prime crawl/search/knowledge routes:

- **`th crawl crawl <seed>`** — new authed whole-site crawl. Supports `--limit`, `--max-depth` (sent as `maxDiscoveryDepth`), `--extract` (JSON spec verbatim or wrapped as `{"prompt": …}`), `--json`, and `--org`. Default output is a compact `completed/total` summary plus one crawled URL per line.
- **`th crawl map <url>`** — new authed URL-discovery command. Supports `--search`, `--limit`, `--include-subdomains`, `--json`, and `--org`; prints one discovered link per line by default.
- **`th crawl scrape`** — new `--extract <SPEC>` (JSON verbatim or `{"prompt": …}`), `--screenshot` (appends `screenshot` to the formats), and `--render <MODE>` flags. `--extract`/`--render` are authed-only; the free tier surfaces the backend's rejection.
- **`th search --scrape`** — forces `searchDepth: "advanced"` so each result is crawl-enriched with full page content (authed tier; clamped on the free tier).
- **`th api knowledge add-url <url>`** — new authed command that kicks off a crawl→ingest job into the org's knowledge base via `POST /organizations/{org}/knowledge/websites` (`--name` defaults to the URL).
