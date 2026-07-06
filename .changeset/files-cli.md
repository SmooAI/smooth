---
"@smooai/smooth": minor
---

`th files` — CLI for the Smoo AI org file system (ADR-060). `ls` (folders + files in a folder or root), `mkdir`, `upload` (presigned PUT from a local path, MIME inferred from extension), `download` (presigned GET → local file), `mv`/`mvdir` (move and/or rename; `root` moves to the org root), `rm`/`rmdir`, `lock`/`unlock` (admin deletion lock on a file or folder), and shares: `share` (anonymous link with `--permission`/`--password`/`--expires-in-hours`/`--max-downloads`, prints the `smoo.ai/share/<token>` URL), `shares` (list), `unshare` (revoke), and `invite` (tracked email invite → `smoo.ai/share/recipient/<token>`). Available as both `th files …` and `th api files …`.
