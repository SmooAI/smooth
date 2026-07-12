---
"@smooai/smooth": patch
---

feat: `th crawl scrape` now works logged-out via the anonymous free tier (SMOODEV-2564, ADR-005). When not authenticated it POSTs to the public `POST /crawl/scrape` route with a bundled publishable client id (`x-crawl-client-id`) instead of failing — static-only, per-IP quota-capped — and prints a one-line nudge to `th auth login` for JS render + higher limits. Logged-in behavior (authed org route, full features) is unchanged. Implements the real-identity + free-tier seams of [[ADR-005-public-client-crawl-auth]] (now Accepted).
