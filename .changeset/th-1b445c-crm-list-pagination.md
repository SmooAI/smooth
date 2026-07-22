---
'@smooai/smooth': minor
---

Fix silent truncation in `th api crm contacts|companies|deals list`.

These commands only sent `?limit=` (default 50; the API clamps to a 200-row max page) with no offset and no auto-paginate, so they silently returned just the first page — a `contacts list --limit 1000` came back with 200 rows while the endpoint's `x-total-count` said 267, dropping 67 contacts with no warning. The endpoint was always fine; the CLI was the bug.

Each `list` command now takes `--offset <N>` (page past the first page) and `--all` (fetch every page, ignoring `--limit`/`--offset`). `--all` reuses the same offset-paginating loop the import scan already used — generalized into `fetch_all_resource` and paced to stay under the 100-req/60s rate limit. The deals `--stage` filter is not applied in `--all` mode (the endpoint filters by `stage`, not the paginator's `search`).
