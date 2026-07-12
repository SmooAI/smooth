---
"@smooai/smooth": patch
---

docs: ADR-005 — publishable-client + scoped-org auth model for the th web-crawl free tier. The credential bundled in the binary is a public client identifier (not a secret M2M key), scoped to a dedicated org that can reach only the crawl free-tier route; paid tiers authenticate with the caller's own org identity. Blocks search.smoo.ai (th-1d88f5).
