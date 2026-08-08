---
'smooth': patch
---

bench: judge `expect_fail` across trials, not per trial

Measured at `--trials 3`, the documented interrupt gap came back XFAIL, XFAIL, XPASS —
it is not broken, it is flaky. Judged per-trial, that single XPASS failed the whole run,
so a ~1-in-3 flake would turn CI red at random. A benchmark that cries wolf gets ignored.

An `expect_fail` scenario now counts as XPASS ("the gap is closed, drop the flag") only
when it passed on every conclusive trial. A partial pass is reported as a flaky gap with
its rate and keeps the suite green. An inconclusive trial still fails: missing data must
never read as a satisfied expectation.
