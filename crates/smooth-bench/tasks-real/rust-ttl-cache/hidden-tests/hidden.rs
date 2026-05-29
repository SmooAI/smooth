//! Held-out tests. The grader overlays this file at
//! `tests/hidden.rs` in the scratch workspace after the agent has
//! signalled TASK_COMPLETE, then runs `cargo test`.

use std::time::Duration;

use rust_ttl_cache::{CacheStatus, CachedClient, Fetcher, TtlCache};

struct RecordingFetcher {
    calls: std::cell::RefCell<Vec<String>>,
    body: String,
    status: u16,
}

impl RecordingFetcher {
    fn new(status: u16, body: &str) -> Self {
        Self {
            calls: std::cell::RefCell::new(Vec::new()),
            body: body.into(),
            status,
        }
    }
    fn call_count(&self) -> usize {
        self.calls.borrow().len()
    }
}

impl Fetcher for RecordingFetcher {
    fn fetch(&self, url: &str) -> (u16, String) {
        self.calls.borrow_mut().push(url.into());
        (self.status, self.body.clone())
    }
}

#[test]
fn cache_hit_after_first_fetch() {
    let f = RecordingFetcher::new(200, "hello");
    let mut c = CachedClient::with_ttl(f, Duration::from_secs(60));
    let (body1, status1) = c.get_status("https://x");
    assert_eq!(body1, "hello");
    assert_eq!(status1, CacheStatus::Miss);
    let (body2, status2) = c.get_status("https://x");
    assert_eq!(body2, "hello");
    assert_eq!(status2, CacheStatus::Hit);
}

#[test]
fn ttl_zero_means_no_cache() {
    let f = RecordingFetcher::new(200, "hello");
    let mut c = CachedClient::with_ttl(f, Duration::from_secs(0));
    let _ = c.get_status("https://x");
    let (_body, status) = c.get_status("https://x");
    assert_eq!(status, CacheStatus::Miss, "0-duration TTL must not cache");
}

#[test]
fn non_200_not_cached() {
    let f = RecordingFetcher::new(404, "nope");
    let mut c = CachedClient::with_ttl(f, Duration::from_secs(60));
    let (_b1, s1) = c.get_status("https://x");
    let (_b2, s2) = c.get_status("https://x");
    assert_eq!(s1, CacheStatus::Miss);
    assert_eq!(s2, CacheStatus::Miss);
}

#[test]
fn evict_expired_is_constant_time_on_hot_path() {
    // Loose proxy: insert a lot of entries with very short TTLs,
    // wait for them all to expire, then time a single insert. If
    // the new implementation truly avoids a linear scan when
    // nothing is expired, this is fast even with 10k entries.
    let mut cache = TtlCache::new();
    for i in 0..1000 {
        cache.insert(format!("k{i}"), "v".into(), Duration::from_secs(3600));
    }
    let t = std::time::Instant::now();
    cache.insert("hot".into(), "v".into(), Duration::from_secs(3600));
    let elapsed = t.elapsed();
    assert!(elapsed.as_millis() < 50, "insert took {elapsed:?} — eviction path is still O(n)?");
}
