---
'smooai-smooth-daemon': patch
---

Phase 5 (EPIC th-c89c2a): the scheduler tick — what makes the always-on agent
*proactive*. A background loop (`spawn_scheduler`, spawned from
`serve_persistent`) wakes every 30s, asks the `ScheduleStore` which schedules
are due, fires each one's prompt into a per-schedule `schedule:{id}` session
via the same coordinator + `run_task` path a live client uses, then advances
the schedule's `next_due`. Scheduled runs have no connected client so their
events are drained (they still persist to the durable event log + conversation
history, recoverable via `/api/session`). The tick logic is split from the
loop and tested: a due schedule fires (records `last_run`, advances past now)
and gets its session, while a not-yet-due one is untouched. The `th`/API
surface to create/list/remove schedules follows.
