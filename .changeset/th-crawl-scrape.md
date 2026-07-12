---
"@smooai/smooth": patch
---

feat: `th crawl scrape <url>` — turn a page into clean markdown through Smoo's authed crawler (SMOODEV-2559). Any authenticated org member can use it; it POSTs to the new api-prime `POST /organizations/:org_id/crawl/scrape` route with the caller's own org identity. This is the **real-identity** seam of [[ADR-005-public-client-crawl-auth]] (paid/authenticated tier); the free bundled-public-client tier + search.smoo.ai backend remain future work. `--json` for the full response, `--format` for extra Firecrawl formats, `--org` to override the active org.
