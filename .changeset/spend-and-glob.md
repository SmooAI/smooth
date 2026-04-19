---
"smooai-smooth-operator": minor
"smooai-smooth-operator-runner": minor
"smooai-smooth-bigsmooth": patch
"smooai-smooth-code": minor
---

- **Cost threading**: `AgentEvent::Completed` now carries `cost_usd`, and Big Smooth's sandboxed dispatch path forwards that into `ServerEvent::TaskComplete` instead of the hardcoded `0.0` it sent before. `LlmResponse.gateway_cost_usd` captures the authoritative gateway-reported cost (LiteLLM's `x-litellm-response-cost-*` headers, with `-margin-amount` / `-original` / the legacy `-response-cost` all checked); `CostTracker::record_with_cost` replaces local `ModelPricing` guesswork when the gateway reports a real number.
- **Spend meter in the TUI**: status bar shows `spend: $X.XXX` next to the token count, accumulated from every `ServerEvent::TaskComplete` across the session. Renders `$0` on fresh sessions; three-decimal precision under $1, two-decimal above.
- **Glob `@` autocomplete**: `@**/*.rs`, `@../**/(dashboard)`, `@~/dev/**/README.md`, `@apps/**/package.json` all resolve through `ignore::WalkBuilder` + `globset`, respecting `.gitignore`. Falls through to the existing path-prefix listing when the query has no glob metacharacters. `(parens)` from Next.js route groups match literally.
