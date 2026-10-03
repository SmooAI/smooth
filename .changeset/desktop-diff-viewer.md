---
'@smooai/smooth': patch
---

SmoothFlow Desktop gets the Diff tab (th-26f5b9, th-d89238). The Linux/Windows GPUI app now has Terminal | Diff center tabs, gated by the worktree like the Mac. The Diff tab is a virtualized review viewer over the engine's `flow.diff`: the file tree with status badges, counts and viewed marks; unified or side-by-side lines with the engine's syntax and word spans in Catppuccin Mocha; the Last turn / Uncommitted / vs branch picker; noise files collapsed with Show; big diffs paged in per file as they scroll into view; per-hunk Revert (confirmed, Cancel by default), Stage and Unstage; line, range and file comments sent as one `flow.diff.review`; refresh on `flow.diff.changed`; and the shared `j`/`k`, `n`/`p`, `]`/`[`, `v`, `c`, `r`, `s`, `u` keys. `stale` and `blocked` refusals are shown verbatim and never retried.
