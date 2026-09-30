---
'@smooai/smooth': patch
---

The `attest-push-hint` hook knows about attest-first CI (SMOODEV-3460).

In a repo whose PR CI is attest-first (it ships `scripts/ci/_attest-gate.mjs`, which smooai has done since SMOODEV-3458), an uncredited heavy check fails the PR instead of running. The hint there now says to push with `th attest --all`, and to add the `ci:full` PR label when a check can't run locally. It no longer tells agents to "pick what the diff touches" or that "attesting everything is usually wrong", because following that advice now produces a red PR. Repos without the gate keep the pick-your-checks guidance, now framed as an optimisation rather than a rule.
