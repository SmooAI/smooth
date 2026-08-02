---
"@smooai/smooth": patch
---

ADR-006: durable execution — extend `SqliteScheduleStore`, don't adopt apalis (th-2bcd7f)

Records the "local Temporal" decision for Big Smooth. apalis is a job queue, not durable execution — it gives at-least-once delivery and retry/backoff, not Temporal's deterministic replay — and replay is the wrong model for an agent turn anyway (LLM sampling is non-deterministic and side-effecting; the fitting primitive is checkpointing, which the engine already models). Adopting `apalis-sql` would also pull a second SQLite driver via sqlx into a binary that deliberately links exactly one, reversing two prior on-the-record decisions. The gaps that actually exist are five scheduling features (one-shot schedules, cron + timezone, catch-up policy, a bounded retry budget reusing the th-e979ac backoff shape, firing outcomes) plus one durability gap in the daemon's checkpoint store, which is still `MemoryCheckpointStore`. All of them are fields and match arms on existing types, not a dependency. Names the concrete triggers that would re-open the decision.
