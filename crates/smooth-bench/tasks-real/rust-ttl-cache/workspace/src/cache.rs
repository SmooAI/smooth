//! TTL cache. **Known issues** (you're here to fix them):
//!
//! - `evict_expired` is called on every `insert` and does an O(n) scan
//!   of the entire map. Replace with something that only touches
//!   actually-expired keys.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct Entry {
    value: String,
    expires_at: Instant,
}

#[derive(Debug, Default)]
pub struct TtlCache {
    entries: HashMap<String, Entry>,
}

impl TtlCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `value` for `key` with a per-call TTL. Triggers an
    /// O(n) sweep — see module docs.
    pub fn insert(&mut self, key: String, value: String, ttl: Duration) {
        self.evict_expired();
        let expires_at = Instant::now() + ttl;
        self.entries.insert(key, Entry { value, expires_at });
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let entry = self.entries.get(key)?;
        if Instant::now() < entry.expires_at {
            Some(entry.value.clone())
        } else {
            None
        }
    }

    /// Linear scan — the bug. Replace.
    fn evict_expired(&mut self) {
        let now = Instant::now();
        let to_drop: Vec<String> = self.entries.iter().filter(|(_k, e)| e.expires_at <= now).map(|(k, _)| k.clone()).collect();
        for k in to_drop {
            self.entries.remove(&k);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
