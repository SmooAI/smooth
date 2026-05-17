---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 3 iter-5a. New
`smooth_pearls::memory` module providing CRUD over the
`memories` table that's existed in the pearl Dolt schema
since day one but had no API.

`MemoryStore::new(SmoothDolt)` constructor; methods:

* `append(content, source)` — insert with a fresh
  `mem-XXXXXX` id; rejects empty content.
* `list_recent(limit)` — newest-first, capped.
* `list_by_source(source, limit)` — filter to a specific
  origin tag (a pearl id, an operator id, `"manual"`, …).
* `count()` / `clear_by_source(source)` / `clear_older_than(cutoff)`.

The `source` field is the join key that lets us recall
"everything the agent learned working on `th-abc123`" or
"everything written by operator-7". Append-only API on
purpose; pruning is bulk-by-source or bulk-by-age.

SQL quoting via single-quote doubling (Dolt's CLI doesn't
expose prepared statements). 8 new tests cover round-trips,
filter-by-source, limit honoring, both clear paths, the
empty-content guard, and a single-quote-in-content insert.

Dolt's `DATETIME` column has 1-second resolution so two
inserts within the same second tie on ordering; documented
in the API + tests cover both the ≥1s-apart case (strict
ordering) and the same-second case (every row retrievable
but order unspecified). Production callers write seconds /
minutes apart so this isn't a real constraint — long-term
we'd switch to TIMESTAMP(3) or a sortable insert sequence.

iter-5b will wire this into the dispatch path so the agent
sees recent notes on task start and can write new ones on
completion.
