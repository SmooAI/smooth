//! Public smoke tests. The real hidden suite is overlaid by the
//! grader after TASK_COMPLETE.

use std::time::Duration;

use rust_ttl_cache::TtlCache;

#[test]
fn cache_get_after_insert() {
    let mut c = TtlCache::new();
    c.insert("k".into(), "v".into(), Duration::from_secs(60));
    assert_eq!(c.get("k").as_deref(), Some("v"));
}

#[test]
fn cache_returns_none_for_missing() {
    let c = TtlCache::new();
    assert_eq!(c.get("missing"), None);
}
