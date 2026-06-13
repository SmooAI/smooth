---
"smooai-smooth": minor
---

Pearls: migrate to beads model — `.smooth/dolt/` no longer git-tracked

Pearl `th-975dfe`. Reverses an early decision (called out explicitly in
the prior `.gitignore` comment: "we WANT [.smooth/dolt/.dolt/]
committed — git is how pearls sync between machines") that produced a
recurring class of merge conflicts: Dolt rewrites the noms mutable
pointer files (`manifest`, `journal.idx`, `*.darc`, journal-chunk) on
every store open; git can't 3-way-merge binaries; main moving forward
while a feature worktree was open meant the conflict-on-merge-back
pattern recurred constantly. PR #94 (linked-worktree auto-commit
guard) and smooai #1513 (pre-commit `git add -A` exclusion) addressed
the worktree-as-author side but not the main-moves-forward side.

Beads precedent: `.beads/embeddeddolt/` is gitignored; sync happens
via dolt's custom `refs/dolt/data` ref pushed alongside normal git
refs (`bd dolt push`/`pull`). The ref-based sync was always available
in `th pearls`; this PR just stops materializing the on-disk noms
files in git's tracked set.

**Changes**:

- `.gitignore`: add `.smooth/dolt/`. Old comment that said "we WANT
  this committed" replaced with the beads-model rationale.
- `git rm -r --cached .smooth/dolt/`: untrack the 7 currently-tracked
  files from the index. History is preserved (history isn't rewritten);
  new commits no longer sweep noms churn into git.
- `th pearls init`:
  - Ensures `.smooth/dolt/` is in `.gitignore` (idempotent — matches
    against `.smooth/dolt`, `/.smooth/dolt/`, `.smooth/dolt/**`).
  - On post-`git clone` bootstrap (no local store + git origin URL
    available), runs `smooth-dolt clone <origin> .smooth/dolt/` to
    populate from `refs/dolt/data`. Falls back to empty init if the
    clone fails. No manual `th pearls pull` needed.
- `smooth-pearls`: new `dolt::clone_from(remote_url, target_dir)`
  public helper. Mirrors `recover_from_remote`'s subprocess shape but
  takes the URL as an argument instead of reading it from
  `repo_state.json` (which doesn't exist yet on a fresh bootstrap).
- `CLAUDE.md` §5: documents the new model + implications.
- 8 new tests covering `ensure_dolt_gitignored` (idempotency,
  wildcard variant detection, anchored leading-slash variant) and
  `read_git_origin_url` (none / present / non-git dir).

**Other repos** (smooai, smooblue) get their own follow-up migration
PRs (pearls `th-482e14`, `th-ad1f41`). After all three, the
`pearls-dolt-git-conflicts` memory's "How to apply" workarounds drop
entirely.
