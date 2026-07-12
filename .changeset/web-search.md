---
"@smooai/smooth": patch
---

feat: `th web-search search <query>` — agentic web search (Tavily-backed) alongside `th crawl`, for augmenting agentic coding (usable directly from `th code` / Big Smooth worker sessions). Ranked results with an optional synthesized `--answer`; `--max`, `--depth basic|advanced`, `--json`. Same two-tier model as crawl (SMOODEV-2573): full options when logged in (authed org route), an anonymous free tier (basic depth, capped results, per-IP quota) with the bundled `th` public-tools client id when not. Shares the ADR-005 public-client id with `th crawl`.
