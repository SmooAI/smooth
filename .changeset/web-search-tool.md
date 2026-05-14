---
'@smooai/smooth': patch
---

tools: native `web_search` backed by DuckDuckGo HTML, no API key. New
`smooth_bigsmooth::web_search` module + `GET /api/web_search?q=&n=`
route. Big Smooth makes the outbound request so each sandbox doesn't
need a TLS HTTP client + outbound permission for the search backend.
`html.duckduckgo.com` and `duckduckgo.com` join the Narc obviously-
safe domain list so the in-VM Wonk auto-approves without a human
prompt. Untrusted result content is scanned for prompt-injection
markers (`ignore previous instructions`, `</system>`, etc.) and
redacted before return; `redacted_count` in the response surfaces
how many hits fired. 16 unit tests (parser + redaction) + 8 wire-
shape integration tests. Pearl th-70b68b.
