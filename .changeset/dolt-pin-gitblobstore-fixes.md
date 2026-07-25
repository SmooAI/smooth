---
'@smooai/smooth': patch
---

smooth-dolt: bump embedded dolt to 2026-07-23 main + driver v1.88.1 — picks up
two upstream gitblobstore fixes that were breaking pearls sync at scale:

1. "nbs: spool git-backed table files for chunk reads" — chunk reads used to
   spawn `git cat-file blob` and read-discard to the offset for EVERY ranged
   read; on a 43MB table file a fresh clone re-streamed the same blob
   thousands of times (effectively O(n²), observed as a clone that hangs
   forever). Table files are now spooled to disk once and served by seek.
2. "fix gitblobstore premature flush and prune" — live table files could be
   pruned from the remote tree, which is the likely root cause of the
   checksum/missing-file failures fresh clones hit against the shared remote.

Also switches `smooth-dolt clone` from DOLT_PULL to DOLT_FETCH: the fresh-init
root is always unrelated to the remote history, and newer dolt hard-errors
("no common ancestor") where older dolt silently skipped the merge. Fetch has
no merge step; the existing reset-to-remote-head already aligns `main`.

Verified: fresh clone of the 43MB production pearls store completes in ~12s
(previously never completed); 172 smooth-pearls tests pass.
