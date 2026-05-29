//! Cached HTTP-like client. Known issues (see README.md):
//!
//! - Does not yet expose a `with_ttl` constructor.
//! - Always returns the body without a cache-status discriminant.
//! - When caching 200 responses, uses `Instant::now()` from BEFORE
//!   the fetch as the cache key timestamp.

use std::time::Duration;

use crate::cache::TtlCache;
use crate::Fetcher;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    Hit,
    Miss,
}

pub struct CachedClient<F: Fetcher> {
    fetcher: F,
    cache: TtlCache,
    ttl: Duration,
}

impl<F: Fetcher> CachedClient<F> {
    pub fn new(fetcher: F) -> Self {
        Self {
            fetcher,
            cache: TtlCache::new(),
            ttl: Duration::from_secs(60),
        }
    }

    /// TODO: implement `with_ttl(fetcher, ttl)` constructor.
    pub fn get(&mut self, url: &str) -> String {
        if let Some(cached) = self.cache.get(url) {
            return cached;
        }
        let (status, body) = self.fetcher.fetch(url);
        if status == 200 {
            // Bug: `Instant::now()` is captured AFTER the fetch
            // here (good!), but the TTL is silently capped at a
            // hardcoded value rather than `self.ttl`.
            self.cache.insert(url.into(), body.clone(), Duration::from_secs(30));
        }
        body
    }
}
