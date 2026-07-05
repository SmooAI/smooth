//! SEP extension trust store — the content-hashed allow-list that decides which
//! discovered extensions a host may load.
//!
//! Lives in `smooth-policy` (a leaf crate) so **both** frontends that host
//! extensions can share one trust surface: `smooth-code` (the TUI, which writes
//! it via `th ext trust`) and `smooth-operative` (the dispatched worker, which
//! reads it to load only pre-trusted extensions in an unattended run). Keeping it
//! here avoids the operative depending on the TUI crate.
//!
//! Trust is keyed by extension name and pinned to the content hash of the
//! extension's `extension.toml` at the moment trust was granted; any change to
//! the manifest (its identity, capabilities, or run command — the whole
//! security-relevant surface) invalidates the record, so an edited extension
//! must be re-trusted. Fail-safe: an unknown or changed extension is untrusted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A single trust record, keyed by extension name. `hash` is the content hash at
/// the time trust was granted; a mismatch means the extension changed and must
/// be re-trusted (fail-safe).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustRecord {
    pub source: String,
    pub hash: String,
    pub trusted: bool,
}

/// The trust store: `name → TrustRecord`, persisted as TOML.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default)]
    pub extensions: HashMap<String, TrustRecord>,
}

/// The global extensions directory.
///
/// `$SMOOTH_HOME/extensions` if set, else `~/.smooth/extensions`. Mirrors the
/// engine's `default_global_dir` so the trust file sits alongside the global
/// extensions it governs, without this leaf crate depending on the engine.
///
// ponytail: 3-line mirror of the engine's `default_global_dir`; the `~/.smooth`
// convention is stable. Unify if a third copy ever appears.
#[must_use]
pub fn default_extensions_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("SMOOTH_HOME") {
        return Some(PathBuf::from(home).join("extensions"));
    }
    dirs_next::home_dir().map(|h| h.join(".smooth").join("extensions"))
}

/// Path to the trust file: `<global extensions dir>/trust.toml`.
#[must_use]
pub fn trust_path() -> Option<PathBuf> {
    default_extensions_dir().map(|d| d.join("trust.toml"))
}

impl TrustStore {
    /// Load the trust store (empty if the file is missing/unreadable).
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = trust_path() else { return Self::default() };
        Self::load_from(&path)
    }

    /// Load the trust store from an explicit path (empty if missing/unreadable).
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    /// True if `name` is recorded trusted AND its recorded hash still matches.
    #[must_use]
    pub fn is_trusted(&self, name: &str, hash: &str) -> bool {
        self.extensions.get(name).is_some_and(|r| r.trusted && r.hash == hash)
    }

    /// Record (or overwrite) a trust decision.
    pub fn set(&mut self, name: &str, source: &str, hash: &str, trusted: bool) {
        self.extensions.insert(
            name.to_string(),
            TrustRecord {
                source: source.to_string(),
                hash: hash.to_string(),
                trusted,
            },
        );
    }

    /// Remove a trust record. Returns whether one was present.
    pub fn remove(&mut self, name: &str) -> bool {
        self.extensions.remove(name).is_some()
    }

    /// Persist to `trust.toml`, creating the parent dir.
    ///
    /// # Errors
    /// Returns an error if the directory can't be created or the file written.
    pub fn save(&self) -> Result<()> {
        let path = trust_path().context("no home dir for the trust store")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

/// Content hash of an extension directory: sha256 over its `extension.toml`.
///
/// The manifest fully declares the extension's identity + capabilities + run
/// command — the security-relevant surface a trust decision is made against.
///
/// # Errors
/// Returns an error if `<dir>/extension.toml` can't be read.
pub fn hash_extension(dir: &Path) -> Result<String> {
    let manifest = dir.join("extension.toml");
    let bytes = std::fs::read(&manifest).with_context(|| format!("read {}", manifest.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_store_hash_gating() {
        let mut store = TrustStore::default();
        assert!(!store.is_trusted("todo", "abc"));
        store.set("todo", "/path", "abc", true);
        assert!(store.is_trusted("todo", "abc"));
        // A changed hash (extension edited) is no longer trusted.
        assert!(!store.is_trusted("todo", "def"));
        // Explicit distrust.
        store.set("todo", "/path", "abc", false);
        assert!(!store.is_trusted("todo", "abc"));
        // Removal.
        assert!(store.remove("todo"));
        assert!(!store.remove("todo"));
    }

    #[test]
    fn load_from_missing_is_empty() {
        let store = TrustStore::load_from(Path::new("/no/such/trust.toml"));
        assert!(store.extensions.is_empty());
    }

    #[test]
    fn hash_extension_hashes_manifest_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("extension.toml"), "name = \"x\"\n").unwrap();
        let h1 = hash_extension(tmp.path()).unwrap();
        // Deterministic + 64 hex chars (sha256).
        assert_eq!(h1.len(), 64);
        assert_eq!(h1, hash_extension(tmp.path()).unwrap());
        // A changed manifest yields a different hash.
        std::fs::write(tmp.path().join("extension.toml"), "name = \"y\"\n").unwrap();
        assert_ne!(h1, hash_extension(tmp.path()).unwrap());
    }

    #[test]
    fn default_extensions_dir_honors_smooth_home() {
        std::env::set_var("SMOOTH_HOME", "/tmp/smooth-home-test");
        assert_eq!(default_extensions_dir(), Some(PathBuf::from("/tmp/smooth-home-test/extensions")));
        assert_eq!(trust_path(), Some(PathBuf::from("/tmp/smooth-home-test/extensions/trust.toml")));
        std::env::remove_var("SMOOTH_HOME");
    }
}
