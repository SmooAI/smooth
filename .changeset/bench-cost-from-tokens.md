---
'@smooai/smooth': patch
---

bench: `score` reports real per-task cost. Streaming LLM responses never carry
the gateway's cost header (it flushes before the body, so `x-litellm-response-cost`
reads 0.0) and the trailing usage chunk has tokens only — so every polyglot
engine reported `$0`. The `score` path now fetches list prices from
`/v1/model/info` (the `PriceBook` already used by the agentic path) and computes
cost as tokens × price, and `result.json` records `prompt_tokens`/`completion_tokens`
(th-c3618b).
