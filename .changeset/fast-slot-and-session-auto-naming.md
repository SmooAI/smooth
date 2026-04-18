---
"@smooai/smooth": minor
---

New `Activity::Fast` routing slot + LLM-generated session titles.

**`smooth-fast` slot**: a new utility routing slot for short,
latency-sensitive calls — session naming, short titles, autocomplete,
one-liner tool-result summaries. Targets a Haiku-class model. The
llm.smoo.ai gateway exposes it as `smooth-fast` (anthropic Haiku 4.5
behind the alias).

- `Activity::Fast` variant added to the routing enum.
- `ModelRouting.fast: Option<ModelSlot>` — optional on disk, so
  existing `providers.json` files still parse. When absent,
  `slot_for(Activity::Fast)` falls back to the `default` slot.
- Every preset (`SmoaiGateway`, `OpenRouterLowCost`, `LlmGatewayLowCost`,
  `OpenAI`, `Anthropic`) now configures a sensible `fast` slot
  (Haiku / gpt-4o-mini / gemini-flash-lite).
- `th routing show` lists the new slot so users can see where
  utility calls go.

**Session auto-naming**: first-message titles now come from the
`smooth-fast` slot — 3–6 words, Title Case, trimmed — instead of a
60-char truncation of the user's prompt. The LLM call is spawned
into a detached tokio task so chat response latency is unaffected.
On LLM failure we fall back to the legacy truncation so a session
is never stuck at "New chat".

Wire it up in your `~/.smooth/providers.json` once llm.smoo.ai's
`smooth-fast` alias is live in prod:

```json
"routing": {
  …existing slots…,
  "fast": { "provider": "smooth", "model": "smooth-fast" }
}
```
