---
'@smooai/smooth': patch
---

Stop `th code` from displaying fabricated telemetry: hardcoded `0.0s` tool
durations, a permanent `0 tok · $0`, and a synthesized model name.

None of these numbers came from the daemon. The client hardcoded `duration_ms: 0`
and `cost_usd: 0.0` when translating canonical frames, and the status bar built a
model name out of a local cast table (`smooth-{slot}`) that had nothing to do with
what was actually running — meanwhile the session JSON recorded a never-updated
`claude-sonnet-4` default. This is not cosmetic: asked to explain its own crash,
the agent read a screen where all ten tool durations said `0.0s` and invented a
`find` timeout and an OOM to account for them. Fabricated telemetry produces
fabricated reasoning.

- **Durations** — the event loop already captured an `Instant` on each
  `ToolCallStart` and then threw it away; it's now what gets rendered. The
  canonical `toolResult` frame is read for a `durationMs` first, in case the
  server ever forwards the one the engine measures.
- **Cost and tokens** — read off the terminal `eventual_response`'s
  `data.data.usage` (`costUsd` / `promptTokens` / `completionTokens`), which was
  on the wire all along. Token totals now accumulate at all. When the server
  reports no usage, the segment renders nothing instead of a false `$0`.
- **Model** — the status bar names the routing the daemon reported, else the model
  we ourselves put on the wire, else `unknown`. `/model <x>` now sets the field
  that actually rides `send_message`, so switching models does something. Nothing
  is synthesized from local tables any more.

Throughout: an absent field renders as nothing or `unknown`. A blank is honest,
a zero is a lie.
