---
'@smooai/smooth': minor
---

`th pearls sync` — two-way sync of the local pearl store with Smoo Projects work items (pearl th-19cca5). Why: pearls moved to a local SQLite file (th-d3e842) and lost Dolt's push/pull; the platform's work-items API is the shared home now, and Jira comes in through `smoo work jira import` instead of smooth-diver's direct client.

- `th pearls sync --project <KEY|uuid>` binds a checkout once (stored in the pearl store's config); then `th pearls sync [--pull-only|--push-only] [--dry-run] [--json] [--org]`.
- Pearl ↔ work item pairs live in a new `sync_map` table; pushed items carry `externalRef = th-xxxxxx@<checkout-dir>` so other machines adopt instead of duplicate.
- Field map: title/description/labels/parent; status `closed`↔`done`, `deferred`↔`blocked` (remote `in_review`/`cancelled` are never demoted); priority inverted (P0 ↔ 4); `epic` survives as a label; deps → `blocks` links; comments both ways with a `pearl-comment:<id>` marker to stop echoes.
- Last writer wins on `updated_at` against the last-sync baseline; conflicts are listed, never silent. Create-on-first-sight both ways; nothing is ever deleted on either side.
- `th pearls push` / `pull` notices now point at `th pearls sync`.
