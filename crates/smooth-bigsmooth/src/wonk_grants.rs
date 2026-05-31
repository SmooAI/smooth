//! Persistent permission grants — `wonk-allow.toml`.
//!
//! When the user picks `[u]ser` (or `[p]roject`) on an inline approval
//! card, the resolution is persisted here so subsequent sessions don't
//! re-ask. Two TOML files, stacked at lookup time:
//!
//! - `~/.smooth/wonk-allow.toml` — the user's personal grants
//! - `<repo>/.smooth/wonk-allow.toml` — project-scoped grants checked
//!   into git so teammates pulling the project inherit the approvals
//!
//! ## Schema (v1)
//!
//! ```toml
//! schema_version = 1
//!
//! [network]
//! allow_hosts = ["api.openai.com", "*.openai.com"]
//!
//! [tools]
//! allow = ["aws-readonly", "web_search"]
//!
//! [bash]
//! allow_patterns = ["cargo *", "pnpm *"]
//! ```
//!
//! Globs use the standard shell wildcards: `*` matches anything inside
//! a single host label, and a leading `.suffix` style match is
//! supported via [`host_matches_glob`]'s suffix logic.
//!
//! ## Conflict resolution
//!
//! Allow-lists from multiple sources are unioned. There is no deny
//! list in the v1 schema — denies live in `smooth-narc`'s baked-in
//! `DANGEROUS_*` lists and per-request denials don't persist (a
//! human-denied request becomes Open again next time the agent asks,
//! since intent may have changed).
//!
//! ## Atomic writes
//!
//! Saves use the write-to-tempfile-then-rename pattern. A crash mid-
//! save leaves the previous file intact rather than a half-written
//! TOML.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// In-memory snapshot of `wonk-allow.toml`. Cheap to clone (Arc'd
/// internally by the [`SharedWonkGrants`] wrapper that callers use).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WonkGrants {
    /// Currently always 1. Used for forward-compatible migrations.
    pub schema_version: u32,
    #[serde(skip_serializing_if = "NetworkSection::is_empty", default)]
    pub network: NetworkSection,
    #[serde(skip_serializing_if = "ToolsSection::is_empty", default)]
    pub tools: ToolsSection,
    #[serde(skip_serializing_if = "BashSection::is_empty", default)]
    pub bash: BashSection,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NetworkSection {
    /// Hosts (or host globs) that should be approved without asking.
    /// `*.example.com` matches every subdomain; `example.com` matches
    /// only that exact name.
    pub allow_hosts: BTreeSet<String>,
}

impl NetworkSection {
    fn is_empty(&self) -> bool {
        self.allow_hosts.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolsSection {
    /// Tool names that should be approved without asking. Exact match
    /// only — tools have a flat namespace so globs would be confusing.
    pub allow: BTreeSet<String>,
}

impl ToolsSection {
    fn is_empty(&self) -> bool {
        self.allow.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BashSection {
    /// Substrings that, if found at the start of a command, mark it
    /// as approved. `"cargo "` matches `cargo test`, `cargo build`,
    /// etc. Trailing space is significant — it prevents `"cargo "`
    /// from matching `cargonaut`.
    pub allow_patterns: BTreeSet<String>,
}

impl BashSection {
    fn is_empty(&self) -> bool {
        self.allow_patterns.is_empty()
    }
}

impl WonkGrants {
    /// Create grants pinned at the current schema version.
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            ..Self::default()
        }
    }

    /// Match `host` against the `[network]` allow-hosts list. Exact
    /// matches and `*.<suffix>` globs both count.
    #[must_use]
    pub fn matches_host(&self, host: &str) -> bool {
        let lower = host.to_ascii_lowercase();
        self.network.allow_hosts.iter().any(|pat| host_matches_glob(&lower, pat))
    }

    /// True if `tool_name` is in the `[tools]` allow list.
    #[must_use]
    pub fn matches_tool(&self, tool_name: &str) -> bool {
        self.tools.allow.contains(tool_name)
    }

    /// True if `command` starts with any pattern in `[bash]` allow_patterns.
    #[must_use]
    pub fn matches_bash(&self, command: &str) -> bool {
        let lower = command.to_ascii_lowercase();
        self.bash.allow_patterns.iter().any(|p| lower.starts_with(&p.to_ascii_lowercase()))
    }

    /// Add a network host (or glob) to the allow-list. Idempotent.
    pub fn add_host(&mut self, host: impl Into<String>) {
        self.network.allow_hosts.insert(host.into());
    }

    /// Add a tool name to the allow-list. Idempotent.
    pub fn add_tool(&mut self, tool: impl Into<String>) {
        self.tools.allow.insert(tool.into());
    }

    /// Add a bash prefix pattern. Idempotent.
    pub fn add_bash_pattern(&mut self, pattern: impl Into<String>) {
        self.bash.allow_patterns.insert(pattern.into());
    }

    /// Merge `other` into `self` — every allow-list entry is unioned.
    /// The merged result has `schema_version = max(self, other)` so a
    /// future v2 reader knows it touched the more recent shape.
    pub fn merge_with(&mut self, other: Self) {
        self.schema_version = self.schema_version.max(other.schema_version);
        self.network.allow_hosts.extend(other.network.allow_hosts);
        self.tools.allow.extend(other.tools.allow);
        self.bash.allow_patterns.extend(other.bash.allow_patterns);
    }

    /// Parse from a TOML string. Missing sections default to empty.
    ///
    /// # Errors
    ///
    /// Returns the TOML parse error if the input isn't valid v1.
    pub fn parse(toml_text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(toml_text)?)
    }

    /// Serialize to TOML, with the schema_version always at the top
    /// for clarity.
    ///
    /// # Errors
    ///
    /// Propagates `toml::ser::Error` if serialization fails — unlikely
    /// for our shape, but the toml crate's API surfaces it.
    pub fn to_toml_string(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Load from `path`. Missing file yields an empty `WonkGrants`
    /// (not an error) — first-time use shouldn't require manual
    /// touch.
    ///
    /// # Errors
    ///
    /// I/O errors other than `NotFound` and TOML parse errors are
    /// returned as `anyhow::Error`.
    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically write to `path` by serializing to a sibling
    /// tempfile and renaming. Creates the parent directory if it
    /// doesn't exist.
    ///
    /// # Errors
    ///
    /// I/O errors and TOML serialization errors are returned.
    pub fn save_to_path(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = self.to_toml_string()?;
        let tmp_path = path.with_extension("toml.tmp");
        std::fs::write(&tmp_path, text)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

/// Resolve the user-scope grants file: `~/.smooth/wonk-allow.toml`.
/// Returns `None` when there's no home dir (rare — only on broken
/// containers / minimal CI environments).
#[must_use]
pub fn user_grants_path() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".smooth").join("wonk-allow.toml"))
}

/// Resolve the project-scope grants file:
/// `<workspace>/.smooth/wonk-allow.toml`.
#[must_use]
pub fn project_grants_path(workspace: &Path) -> PathBuf {
    workspace.join(".smooth").join("wonk-allow.toml")
}

/// Append a single grant to a TOML file. The file is loaded, mutated,
/// and saved atomically. Creates the file if it doesn't exist.
///
/// `kind` is the request kind string (`"network"` / `"tool"` /
/// `"cli"`); `entry` is the resource the grant approves (a host, a
/// tool name, or a bash prefix pattern). For network grants, an
/// optional `glob_override` lets the caller bind the approval to a
/// glob like `*.example.com` instead of the exact host.
///
/// # Errors
///
/// I/O errors, TOML parse errors, or unknown `kind` strings.
pub fn append_grant(path: &Path, kind: &str, entry: &str, glob_override: Option<&str>) -> anyhow::Result<()> {
    let mut grants = WonkGrants::load_from_path(path)?;
    if grants.schema_version == 0 {
        grants.schema_version = 1;
    }
    match kind {
        "network" => grants.add_host(glob_override.unwrap_or(entry).to_string()),
        "tool" => grants.add_tool(entry),
        "cli" => grants.add_bash_pattern(entry),
        // file / mcp / port don't yet have a [section] in v1. We could
        // add them later; for now, refuse rather than silently lose
        // the grant.
        other => anyhow::bail!("unsupported grant kind '{other}' for wonk-allow.toml (v1)"),
    }
    grants.save_to_path(path)?;
    Ok(())
}

/// Thread-safe wrapper for shared access to the merged grants. The
/// orchestrator hands one of these to SafehouseNarc; the access
/// handlers also hold a reference so writes-back invalidate the
/// in-memory copy.
#[derive(Debug, Clone, Default)]
pub struct SharedWonkGrants {
    inner: Arc<RwLock<WonkGrants>>,
}

impl SharedWonkGrants {
    #[must_use]
    pub fn new(grants: WonkGrants) -> Self {
        Self {
            inner: Arc::new(RwLock::new(grants)),
        }
    }

    /// Read a snapshot. Cloned out of the lock so callers can use it
    /// without holding the reader open.
    #[must_use]
    pub fn snapshot(&self) -> WonkGrants {
        self.inner.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// Merge `other` into the live grants — typically called after a
    /// successful `append_grant` so the runtime view stays in sync
    /// with the file.
    pub fn merge_in(&self, other: WonkGrants) {
        if let Ok(mut g) = self.inner.write() {
            g.merge_with(other);
        }
    }

    /// Replace the live grants wholesale (used by hot-reload + tests).
    pub fn replace(&self, grants: WonkGrants) {
        if let Ok(mut g) = self.inner.write() {
            *g = grants;
        }
    }
}

/// Glob match for a single host pattern. The pattern can be:
///
/// - An exact host: `api.example.com` matches only that.
/// - A wildcard suffix: `*.example.com` matches any subdomain.
/// - An open suffix: `example.com` (no leading dot) also matches
///   bare `example.com` AND any subdomain — this is the same shape
///   the [`smooth_narc::judge::domain_matches_suffix_list`] table uses.
///
/// Case-insensitive on both sides.
#[must_use]
pub fn host_matches_glob(host: &str, pattern: &str) -> bool {
    let h = host.to_ascii_lowercase();
    let p = pattern.to_ascii_lowercase();
    if h == p {
        return true;
    }
    if let Some(suffix) = p.strip_prefix("*.") {
        return h.ends_with(&format!(".{suffix}")) || h == suffix;
    }
    if let Some(suffix) = p.strip_prefix('.') {
        return h.ends_with(&format!(".{suffix}")) || h == suffix;
    }
    // Bare suffix (no leading dot / star) — only exact match. Avoid
    // a substring match here to prevent `evil-example.com` matching
    // `example.com`.
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn grants_with_host(host: &str) -> WonkGrants {
        let mut g = WonkGrants::new();
        g.add_host(host);
        g
    }

    #[test]
    fn schema_version_default_is_zero_but_new_sets_to_one() {
        let default = WonkGrants::default();
        assert_eq!(default.schema_version, 0);
        let new = WonkGrants::new();
        assert_eq!(new.schema_version, 1);
    }

    #[test]
    fn host_matches_exact() {
        let g = grants_with_host("api.example.com");
        assert!(g.matches_host("api.example.com"));
        assert!(g.matches_host("API.EXAMPLE.COM"));
        assert!(!g.matches_host("other.example.com"));
    }

    #[test]
    fn host_matches_wildcard_subdomain() {
        let g = grants_with_host("*.example.com");
        assert!(g.matches_host("api.example.com"));
        assert!(g.matches_host("foo.bar.example.com"));
        assert!(g.matches_host("example.com")); // bare apex counts
        assert!(!g.matches_host("evil-example.com"));
        assert!(!g.matches_host("notexample.com"));
    }

    #[test]
    fn host_matches_dot_prefix_suffix_form() {
        let g = grants_with_host(".example.com");
        assert!(g.matches_host("api.example.com"));
        assert!(g.matches_host("example.com"));
        assert!(!g.matches_host("evil-example.com"));
    }

    #[test]
    fn bare_host_pattern_requires_exact_match() {
        // No glob prefix, no leading dot — only the exact host should
        // match. This protects against `evil-example.com` slipping
        // past a `example.com` allow entry.
        let g = grants_with_host("example.com");
        assert!(g.matches_host("example.com"));
        assert!(!g.matches_host("api.example.com"));
        assert!(!g.matches_host("evil-example.com"));
    }

    #[test]
    fn tool_match_is_exact_only() {
        let mut g = WonkGrants::new();
        g.add_tool("aws-readonly");
        assert!(g.matches_tool("aws-readonly"));
        assert!(!g.matches_tool("aws-readonly-write"));
        assert!(!g.matches_tool("aws"));
    }

    #[test]
    fn bash_pattern_matches_command_prefix() {
        let mut g = WonkGrants::new();
        g.add_bash_pattern("cargo ");
        assert!(g.matches_bash("cargo test"));
        assert!(g.matches_bash("cargo build --release"));
        assert!(g.matches_bash("CARGO TEST")); // case-insensitive
        assert!(!g.matches_bash("cargonaut"));
        // Without the trailing space, only `cargo test` would prefix-match
        // but `cargonaut` would too — the trailing space is the guard.
    }

    #[test]
    fn merge_unions_allow_lists() {
        let mut a = WonkGrants::new();
        a.add_host("a.example.com");
        a.add_tool("tool_a");
        let mut b = WonkGrants::new();
        b.add_host("b.example.com");
        b.add_tool("tool_b");
        b.add_bash_pattern("pnpm ");

        a.merge_with(b);
        assert!(a.matches_host("a.example.com"));
        assert!(a.matches_host("b.example.com"));
        assert!(a.matches_tool("tool_a"));
        assert!(a.matches_tool("tool_b"));
        assert!(a.matches_bash("pnpm install"));
    }

    #[test]
    fn merge_takes_max_schema_version() {
        let mut a = WonkGrants::default(); // version 0
        let mut b = WonkGrants::new(); // version 1
        b.add_host("b.example.com");
        a.merge_with(b);
        assert_eq!(a.schema_version, 1);
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("wonk-allow.toml");
        let mut g = WonkGrants::new();
        g.add_host("api.openai.com");
        g.add_host("*.openai.com");
        g.add_tool("web_search");
        g.add_bash_pattern("cargo ");
        g.save_to_path(&path).expect("save");

        let loaded = WonkGrants::load_from_path(&path).expect("load");
        assert_eq!(loaded, g);
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.toml");
        let loaded = WonkGrants::load_from_path(&path).expect("load missing");
        assert_eq!(loaded.network.allow_hosts.len(), 0);
        assert_eq!(loaded.tools.allow.len(), 0);
        // load_from_path returns Self::new() (schema_version=1) for missing.
        assert_eq!(loaded.schema_version, 1);
    }

    #[test]
    fn save_uses_atomic_rename() {
        // Concrete test: save into a path whose parent doesn't exist.
        // The save should create the parent dir AND atomically place
        // the file — verifying the tempfile path doesn't leak.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested").join("dir").join("wonk-allow.toml");
        let mut g = WonkGrants::new();
        g.add_host("a.example.com");
        g.save_to_path(&path).expect("save");
        assert!(path.exists(), "target file should exist");
        // The .toml.tmp sibling must not have leaked.
        let leak = path.with_extension("toml.tmp");
        assert!(!leak.exists(), "tempfile should have been renamed away");
    }

    #[test]
    fn parse_empty_input_returns_default() {
        // schema_version is the only required field, but with serde
        // default it can be omitted (defaults to 0). An empty file is
        // valid (yields fully-empty grants).
        let g = WonkGrants::parse("").expect("empty parses");
        assert_eq!(g.schema_version, 0);
        assert!(g.network.allow_hosts.is_empty());
    }

    #[test]
    fn parse_legacy_schema_version_round_trips() {
        let toml_text = r#"
            schema_version = 1
            [network]
            allow_hosts = ["api.openai.com", "*.openai.com"]
            [tools]
            allow = ["web_search"]
            [bash]
            allow_patterns = ["cargo "]
        "#;
        let g = WonkGrants::parse(toml_text).expect("parse");
        assert_eq!(g.schema_version, 1);
        assert!(g.matches_host("api.openai.com"));
        assert!(g.matches_host("foo.openai.com"));
        assert!(g.matches_tool("web_search"));
        assert!(g.matches_bash("cargo build"));
    }

    #[test]
    fn append_grant_creates_then_extends() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("wonk-allow.toml");
        // First append creates the file.
        append_grant(&path, "network", "api.example.com", None).expect("append 1");
        // Second append loads + extends.
        append_grant(&path, "network", "other.example.com", Some("*.example.com")).expect("append 2");
        append_grant(&path, "tool", "web_search", None).expect("append tool");
        append_grant(&path, "cli", "cargo ", None).expect("append cli");

        let g = WonkGrants::load_from_path(&path).expect("load");
        assert!(g.matches_host("api.example.com"));
        // The glob_override should have been stored, not the exact resource.
        assert!(g.matches_host("foo.example.com"));
        assert!(g.matches_tool("web_search"));
        assert!(g.matches_bash("cargo test"));
    }

    #[test]
    fn append_grant_rejects_unsupported_kind() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("wonk-allow.toml");
        let err = append_grant(&path, "file", "/etc/passwd", None).expect_err("should reject");
        assert!(err.to_string().contains("unsupported grant kind"));
    }

    #[test]
    fn shared_grants_snapshot_is_lock_free() {
        let shared = SharedWonkGrants::new(grants_with_host("a.example.com"));
        let snap = shared.snapshot();
        assert!(snap.matches_host("a.example.com"));
        // Mutating the snapshot does NOT affect the shared store.
        let mut snap_mut = snap;
        snap_mut.add_host("b.example.com");
        let snap2 = shared.snapshot();
        assert!(!snap2.matches_host("b.example.com"));
    }

    #[test]
    fn shared_grants_merge_in_is_visible_to_subsequent_snapshot() {
        let shared = SharedWonkGrants::new(grants_with_host("a.example.com"));
        let mut more = WonkGrants::new();
        more.add_host("b.example.com");
        more.add_tool("web_search");
        shared.merge_in(more);

        let snap = shared.snapshot();
        assert!(snap.matches_host("a.example.com"));
        assert!(snap.matches_host("b.example.com"));
        assert!(snap.matches_tool("web_search"));
    }

    #[test]
    fn shared_grants_replace_drops_old() {
        let shared = SharedWonkGrants::new(grants_with_host("a.example.com"));
        let mut new_grants = WonkGrants::new();
        new_grants.add_host("b.example.com");
        shared.replace(new_grants);

        let snap = shared.snapshot();
        assert!(!snap.matches_host("a.example.com"));
        assert!(snap.matches_host("b.example.com"));
    }

    #[test]
    fn user_grants_path_under_home_dot_smooth() {
        // Path resolution shape: ends with `.smooth/wonk-allow.toml`.
        // We don't assert on the home prefix because tests can run
        // under HOME=/nonexistent without breaking.
        if let Some(p) = user_grants_path() {
            let s = p.to_string_lossy();
            assert!(s.ends_with(".smooth/wonk-allow.toml") || s.ends_with(".smooth\\wonk-allow.toml"));
        }
    }

    #[test]
    fn project_grants_path_is_workspace_relative() {
        let p = project_grants_path(Path::new("/tmp/example"));
        assert_eq!(p, PathBuf::from("/tmp/example/.smooth/wonk-allow.toml"));
    }
}
