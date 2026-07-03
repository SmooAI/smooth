---
'@smooai/smooth': patch
---

`th pearls doctor`'s remote-sync section now runs a cheap tip-level check before the deep probe clone. Four signals answer the common case without cloning: local dolt branch head vs the remote-tracking head (any unpushed commits?), and the last-synced `refs/dolt/data` tip (from dolt's git-remote-cache `FETCH_HEAD`) vs a bounded `git ls-remote` (has the remote ref moved?). All-in-sync → verdict in ~1s with no clone; anything else falls through to the existing probe-clone classification. On a 2547-commit store the probe clone is ~5 minutes at 96% CPU, which always exceeded the default 30s bound — doctor previously skipped the comparison entirely on large stores.
