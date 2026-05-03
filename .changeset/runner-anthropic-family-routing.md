---
"@smooai/smooth": patch
---

operator-runner: family-aware Anthropic shape for Claude models

Probed the gateway: `https://llm.smoo.ai/v1/messages` (LiteLLM's
Anthropic-shape route) already resolves smooth-* aliases AND uses native
Anthropic shape with proper `tool_use` / `tool_result` block pairing.
The OpenAI-compat translation at `/v1/chat/completions` silently mangles
Claude tool calls on the second turn (per customer-service-bot research,
memory: `reference_litellm_native_passthrough.md`).

Smooth's LLM client already supports `ApiFormat::Anthropic` and
`convert_messages_to_anthropic` — they construct `<api_url>/messages`
requests with the right shape. The gap was the operator-runner always
selecting `OpenAiCompat` regardless of the routed model.

Fix:
- New `provider_overlay::is_anthropic_family(&str)` helper detects
  Claude-class models (smooth-judge, smooth-fast-haiku, smooth-reviewing-haiku
  aliases, plus any model name containing `claude`, `anthropic`, `haiku`,
  `sonnet`, `opus`). Case-insensitive.
- Operator runner's LlmConfig construction site (line ~1559) now picks
  `ApiFormat::Anthropic` when the family check matches, otherwise the
  existing `OpenAiCompat`. Logs the routing decision via tracing for
  observability.

Combined with the prior tool-name compat fix (PR before this), Smooth now
routes:
- Claude models → `https://llm.smoo.ai/v1/messages` (Anthropic-shape,
  alias-resolving)
- Everything else → `https://llm.smoo.ai/v1/chat/completions` (OpenAI-compat,
  alias-resolving, with tool-result `name` field for Gemini compat)

One unit test covers the family detection across alias and direct-model
spellings + negative cases (gpt, kimi, gemini, deepseek must NOT match).
