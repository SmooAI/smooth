---
'@smooai/smooth': patch
---

`pnpm install:th` now makes the binary it just built actually win on `PATH`.

The menu bar's "Install th CLI…" symlinks `/usr/local/bin/th` (or
`~/.local/bin/th`) at `Big Smooth.app/Contents/Resources/bin/th`, and those
directories usually precede `~/.cargo/bin`. A successful dev install therefore
kept serving the older bundled binary — you'd debug a stale `th` and conclude
your change hadn't worked.

`install:th` now ends with `scripts/dev-link-th.sh`, which repoints that
symlink at the fresh build. It only ever rewrites a symlink; a regular file
(Homebrew, a manual copy) gets a warning and is left alone. Opt out with
`SMOOTH_NO_DEV_LINK=1`.
