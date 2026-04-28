---
"@smooai/smooth": patch
---

Fix `th pearls create` silently dropping writes from CLI mode

`smooth-dolt sql -q ...` ran every statement through Go's
`db.Query`, including writes (INSERT/UPDATE/DELETE). Dolt returns
`__ok_result__` for those, but the implicit transaction never
commits to the working set before the subprocess exits — Dolt
rolls it back. Result: `th pearls create`'s INSERT was silently
dropped, then `store.create`'s verify-after-create failed with
`pearl not found after create: th-XXXXXX` and the row was gone
from disk.

Server mode (`smooth-dolt serve`) had a separate `doExec`
(uses `db.Exec`, commits on close); CLI mode had no equivalent.

Fix:
- New `smooth-dolt exec <data-dir> -q "SQL"` subcommand that uses
  `db.Exec` and prints `<n> rows affected`.
- `SmoothDolt::exec` (Rust, CLI path) routes writes to the new
  subcommand instead of `sql`.

Verified: create-then-read across `th pearls create` → row appears
in subsequent `SELECT` from a fresh subprocess.
