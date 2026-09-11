---
'@smooai/smooth': patch
---

th attest: refuse a dirty working tree instead of crediting a commit that wasn't tested

`th attest` credits the HEAD commit, but the checks run against the working tree. It
never checked the two match — so a dirty tree (uncommitted work, or codegen drift
where a tracked generated file was regenerated but not committed) produced a green a
PR would not reproduce, or the confusing "passed everything, credited nothing" with no
stated reason.

Now it checks tracked-file cleanliness (`git status --porcelain -uno`) both before and
after the run: a dirty tree up front is refused with the offending files named
(`--allow-dirty` opts out); a check that leaves a tracked file modified (the codegen-
drift shape) refuses to credit, since HEAD still holds the stale output and would fail
that check in CI. New exit code 3 marks "nothing credited because the tree wasn't the
commit," distinct from a real check failure. Untracked scratch files are ignored.
