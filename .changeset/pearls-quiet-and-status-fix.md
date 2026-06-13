---
"smooai-smooth": patch
---

th pearls: quiet auto-commit under beads model + fix smooth-dolt status

Two follow-ups to pearl `th-975dfe` (beads-model migration):

**Pearl `th-016296`**: `auto_commit_pearl_state` now detects that
`.smooth/dolt/` is git-ignored (via `git check-ignore -q`) and
silent-noops instead of erroring on `git add .smooth/dolt/` with
"use -f to force-add ignored files". Sync stays via `th pearls push`
to `refs/dolt/data`; no git commits are needed for the on-disk store
under the beads model. Repos that haven't migrated yet (still track
`.smooth/dolt/`) keep the legacy auto-commit path.

**Pearl `th-f6c50c`**: `smooth-dolt status` previously called
`CALL DOLT_STATUS()` which errored with "stored procedure does not
exist". DOLT_STATUS is a *system table* in Dolt, not a procedure or
table function. Fix: `SELECT table_name, staged, status FROM
dolt_status` in both the CLI handler (`cmdStatus`) and the
socket-mode handler (`doDoltCmd`). Clean working set → empty output
(preserves the pre-commit hook's `.trim().is_empty()` contract);
changed tables → one line per row.

4 new tests covering `is_dolt_gitignored` (true / false / non-git)
and `auto_commit_silent_noop_when_dolt_gitignored`.
