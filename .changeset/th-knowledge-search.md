---
"@smooai/smooth": minor
---

`th knowledge` — promote knowledge to a top-level command with semantic search.

`th knowledge search "<query>"` runs the SAME retrieval an agent does over the org's OWN knowledge base (embed → dense+sparse hybrid → RRF), returning the most relevant passages. Scope to a single document with `--doc <id>`, cap with `--max`, or get raw JSON with `--json`. It rounds out the agentic-coding trio alongside `th search` (the web) and `th crawl` (a page).

The full knowledge surface (`list` / `show` / `content` / `upload` / `website` / `process` / `update` / `delete`) is now available top-level as `th knowledge` (alias `kb`), same as `th api knowledge`. Backed by the new authed `POST /organizations/:org_id/knowledge/search` endpoint (SMOODEV-2610).
