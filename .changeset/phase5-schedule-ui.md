---
'smooai-smooth-web': patch
---

Phase 5 (EPIC th-c89c2a): control-surface schedule panel. The sidebar gains a
Schedules section that lists the agent's proactive tasks (cadence + prompt,
with a remove button) and an add form — a prompt field plus a compact cadence
input (`30m` for every-N-minutes, `08:00` for daily UTC), parsed client-side
(`parseCadence`) and validated before enabling Add. Backed by the existing
`/api/schedule` endpoints (`daemon.ts` gains typed `listSchedules` /
`createSchedule` / `deleteSchedule`). This completes the scheduler across all
three surfaces — API, `th` CLI, and the web control surface.
