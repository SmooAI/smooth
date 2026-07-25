---
'@smooai/smooth': patch
---

Pearls Dolt: share the git-remote cache across worktrees to kill cold
full-history fetches (pearl th-20f330).

Dolt hardcodes its git-remote cache to the per-worktree DB dir
(`<db>/.dolt/git-remote-cache`), which is gitignored and rebuilt on
demand. Every fresh worktree/clone therefore started cold and re-fetched
the entire `refs/dolt/data` history from scratch — hundreds of MB,
byte-silent — while holding the single-writer noms LOCK, wedging every
other agent's pearl store read-only until the sync finished or timed out.
That was the root cause of the recurring "database is read only" wedges,
not a whole-monorepo clone (the fetch is a single `+refs/dolt/data`
refspec).

`th pearls push`/`pull` now symlink each worktree's cache dir to one
shared per-machine slot under `~/.smooth/git-remote-cache/<owner_repo>/`
(keyed by remote URL) before the remote op. The first bootstrap on a
machine is the only cold fetch; every later worktree and sync is
incremental (seconds). Data stays entirely in the repos — only the cache
moves. Best-effort and Unix-only for now; a stalled cold fetch can no
longer pin the store for minutes.
