---
'smooai-smooth-daemon': patch
---

EPIC th-c89c2a (th-2ff975): restore the schedule model as the foundation for
operator-driven proactivity. `ScheduleKind` (EveryNSeconds / DailyAt), `Schedule`
(prompt + cadence + next-due + enabled), the `ScheduleStore` trait, and
`InMemoryScheduleStore` are back (self-contained, 4 tests) after being deleted with
the bespoke serve_persistent loop. Architecture for the loop ahead: the daemon
fires due schedules by acting as a **WS client of its own operator** (the public
`handle_frame` + canonical `send_message` protocol) — proactivity is "just another
client," on-message with the north star. Next: a durable store + the tick loop.
