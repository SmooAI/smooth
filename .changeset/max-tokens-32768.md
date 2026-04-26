---
"@smooai/smooth": patch
---

Bump default `max_tokens` from 8192 → 32768 across the operator stack. Reasoning-model coding slots (smooth-coding via MiniMax M2.7) burn 1k–4k tokens on chain-of-thought before any visible content; with 8192 there's not enough budget left for the actual response + tool-call JSON, so multi-arg edits truncate and the agent burns iterations recovering. Affected configs: `LlmConfig::openrouter`/`anthropic` defaults, `ProviderRegistry::resolve_slot`, and the in-VM `smooth-operator-runner` startup config.
