---
'@smooai/smooth': patch
---

SmoothFlow's Diff and PR tabs now follow the worktree, not the session kind (th-68d10a). A shell in a worktree has a branch, a diff and maybe a PR, so it gets both tabs; a shell outside any repo gets neither. PR stays visible whenever there's a branch and shows "No PR for this branch yet" instead of vanishing. Activity is the one kind-gated tab, on the harness manifest's `state.source`: `hooks`/`native` get the full event log, `scrape` a thin view, and a shell none. A tab the session can't use is disabled with a tooltip saying why, never hidden. The Diff tab now diffs the working tree against the merge base with the default branch, so it shows the branch's whole change, not just uncommitted edits.
