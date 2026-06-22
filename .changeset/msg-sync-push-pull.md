---
"@smooai/smooth": patch
---

feat: `th msg`/`th agent` sync over `refs/dolt/data` (push-on-send, pull-on-watch)

Messages live in the pearl Dolt store, which syncs over the repo's git remote
via `refs/dolt/data` — but `th msg send` previously only committed locally, so
agents in different clones/machines of the same repo didn't see each other
until a manual `th pearls push`. Now the messaging commands sync automatically:

- `th msg send` / `th msg reply` / `th agent register` / `th agent offline`
  **push** after committing (`--no-push` to skip).
- `th msg watch` **pulls** each poll by default (`--no-pull` for a local-only,
  offline mailbox).
- `th msg inbox --pull` fetches the remote before listing.

Sync drives only `dolt push` / `dolt pull` (which capture their own output), so
a repo with no remote — or the global `~/.smooth/dolt` store, or being offline
— is a silent no-op: no error, no stray `fatal: No configured push destination`
on stderr. Pearl th-bdaaa7.
