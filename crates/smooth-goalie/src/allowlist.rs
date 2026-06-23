//! Egress allowlist + hostname normalization (EPIC th-c89c2a Phase 3, P0 #3).
//!
//! The security-critical core of the daemon's egress boundary: an in-process,
//! **exact-host** allowlist with a single, strict hostname parser. No Wonk
//! round-trip (that was the per-VM design) — the always-on daemon's proxy calls
//! this directly.
//!
//! The parser is the load-bearing piece. Host-allowlist bypasses are almost
//! always a *normalization* mismatch — the classic being a null byte or
//! non-ASCII label that one parser stops at and another doesn't
//! (`attacker.com\x00.google.com`, the SOCKS5 CVE-2025-55284 class). So
//! [`normalize_hostname`] rejects anything that isn't a clean ASCII DNS name
//! **before** the membership check, and the allowlist holds **exact hosts only**
//! — no wildcards, so `*.github.com` can never silently widen the surface.

use std::collections::HashSet;

/// Normalize a hostname to its canonical comparison form, or `None` if it is
/// not a syntactically valid ASCII DNS hostname.
///
/// Rejections (each a real bypass primitive): empty input, embedded NUL or any
/// non-ASCII byte, a port/scheme/path delimiter (`:` `/` `@` `\`), characters
/// outside `[a-z0-9.-]`, and malformed label structure (leading/trailing/double
/// dots, empty/over-long labels, over-long name). A single trailing dot (the
/// FQDN root) is stripped; the result is lowercased.
#[must_use]
pub fn normalize_hostname(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    // Strip exactly one trailing FQDN-root dot, then reject any name that still
    // ends with a dot (i.e. the original had a double trailing dot).
    let host = raw.strip_suffix('.').unwrap_or(raw);
    if host.is_empty() || host.ends_with('.') {
        return None;
    }
    // ASCII only — a single non-ASCII or NUL byte is an immediate reject (no
    // IDNA, no lossy decode; those are exactly where parsers diverge).
    if !host.is_ascii() {
        return None;
    }
    let lower = host.to_ascii_lowercase();
    // Reject anything carrying a port, scheme, userinfo, path, or wildcard —
    // the caller must hand us a bare host.
    if lower.contains([':', '/', '@', '\\', '*', '?', '#', ' ', '\t']) || lower.contains('\0') {
        return None;
    }
    if lower.len() > 253 {
        return None;
    }
    // Validate each DNS label.
    for label in lower.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None; // empty label = leading/trailing/double dot
        }
        if label.starts_with('-') || label.ends_with('-') {
            return None;
        }
        if !label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return None;
        }
    }
    Some(lower)
}

/// An exact-host egress allowlist.
///
/// Construction normalizes and drops any entry that isn't a clean exact host
/// (wildcards, ports, malformed names), so a bad config can only ever *narrow*
/// what's reachable, never widen it.
#[derive(Debug, Clone, Default)]
pub struct EgressAllowlist {
    hosts: HashSet<String>,
}

impl EgressAllowlist {
    /// Build from raw entries, returning the allowlist plus the entries that
    /// were **rejected** (so the caller can log them — a silently dropped
    /// allowlist entry is its own footgun).
    #[must_use]
    pub fn from_entries<I, S>(entries: I) -> (Self, Vec<String>)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut hosts = HashSet::new();
        let mut rejected = Vec::new();
        for entry in entries {
            let raw = entry.as_ref();
            match normalize_hostname(raw) {
                Some(h) => {
                    hosts.insert(h);
                }
                None => rejected.push(raw.to_owned()),
            }
        }
        (Self { hosts }, rejected)
    }

    /// Whether `raw_host` is allowed. The query host is normalized through the
    /// *same* parser, so a normalization mismatch can't sneak past.
    #[must_use]
    pub fn is_allowed(&self, raw_host: &str) -> bool {
        normalize_hostname(raw_host).is_some_and(|h| self.hosts.contains(&h))
    }

    /// Number of distinct allowed hosts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hosts.len()
    }

    /// Whether the allowlist is empty (deny-all).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_and_trailing_dot() {
        assert_eq!(normalize_hostname("GitHub.Com").as_deref(), Some("github.com"));
        assert_eq!(normalize_hostname("github.com.").as_deref(), Some("github.com"));
        assert_eq!(normalize_hostname("API.GitHub.com.").as_deref(), Some("api.github.com"));
    }

    #[test]
    fn rejects_null_byte_and_non_ascii_smuggling() {
        // The CVE-2025-55284 class: a NUL or unicode label that one parser
        // truncates at and another reads through.
        assert_eq!(normalize_hostname("attacker.com\u{0}.google.com"), None);
        assert_eq!(normalize_hostname("google.com\u{0}"), None);
        assert_eq!(normalize_hostname("аllowed.com"), None, "Cyrillic 'а' must not pass as ascii 'a'");
        assert_eq!(
            normalize_hostname("xn--e1afmkfd.com").as_deref(),
            Some("xn--e1afmkfd.com"),
            "punycode IS valid ascii"
        );
    }

    #[test]
    fn rejects_ports_schemes_paths_and_wildcards() {
        for bad in [
            "github.com:443",
            "https://github.com",
            "github.com/path",
            "user@github.com",
            "github.com\\evil",
            "*.github.com",
            "github.com?x=1",
            "git hub.com",
            "",
        ] {
            assert_eq!(normalize_hostname(bad), None, "{bad:?} must be rejected");
        }
    }

    #[test]
    fn rejects_malformed_label_structure() {
        for bad in [".github.com", "github.com.", "github..com", "-github.com", "github-.com", "..", "."] {
            // (trailing single dot is stripped, so "github.com." is actually OK;
            // assert only the genuinely malformed ones)
            if bad == "github.com." {
                continue;
            }
            assert_eq!(normalize_hostname(bad), None, "{bad:?} must be rejected");
        }
        // Over-long label (64 chars) and over-long name.
        let long_label = "a".repeat(64);
        assert_eq!(normalize_hostname(&format!("{long_label}.com")), None);
    }

    #[test]
    fn allowlist_is_exact_no_wildcard_widening() {
        let (allow, rejected) = EgressAllowlist::from_entries(["github.com", "API.GitHub.com.", "*.evil.com", "bad:host"]);
        // Wildcard + port entries are dropped, not honored.
        assert_eq!(rejected.len(), 2, "wildcard and port entries rejected: {rejected:?}");
        // The two valid entries are distinct exact hosts.
        assert_eq!(allow.len(), 2);
        assert!(allow.is_allowed("github.com") && allow.is_allowed("api.github.com"));
        // The wildcard did NOT widen the surface to arbitrary *.evil.com hosts.
        assert!(!allow.is_allowed("anything.evil.com") && !allow.is_allowed("evil.com"));
    }

    #[test]
    fn allowlist_membership_normalizes_the_query() {
        let (allow, _) = EgressAllowlist::from_entries(["github.com", "api.github.com"]);
        assert!(allow.is_allowed("github.com"));
        assert!(allow.is_allowed("GitHub.com."), "query is normalized through the same parser");
        assert!(allow.is_allowed("api.github.com"));
        // Exact only — a sibling/parent/child subdomain is NOT implied.
        assert!(!allow.is_allowed("evil.github.com"));
        assert!(!allow.is_allowed("notgithub.com"));
        // Smuggling attempts are denied because normalization fails first.
        assert!(!allow.is_allowed("github.com\u{0}.evil.com"));
        assert!(!allow.is_allowed("github.com:443"));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let (allow, _) = EgressAllowlist::from_entries(Vec::<String>::new());
        assert!(allow.is_empty());
        assert!(!allow.is_allowed("github.com"));
    }
}
