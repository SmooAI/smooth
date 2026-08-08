---
'@smooai/smooth': patch
---

bench: nightly engine-parity regression gate

`operator-serve.sh smoke` only checks that an engine listens — both currently-broken
engines pass it. A new scheduled workflow runs real scenarios against all five
smooth-operator engines and compares them to `docs/engine-baseline.json`.

The baseline encodes expected state rather than aspiration: two engines are broken
today (th-11284c, th-df7007), and a gate demanding 100% everywhere would be red
forever and therefore ignored. It fails on regression and reports loudly when a
known-broken engine starts working, so the exception gets removed instead of becoming
permanent. A crashing engine writes no scoreboard, and absence is counted as
not-passing rather than skipped.
