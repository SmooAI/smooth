---
'smooth': patch
---

bench: measure real LLM cost, and fix `--model` silently doing nothing

The cost column reported `$0` because llm.smoo.ai returns a request's price only
in the `x-litellm-response-cost` **response header**, and the engine parses the
JSON body (pearl th-11f9bb tracks the root fix). The bench now measures spend
itself from the gateway key's delta, minus its own driver/judge calls — with two
correctness guards: it waits for LiteLLM's asynchronous spend posting to settle,
and it samples the shared key's background traffic and renders anything below 2x
that noise as `<noise` instead of a precise-looking figure.

Separately, and more seriously: `--model` was silently ignored on the host spawn
path. The daemon reads `SMOOTH_AGENT_MODEL`, the bench set only `SMOOAI_MODEL`,
so every row of a model matrix ran the daemon's own default and the differences
between rows were run-to-run variance. Now pinned via a tested `apply_engine_env`.

With the pin working, every model other than the default returns an empty reply
(pearl th-c127d1, P0) — the bench reports those as INCONCLUSIVE rather than
inventing a result.
