---
'@smooai/smooth': patch
---

`th code` is a first-class Big Smooth client again — canonical WS protocol
(pearl th-248f33).

`th code` spoke the bespoke `smooth-bigsmooth` protocol (`TaskStart` out,
`Connected`/`TokenDelta` in). That crate was **deleted** with the microVM stack
(th-f4a801) and Big Smooth now hosts smooth-operator's canonical,
schema-driven WS — so nothing had spoken `th code`'s dialect for months and
every turn died on "Timed out waiting for Connected event".

It now speaks the same protocol as the web SPA (`smooth-web/web/src/operator.ts`):
`create_conversation_session` on connect, `send_message` per turn, streaming
back over `stream_token` / `stream_chunk`. Same daemon, same conversations and
sessions, same tools — just a terminal instead of a browser.

Translation happens at the edge of `client.rs`, mapping canonical frames onto
the TUI's existing internal events, so `app.rs`/`render.rs` are untouched.
Details that came from reading the shipped client rather than guessing: the
session reply IS the connection signal; tool calls *and* results both nest
under `rawResponse` (reading `state.toolResult` leaves tools stuck "running"
forever); `stream_reasoning` is dropped so chain-of-thought never renders as
the answer. Cancel/steer have no canonical verb, so they send nothing rather
than a frame the server would reject.

Verified end-to-end against the live daemon: a real turn streams a reply.
