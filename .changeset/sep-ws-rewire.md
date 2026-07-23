---
'@smooai/smooth': minor
---

Talk to your org's Smooth Operator again — rewired over the SEP WebSocket.

The buffered REST route `POST /organizations/{org}/smooth-operator/chat` was
deleted upstream (SMOODEV-2673), so both `ask_business` (the MCP tool Claude
Desktop / Cursor use) and `th api smooth-operator chat` were pointed at a dead
endpoint and 404'd. New `smooai::smooth_operator_ws::operator_turn` mints a
short-lived socket token from api-prime, connects
`wss://smooth-operator.smoo.ai/ws`, creates/resumes a conversation session,
sends the message, and buffers the streamed turn into a final reply — hand-rolled
because the `smooth-operator` crates are server-side and ship no Rust client.

Destructive tools now confirm **inline**: the socket parks the turn
(`write_confirmation_required`) and the decision rides the same connection, so
approval is a flag (`approve: true` / `--confirm`) instead of a second call.
Without it the action is declined and reported, never silently run. The old
`th api smooth-operator confirm` subcommand is retired with an explanation.

Verified live against production on both surfaces.
