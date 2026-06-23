---
'smooai-smooth-daemon': patch
---

Phase 5 (EPIC th-c89c2a): the schedule management API. `GET /api/schedule`
lists scheduled tasks, `POST /api/schedule` creates one from
`{prompt, schedule}` (the `schedule` being a tagged `ScheduleKind`, e.g.
`{"kind":"daily_at","hour":8,"minute":0}` or
`{"kind":"every_n_seconds","secs":300}`; empty prompt → 400), and
`DELETE /api/schedule/{id}` removes one (204). New schedules are first due at
the next cadence point after now, so the scheduler tick picks them up. Tested
at the handler level (create/list/empty-reject/delete) and verified live (CRUD
round-trip, `next_due` resolving to the next 08:00). The `th` CLI + control-
surface UI follow.
