//! Project-cache Volume helpers.
//!
//! The operator VM project cache has two backends (see
//! [`crate::protocol::SandboxSpec::use_named_volume_for_cache`]):
//!
//! 1. Legacy bind-mount under `~/.smooth/project-cache/<cache_key>/`,
//!    managed directly by the CLI via filesystem APIs.
//! 2. Named microsandbox `Volume` under
//!    `~/.microsandbox/volumes/smooth-cache-<cache_key>/`, tagged with
//!    the label `smooth-kind=project-cache`.
//!
//! This module exposes typed, side-effect-free-ish helpers that the CLI
//! (`th cache …`) uses to enumerate, size, and remove entries in the
//! volume backend without reaching into the microsandbox types directly.
//! Keeping the microsandbox crate confined to this crate means `th` can
//! still build without having to re-pull its transitive dep graph.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::server::sanitize_volume_name;

/// Label key that marks a named volume as a smooth project cache.
/// Matches the label applied by [`crate::server::spawn_sandbox`] when it
/// creates a new cache volume.
pub const PROJECT_CACHE_LABEL_KEY: &str = "smooth-kind";
/// Label value matching [`PROJECT_CACHE_LABEL_KEY`].
pub const PROJECT_CACHE_LABEL_VALUE: &str = "project-cache";
/// Label key that stores the original cache_key (pre-sanitization) so
/// `th cache clear <project>` can walk from project path → cache_key →
/// volume name without having to re-derive the exact sanitization.
pub const PROJECT_CACHE_KEY_LABEL: &str = "smooth-cache-key";

/// Per-volume info. Fully owned — no lifetimes back to microsandbox.
#[derive(Debug, Clone)]
pub struct ProjectCacheVolumeInfo {
    /// Volume name in microsandbox (e.g. `smooth-cache-budgeting-abc123`).
    pub volume_name: String,
    /// Original cache key (workspace-path hash), recovered from the
    /// `smooth-cache-key` label if present. Falls back to the volume
    /// name with the `smooth-cache-` prefix stripped when the label is
    /// missing (older volumes created before labels were added).
    pub cache_key: String,
    /// Host-side directory where the volume's data lives.
    /// `~/.microsandbox/volumes/<volume_name>/`.
    pub path: PathBuf,
    /// Recursive on-disk size in bytes.
    pub size_bytes: u64,
    /// Wall-clock creation time from the microsandbox DB, if recorded.
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Most recent filesystem mtime inside the volume tree — used as a
    /// "last touched" signal for LRU-style pruning. Falls back to
    /// `created_at` equivalent (dir mtime) when there are no files yet.
    pub last_modified: Option<std::time::SystemTime>,
}

/// Convert a workspace cache key into the microsandbox Volume name.
///
/// Mirror of the naming applied by [`crate::server::spawn_sandbox`]
/// for the named-volume backend. Exposed so CLI-side code can compute
/// "what would the volume be named?" without cross-referencing
/// internals.
#[must_use]
pub fn volume_name_for_cache_key(cache_key: &str) -> String {
    sanitize_volume_name(cache_key)
}

/// List every microsandbox volume tagged as a smooth project cache.
///
/// Filters the full `Volume::list()` output by the
/// [`PROJECT_CACHE_LABEL_KEY`] = [`PROJECT_CACHE_LABEL_VALUE`] label.
///
/// # Errors
///
/// Returns the microsandbox DB error if the volumes database cannot be
/// opened (typically because `~/.microsandbox/` doesn't exist yet).
pub async fn list_project_cache_volumes() -> Result<Vec<ProjectCacheVolumeInfo>> {
    let handles = match microsandbox::volume::Volume::list().await {
        Ok(h) => h,
        Err(e) => {
            let msg = format!("{e}");
            // If the DB is missing (fresh install, never spawned a
            // sandbox), return an empty list rather than a hard error.
            if msg.contains("no such file") || msg.to_ascii_lowercase().contains("no such") {
                return Ok(Vec::new());
            }
            return Err(anyhow::anyhow!(e).context("microsandbox::Volume::list"));
        }
    };

    let volumes_dir = microsandbox::config::config().volumes_dir();

    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        let labels = h.labels();
        let is_project_cache = labels.iter().any(|(k, v)| k == PROJECT_CACHE_LABEL_KEY && v == PROJECT_CACHE_LABEL_VALUE);
        if !is_project_cache {
            continue;
        }

        let cache_key = labels
            .iter()
            .find(|(k, _)| k == PROJECT_CACHE_KEY_LABEL)
            .map_or_else(|| h.name().strip_prefix("smooth-cache-").unwrap_or(h.name()).to_string(), |(_, v)| v.clone());

        let path = volumes_dir.join(h.name());
        let size_bytes = dir_size_bytes(&path);
        let last_modified = dir_mtime(&path);

        out.push(ProjectCacheVolumeInfo {
            volume_name: h.name().to_string(),
            cache_key,
            path,
            size_bytes,
            created_at: h.created_at(),
            last_modified,
        });
    }

    Ok(out)
}

/// Remove the project-cache volume for a given `cache_key`, if one
/// exists. Returns `Ok(true)` if a volume was removed, `Ok(false)` if
/// nothing matched.
///
/// # Errors
///
/// Returns the microsandbox error if the volume exists but removal
/// fails (e.g. still mounted by a running sandbox).
pub async fn remove_project_cache_volume(cache_key: &str) -> Result<bool> {
    let volume_name = volume_name_for_cache_key(cache_key);
    match microsandbox::volume::Volume::get(&volume_name).await {
        Ok(handle) => {
            handle.remove().await.with_context(|| format!("remove project-cache volume '{volume_name}'"))?;
            Ok(true)
        }
        Err(e) => {
            let msg = format!("{e}");
            if msg.contains("not found") || msg.to_ascii_lowercase().contains("not found") {
                Ok(false)
            } else {
                // DB open failure on a fresh install — treat as "not there".
                if msg.contains("no such file") || msg.to_ascii_lowercase().contains("no such") {
                    Ok(false)
                } else {
                    Err(anyhow::anyhow!(e).context(format!("look up project-cache volume '{volume_name}'")))
                }
            }
        }
    }
}

/// Recursive on-disk size of a directory tree, counting file bytes only.
/// Returns 0 on IO errors rather than bailing — a partially listable
/// cache is still useful to report.
fn dir_size_bytes(path: &std::path::Path) -> u64 {
    fn walk(p: &std::path::Path) -> u64 {
        let mut total = 0u64;
        let Ok(entries) = std::fs::read_dir(p) else { return 0 };
        for e in entries.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                total = total.saturating_add(walk(&e.path()));
            } else {
                total = total.saturating_add(md.len());
            }
        }
        total
    }
    if !path.is_dir() {
        return 0;
    }
    walk(path)
}

/// Most recent mtime across a directory's direct children, falling back
/// to the directory's own mtime. Best-effort; returns `None` on IO
/// errors. Used as a "last touched" signal for LRU pruning since
/// microsandbox doesn't track per-volume access time.
fn dir_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    let dir_md = std::fs::metadata(path).ok()?;
    let mut newest = dir_md.modified().ok()?;
    let Ok(entries) = std::fs::read_dir(path) else { return Some(newest) };
    for e in entries.flatten() {
        if let Ok(md) = e.metadata() {
            if let Ok(m) = md.modified() {
                if m > newest {
                    newest = m;
                }
            }
        }
    }
    Some(newest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_name_for_cache_key_matches_sanitize() {
        // The CLI needs to compute the same name Bill would. Lock the
        // mapping via a specific example so an accidental divergence
        // trips the test.
        let name = volume_name_for_cache_key("budgeting-abc123");
        assert_eq!(name, "smooth-cache-budgeting-abc123");
    }

    #[test]
    fn volume_name_for_cache_key_handles_unsafe_input() {
        // / and + are replaced, leading non-alnum is stripped, the
        // `smooth-cache-` prefix is added.
        let name = volume_name_for_cache_key("/weird+key");
        assert_eq!(name, "smooth-cache-weird_key");
    }

    #[test]
    fn project_cache_label_constants_stay_stable() {
        // These values are part of the wire contract between Bill
        // (writer) and the CLI (reader). Changing them silently would
        // make `th cache list` stop seeing any volumes after the next
        // Bill spawn writes the new label. Lock them down.
        assert_eq!(PROJECT_CACHE_LABEL_KEY, "smooth-kind");
        assert_eq!(PROJECT_CACHE_LABEL_VALUE, "project-cache");
        assert_eq!(PROJECT_CACHE_KEY_LABEL, "smooth-cache-key");
    }

    #[test]
    fn dir_size_bytes_returns_zero_for_missing_path() {
        let bogus = std::path::PathBuf::from("/tmp/smooth-cache-volumes-test-does-not-exist-xyz");
        assert_eq!(dir_size_bytes(&bogus), 0);
    }

    #[test]
    fn dir_mtime_returns_none_for_missing_path() {
        let bogus = std::path::PathBuf::from("/tmp/smooth-cache-volumes-test-does-not-exist-xyz");
        assert!(dir_mtime(&bogus).is_none());
    }

    #[test]
    fn dir_size_and_mtime_work_on_real_tmp_dir() {
        let dir = tempfile::tempdir().expect("tmp");
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, "hi").expect("write");
        assert!(dir_size_bytes(dir.path()) >= 2);
        assert!(dir_mtime(dir.path()).is_some());
    }
}
