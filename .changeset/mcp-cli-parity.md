---
'@smooai/smooth': minor
---

MCP→CLI parity batch: the ~35 hosted-MCP tools that had no `smoo` verb now do. New command groups `smoo analytics` (catalog / org-scoped validated query / GA4 reports), `smoo campaigns` (list / analytics / preview-first send — a real send requires `--confirm`, and per-recipient suppression stays server-side), `smoo drip` (sequences / enrollments / enroll / cancel / test-send), `smoo audiences` (list / create / members / add-members / resolve), `smoo forms`, `smoo gbp reviews`, `smoo search-console queries`, `smoo sheets snapshots`, and `smoo workforce` (bare command = the directory). Extended: `smoo files search|summarize`, `smoo heypage versions|rollback|source get|set|content get|set`, and `smoo api observability metrics list|query|attributes` + `web-vitals`. Every verb mirrors the exact route its mcp.smoo.ai twin calls, takes `--json`, reports empty results as answers, and always reports truncation.
