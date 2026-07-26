---
'@smooai/smooth': patch
---

Add `th referrals` — the operator CLI for the partner / advocate referral program (SMOODEV-1035), which had API routes and schemas but no way to drive them short of hand-rolled curl. Covers `show` / `create` / `update` for the program economics, `partners list|add|update|remove`, `link` to print a partner's shareable referral URL, plus `attributions` / `visits` / `commissions`. Registered as top-level `th referrals` and under `th api referrals`. `--rate` takes a human percentage (20) rather than basis points, and partners are addressable by email, display name, or code instead of uuid. Referral links point at `api.smoo.ai/r/<code>` — the host that actually serves the redirect; the marketing site 404s that path.
