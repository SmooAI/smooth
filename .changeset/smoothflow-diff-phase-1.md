---
'@smooai/smooth': minor
---

SmoothFlow Diff, phase 1 (th-26f5b9): the Diff tab is a real review tool. The flow engine serves structured diffs by agent turn (worktree snapshots at each turn boundary through a throwaway index), uncommitted, or against the default branch: files and hunks with word-level change spans and server-side syntax spans, noise collapsed, explicit truncation, and a 512 KiB frame budget with paging by file for the relay. New `flow.diff*` frames (and HTTP twins) revert, stage and unstage a single hunk, refusing stale ones, and send review comments to the agent as one batched steer. SmoothFlow for Mac replaces its `git diff` text dump with a native viewer: file tree with viewed marks, unified or side by side, Catppuccin syntax colors, per-hunk Revert and Stage, gutter comments, and j/k/n/p/]/[/v/c/r/s/u keys.
