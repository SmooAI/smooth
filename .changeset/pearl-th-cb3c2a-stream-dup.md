---
'@smooai/smooth': patch
---

Pearl th-cb3c2a: fix streaming buffer duplication (first-char doubling
and whole-paragraph re-emit) in `smooth-operator`'s `chat_stream`.

The OpenAI-compatible streaming path always treated `delta.content`
chunks as incremental deltas and `push_str`'d them onto the running
buffer. Some upstreams behind LiteLLM (and a few OpenRouter providers
in certain modes) actually emit **cumulative** content per chunk —
each chunk contains everything-so-far instead of the new tail. Treating
those as deltas produced the quadratic blowup seen in
`~/.smooth/coding-sessions/*.json`:

* First-character doubling/tripling — `"I'll help you"` arriving as
  `"III'll help you"` because chunks `"I"`, `"I"`, `"I'll help you"`
  were all appended.
* Word-level doubling — `"Let Let me me first first read read"` from
  `"Let"`, `"Let me"`, `"Let me first"`, `"Let me first read"` all
  appended verbatim.
* Entire paragraphs repeated 3-4× within a single assistant message.

The corruption then fed into the next turn's `prior_messages`, so the
LLM saw its own garbled prior turn and tended to bail with "I don't
have context" instead of calling tools — which is why the agent in
the smoking-gun session emitted zero successful tool calls over 12
turns.

Fix: a per-stream `StreamContentNormalizer` between `parse_sse_line`
and the consumer. For each chunk, if the chunk is exactly the
accumulator (cumulative-restart), drop it; if it strictly extends the
accumulator, emit only the new tail; otherwise treat as a normal
delta. A separate per-tool-call-index normalizer applies the same fix
to `ToolCallArgumentsDelta` chunks so cumulative argument streams
can't produce double-encoded JSON. The normalizer is a no-op on
well-behaved delta-emitting providers (every OpenAI/Anthropic stream
we already ship through). Covered by seven new unit tests in
`crates/smooth-operator/src/llm.rs`.
