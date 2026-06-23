---
'smooai-smooth-daemon': patch
---

Phase 5 (EPIC th-c89c2a): begin scheduled/proactive tasks — the hermes-style
"do this every morning / every N minutes" capability. This first slice is the
pure model + timing core: a `ScheduleKind` (`EveryNSeconds`, `DailyAt` in UTC)
with strictly-after `next_after` computation, and a `Schedule` with
`is_due`/`mark_fired` lifecycle. No storage or tick loop yet (those follow), so
the timing logic is exhaustively unit-tested without a clock or DB —
interval advance, daily today-vs-tomorrow (incl. the exactly-at-time edge),
component clamping, the due/advance lifecycle, and serde round-trip.
