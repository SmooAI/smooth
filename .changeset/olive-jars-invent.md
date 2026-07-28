---
'@smooai/smooth': patch
---

Fix `th code` dying with "Error: unknown error" on any turn longer than 15 seconds.

The keep-alive heartbeat serialized `ClientEvent::Ping` directly, putting the
bespoke `{"type":"Ping"}` on a wire that speaks the canonical operator protocol.
The server rejected it every 15s with `VALIDATION_ERROR / missing 'action' field`,
and that error tore down whatever turn was in flight — while the daemon went on
to finish the work and persist an answer the user never saw.

Three fixes:

- The heartbeat now builds its frame with `to_canonical_frame`, like every other
  outbound message. The loop moved into a testable `heartbeat_loop` function, so
  the regression test drives the real path instead of the helper that was always
  correct.
- New shared `smooth_cast::wire::error_message`, used by `th code`,
  `th api smooth-operator`, and the bench driver. All three read the `error` frame
  with `as_str()` on what is actually an object, so every real server complaint
  surfaced as the literal string "unknown error"; the code is now included in the
  message. The bench's unit test asserted a frame shape the server never emits, so
  it passed while the driver was wrong — corrected to the real shape.
- An error frame only ends the turn it names. Unattributed protocol errors render
  as an inline warning and let the turn keep streaming.
