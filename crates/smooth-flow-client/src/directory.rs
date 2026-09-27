//! The New Session Directory field's pure helpers (spec §6).

/// `/Users/me/dev/x` → `~/dev/x`; `home` itself → `~`. A path that merely
/// starts with the same characters (`/Users/meta`) is left alone.
#[must_use]
pub fn abbreviate(path: &str, home: &str) -> String {
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// What typing a path means: `~` and `~/x` expand against `home`, an absolute
/// path is used as-is (trimmed), and anything else is a search, not a path.
#[must_use]
pub fn expanded_path(typed: &str, home: &str) -> Option<String> {
    let t = typed.trim();
    if t == "~" {
        return Some(home.to_string());
    }
    if let Some(rest) = t.strip_prefix("~/") {
        return Some(format!("{home}/{rest}"));
    }
    t.starts_with('/').then(|| t.to_string())
}

/// The highlighted match after an arrow key, clamped to the list.
#[must_use]
pub fn moved(index: usize, delta: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    index.saturating_add_signed(delta).min(count - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_arrows() {
        assert_eq!(abbreviate("/Users/me/dev/x", "/Users/me"), "~/dev/x");
        assert_eq!(abbreviate("/Users/me", "/Users/me"), "~");
        assert_eq!(abbreviate("/Users/meta/x", "/Users/me"), "/Users/meta/x");
        assert_eq!(expanded_path("~/dev", "/Users/me").as_deref(), Some("/Users/me/dev"));
        assert_eq!(expanded_path(" /tmp/x ", "/Users/me").as_deref(), Some("/tmp/x"));
        assert_eq!(expanded_path("smooth", "/Users/me"), None);
        assert_eq!(moved(0, -1, 3), 0);
        assert_eq!(moved(2, 1, 3), 2);
        assert_eq!(moved(1, 1, 3), 2);
        assert_eq!(moved(0, 1, 0), 0);
    }
}
