---
'@smooai/smooth': minor
---

`th ci-queue` now admits jobs by capacity, not just by slot count (SMOODEV-3355). Within the per-class slot ceiling, a job is admitted only while its estimate fits the machine. The estimate is the p90 of its label's measured peak process-group memory and its mean cores, or the class default until there is history. The memory budget is available memory minus a reserve for the UI and sessions. The CPU budget is cores × `cpu_factor`. An AIMD scale adjusts both: it grows while pressure stays calm and halves on a spike. With nothing running, the head job is always admitted. Smaller jobs may pass a big waiter at most `max_passes` times, and then it holds a reservation. History moves to SQLite at `~/.smooth/ci-queue/history.db`, which records peak group RSS, max single RSS, CPU time, the estimate, and the pressure at admission. `status` shows a Budget section and each running job's live memory. The `--json` schema is now 2.
