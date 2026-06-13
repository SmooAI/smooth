---
"smooai-smooth": patch
---

th pearls: skip the git auto-commit of pearl state when run from a linked worktree

`th pearls` mutations auto-commit the `.smooth/dolt/` store to git so pearl
state syncs across machines. Dolt rewrites its mutable pointer files
(`journal.idx`, `manifest`, the journal chunk) on every store open, and each
linked worktree checks out its own copy — so committing those onto a feature
branch produced binary pointer divergence that couldn't be merged back to
main (recurring `.smooth/dolt` conflicts).

`auto_commit_pearl_state` now detects a linked worktree (`git rev-parse
--git-dir` ≠ `--git-common-dir`) and skips the git commit there, logging a
hint to run pearl mutations from the primary worktree. The dolt mutation and
`th pearls push` (refs/dolt/data) still capture the change, so nothing is
lost — pearl state simply stays on one lineage. Primary-worktree behaviour is
unchanged.
