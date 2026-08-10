---
'@smooai/smooth': minor
---

`th attest` — run a repo's CI checks here and credit the passes, so CI skips them.

A generic port of smooai's `scripts/ci/attest.sh`. Checks stay the repo's own
`scripts/ci/<name>.sh`; `th attest` knows nothing about any particular repo's.
Each pass posts a `ci-attest/<name>` commit status the workflow reads to skip
that row, and the order is run → push → credit, because every CI row reads the
statuses once ~20s in and a push-first run loses that race.

The behaviour worth naming is the third outcome: exit 97 means the check could
not START, so nothing is posted at all. A status is a claim about the commit;
"your Docker daemon is off" is a claim about the laptop. So the runner repairs
what it can (starts a container runtime it finds, installs missing node_modules
with `--frozen-lockfile`, appends the tool dirs a non-login shell loses from
PATH) and distrusts what it cannot: a failure on a machine above 2x its core
count is bucketed as could-not-run, with the load sampled BEFORE each check —
on a box dedicated to attesting the check IS the load, so sampling after would
swallow every genuine failure.

Also new: `--remote <host>` delegates expensive checks to a build box
concurrently with local ones, over a `refs/attest/<sha>` ref that triggers no
workflows. The delegating process keeps the GitHub credentials and the decision;
ssh's own failures, a failed checkout and a full remote disk all map to 97,
never to a verdict.
