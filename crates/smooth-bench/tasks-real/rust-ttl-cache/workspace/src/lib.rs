//! rust-ttl-cache — a tiny per-URL TTL cache wrapped around a
//! pluggable `Fetcher`.

pub mod cache;
pub mod client;

pub use cache::TtlCache;
pub use client::{CacheStatus, CachedClient};

/// Pluggable fetcher trait. Real production code would use `reqwest`
/// here; we use a trait so tests can swap in a recording fake.
pub trait Fetcher {
    /// Fetch `url`. Returns `(status, body)`.
    fn fetch(&self, url: &str) -> (u16, String);
}
