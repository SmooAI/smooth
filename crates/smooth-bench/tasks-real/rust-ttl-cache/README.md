# rust-ttl-cache

## Background

You're working on a tiny HTTP-client crate. It wraps an upstream
`Fetcher` with a per-URL cache so repeated calls to the same URL within
a TTL window return the cached body without hitting the network. The
crate is intentionally small — about 80 lines split across two files.

## Files

- `src/lib.rs` — re-exports + the `Fetcher` trait definition.
- `src/cache.rs` — the `TtlCache` you'll be fixing + extending.
- `src/client.rs` — the `CachedClient` that uses `TtlCache`.
- `tests/cache.rs` — public smoke tests (a few). The **real** test
  suite is held out and applied by the grader.

## What's wrong

1. **`TtlCache::evict_expired` is O(n) per insert.** It scans the
   whole map on every `insert`. Look at how `insert` is currently
   implemented and replace the linear scan with a strategy that only
   touches expired entries (a small BinaryHeap or a sorted Vec of
   `(expires_at, key)` pairs is fine; pick something idiomatic).
2. **`CachedClient::get` doesn't use the cache for non-200 responses.**
   That's intentional, but it currently also fails to cache 200s
   correctly when the body is empty — the cached entry is stored with
   a stale `expires_at` (it uses `Instant::now()` from BEFORE the
   network call, not after).

## What to add

- A `with_ttl(ttl: Duration)` constructor on `CachedClient`.
- Make `CachedClient` return a `CacheHit | CacheMiss` discriminant
  alongside the body so callers can observe cache behaviour. The
  hidden tests will check this.

## How you should work

Run `cargo test` often. The hidden test suite is small but specific;
you'll know quickly whether your fix lands. Don't add new dependencies
— everything you need is already in `Cargo.toml`.

When you're confident, say `TASK_COMPLETE` and the grader will overlay
the hidden tests and run them.
