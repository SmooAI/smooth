//! Per-pearl host-port cache for `forward_port`.
//!
//! Lives at `~/.smooth/port-forwards/<pearl_id>.json`. Maps guest port
//! → host port so successive tasks on the same pearl get the same host
//! port assignments back (assuming the host port is still free).
//!
//! This lets the user "check on the dev server tomorrow" without the
//! mapping changing under them. If a remembered host port is already in
//! use by another process, we skip it and let the dispatcher pick a
//! fresh ephemeral port.

use std::collections::HashMap;
use std::path::PathBuf;

/// File format: flat guest_port → host_port map serialized as JSON.
type CacheMap = HashMap<u16, u16>;

fn cache_dir() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".smooth").join("port-forwards"))
}

fn cache_file(pearl_id: &str) -> Option<PathBuf> {
    cache_dir().map(|d| d.join(format!("{pearl_id}.json")))
}

/// Load the cached mapping for `pearl_id`. Returns an empty map if the
/// file doesn't exist or is malformed.
#[must_use]
pub fn load(pearl_id: &str) -> CacheMap {
    let Some(path) = cache_file(pearl_id) else {
        return CacheMap::new();
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return CacheMap::new();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// Persist `map` for `pearl_id`. Best-effort: errors are logged and
/// swallowed — a failed write doesn't break task dispatch.
pub fn save(pearl_id: &str, map: &CacheMap) {
    let Some(path) = cache_file(pearl_id) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            tracing::warn!(path = %parent.display(), "port_cache: failed to create cache dir");
            return;
        }
    }
    match serde_json::to_string_pretty(map) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!(pearl_id, error = %e, "port_cache: failed to save mapping");
            }
        }
        Err(e) => tracing::warn!(pearl_id, error = %e, "port_cache: serialize failed"),
    }
}

/// Try to bind a specific host port. Returns `Some(port)` if the bind
/// succeeded (and was immediately released so the caller can rebind),
/// `None` if the port was unavailable.
#[must_use]
pub fn try_reserve(host_port: u16) -> Option<u16> {
    match std::net::TcpListener::bind(("127.0.0.1", host_port)) {
        Ok(listener) => {
            let addr = listener.local_addr().ok()?;
            drop(listener);
            Some(addr.port())
        }
        Err(_) => None,
    }
}

/// Ask the kernel for a fresh ephemeral host port.
#[must_use]
pub fn reserve_ephemeral() -> Option<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let addr = listener.local_addr().ok()?;
    drop(listener);
    Some(addr.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_returns_empty() {
        let map = load("nonexistent-pearl-aaaaaaaa");
        assert!(map.is_empty());
    }

    #[test]
    fn save_and_load_roundtrip() {
        // Use a pearl id that's uniquely ours so we don't clobber real
        // state in ~/.smooth/.
        let pearl_id = "test-pearl-roundtrip-z9y8";
        let mut map = CacheMap::new();
        map.insert(3000, 54321);
        map.insert(5173, 54322);
        save(pearl_id, &map);

        let loaded = load(pearl_id);
        assert_eq!(loaded.get(&3000), Some(&54321));
        assert_eq!(loaded.get(&5173), Some(&54322));

        // Clean up.
        if let Some(path) = cache_file(pearl_id) {
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn reserve_ephemeral_returns_valid_port() {
        let port = reserve_ephemeral().expect("should reserve");
        assert!(port >= 1024, "ephemeral port must be >=1024, got {port}");
    }
}
