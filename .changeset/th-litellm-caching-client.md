---
'@smooai/smooth': minor
---

LiteLLM prompt-caching client support. The operator-runner now sends
Anthropic-shaped `cache_control: {type: ephemeral}` markers on Claude
routes (model id contains `claude` / `sonnet` / `opus` / `haiku`, or one
of the Smooth LiteLLM aliases like `smooth-coding-claude`) when the
api_base looks like LiteLLM or anthropic.*. We mark three breakpoints:
the system prompt, the last tool definition (caches the entire tool
block plus system), and the last message in history (extends the cache
turn-by-turn). Non-Claude / OpenAI / Gemini routes still send a plain
string `content` — no cache_control on the wire.

Cache-hit numbers (`usage.prompt_tokens_details.cached_tokens`) are
read back from the response, aggregated in `CostTracker.
total_cached_tokens`, plumbed through `AgentEvent::Completed.
cached_tokens`, and surfaced on Big Smooth's `[METRICS]` pearl-comment
line so a session's cache-hit ratio is observable. Requires the smooai
LiteLLM gateway to have `cache_control_injection_points` configured —
without that, this code is a no-op.
