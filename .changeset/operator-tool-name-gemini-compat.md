---
"@smooai/smooth": patch
---

operator: include `name` field on tool-result messages (Gemini OpenAI-compat fix)

Per customer-service-bot research (memory:
`reference_litellm_native_passthrough.md`), Gemini's OpenAI-compat shim
maps `role: tool` to a `functionResponse` block, which has a `name` field
that's not optional. Smooth was sending tool-result messages without
`name`, so any call routed through the OpenAI-compat layer to a Gemini
upstream would either drop the result silently or 400 with "requires a
tool name for each tool call response."

Fix is two-part:

- `Message::tool_result_named(call_id, name, content)` constructor that
  attaches the originating tool's name to a tool-role message. Old
  `tool_result` retained for legacy callers.
- `ChatMessage` adds a `tool_name` field that serializes as JSON
  `"name"` with `skip_serializing_if = "Option::is_none"` — present when
  set, omitted otherwise so legacy serialization is byte-identical.

The agent loop (`agent.rs`, all 3 tool-result push sites in `run()` and
`run_with_channel()`) now uses `tool_result_named(&tool_call.id,
&tool_call.name, &result.content)`. We always know the originating
tool's name at result time, so the named constructor is the right
default everywhere going forward.

OpenAI ignores the field. Anthropic uses `tool_use_id` pairing already
and doesn't reject the extra field. Sending it always is the safest
serialization across providers.

One new unit test (`tool_result_named_carries_name_through_serialization`)
covers both branches: named results emit `"name":"..."`, legacy results
omit the field entirely.
