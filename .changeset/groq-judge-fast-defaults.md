---
"smooai-smooth": patch
---

model defaults: judge → groq-llama-3.3-70b, fast → groq-llama-3.1-8b (pearl th-3468bd)

Post SMOODEV-1793 the concrete slot defaults in `SmoothSlot::concrete_default`
routed `judge` to `gemini-2.5-flash` and `fast` to `gemini-2.5-flash-lite`.
Update both to Groq Llama models matching the gateway's previous
`smooth-*` primaries:

- **`fast`** → `groq-llama-3.1-8b`. Sub-300ms first token, ~10× cheaper than
  Gemini Flash Lite. Matches the gateway's old `smooth-fast` primary
  (Groq Llama 3.1-8B-Instant).
- **`judge`** → `groq-llama-3.3-70b`. An 8B is too small for adversarial
  prompt-injection detection — the 70B catches paraphrase attacks the
  8B misses, while still landing under 1s on Groq and well under
  Gemini Flash on cost. Judge gates tool execution; refusal quality
  beats latency at this slot.
- `summarize` stays on `gemini-2.5-flash` — its 1M context window is the
  load-bearing feature for context compaction.
- Coding / reasoning / reviewing / default unchanged.

Catalog entries for both Groq models added to `fallback_catalog()` so
the picker has metadata (use_cases, tier, cost, description, AA index)
in offline mode. Migration and policy tests updated.
