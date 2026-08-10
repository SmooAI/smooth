//! `th doctor`'s reclaimable-disk section (pearl th-91de11).
//!
//! `~/.smooth` accumulates several things nothing ever cleans up: per-branch
//! cargo target dirs from `pnpm install:th`, the pre-SMOODEV-1739 legacy auth
//! tree, an unrotated `service.log`, and `providers.json` backups. This module
//! only *reports* them — doctor auto-fixes config, never user data, so every
//! finding ships with the `rm` the user can paste.

use std::path::{Path, PathBuf};

/// `service.log` is only worth mentioning once it's actually large; there is
/// no rotation, so it grows forever.
const LOG_NAG_BYTES: u64 = 5 * 1024 * 1024;

/// One reclaimable thing on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Human label for the finding, e.g. `build-th-fd06bf-target`.
    pub what: String,
    /// Bytes it occupies.
    pub bytes: u64,
    /// The exact command that reclaims it.
    pub hint: String,
}

/// Format bytes the way `du -h` would, near enough for a nag line.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// Recursive on-disk size. Symlinks are not followed (walkdir's default), so
/// a link into the workspace can't inflate the number or loop forever.
fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.len())
        .sum()
}

/// Everything in `smooth_home` (normally `~/.smooth`) that is safe to delete.
///
/// `xdg_auth` is the live auth tree (`~/.config/smooth/auth`); the legacy
/// `<smooth_home>/auth` is only reported as stale when the live one exists,
/// because on a host that never migrated it is still the real thing.
pub fn findings(smooth_home: &Path, xdg_auth: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    if !smooth_home.is_dir() {
        return out;
    }

    let mut build_targets: Vec<PathBuf> = Vec::new();
    let mut backups: Vec<PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(smooth_home) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("build-") && name.ends_with("-target") && entry.path().is_dir() {
            build_targets.push(entry.path());
        } else if name.starts_with("providers.json.bak") {
            backups.push(entry.path());
        }
    }

    build_targets.sort();
    for target in &build_targets {
        let name = target.file_name().unwrap_or_default().to_string_lossy().to_string();
        out.push(Finding {
            bytes: dir_size(target),
            hint: format!("rm -rf {}", target.display()),
            what: name,
        });
    }

    let legacy_auth = smooth_home.join("auth");
    if legacy_auth.is_dir() && xdg_auth.is_dir() {
        out.push(Finding {
            what: "legacy auth tree (live sessions moved to ~/.config/smooth/auth)".to_string(),
            bytes: dir_size(&legacy_auth),
            hint: format!("rm -rf {}", legacy_auth.display()),
        });
    }

    let service_log = smooth_home.join("service.log");
    if let Ok(meta) = std::fs::metadata(&service_log) {
        if meta.len() > LOG_NAG_BYTES {
            out.push(Finding {
                what: "service.log (never rotated)".to_string(),
                bytes: meta.len(),
                hint: format!(": > {}", service_log.display()),
            });
        }
    }

    if !backups.is_empty() {
        let bytes = backups.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        out.push(Finding {
            what: format!("{} providers.json backup(s)", backups.len()),
            bytes,
            hint: format!("rm {}/providers.json.bak*", smooth_home.display()),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn human_bytes_scales_and_keeps_raw_bytes_exact() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_bytes(24_600_000_000), "22.9 GB");
    }

    #[test]
    fn missing_home_yields_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(findings(&tmp.path().join("nope"), &tmp.path().join("xdg")).is_empty());
    }

    #[test]
    fn clean_home_yields_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("smooth");
        write(&home.join("providers.json"), 10);
        write(&home.join("service.log"), 32);
        assert_eq!(findings(&home, &tmp.path().join("xdg")), Vec::new());
    }

    #[test]
    fn reports_build_targets_with_recursive_sizes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("smooth");
        write(&home.join("build-abc-target/debug/deps/x.rlib"), 2048);
        write(&home.join("build-abc-target/debug/y.o"), 1024);
        // Neither prefix nor suffix alone counts.
        write(&home.join("build-notatarget/f"), 900);
        write(&home.join("other-target/f"), 900);

        let f = findings(&home, &tmp.path().join("xdg"));
        assert_eq!(f.len(), 1, "only build-*-target dirs count: {f:?}");
        assert_eq!(f[0].what, "build-abc-target");
        assert_eq!(f[0].bytes, 3072, "size must be recursive");
        assert!(f[0].hint.starts_with("rm -rf "), "{}", f[0].hint);
    }

    #[test]
    fn legacy_auth_only_flagged_once_the_xdg_tree_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("smooth");
        let xdg = tmp.path().join("xdg-auth");
        write(&home.join("auth/smooai-user.json"), 64);

        // Not yet migrated: the legacy tree is still the live one.
        assert_eq!(findings(&home, &xdg), Vec::new());

        write(&xdg.join("smooai-user.json"), 64);
        let f = findings(&home, &xdg);
        assert_eq!(f.len(), 1);
        assert!(f[0].what.starts_with("legacy auth tree"));
        assert_eq!(f[0].bytes, 64);
    }

    #[test]
    fn service_log_only_nags_above_the_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("smooth");
        write(&home.join("service.log"), LOG_NAG_BYTES as usize);
        assert_eq!(findings(&home, &tmp.path().join("xdg")), Vec::new(), "at the threshold is not over it");

        write(&home.join("service.log"), LOG_NAG_BYTES as usize + 1);
        let f = findings(&home, &tmp.path().join("xdg"));
        assert_eq!(f.len(), 1);
        assert!(f[0].hint.starts_with(": > "), "truncate, don't rm — the service holds the fd: {}", f[0].hint);
    }

    #[test]
    fn backups_are_aggregated_into_one_finding() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("smooth");
        write(&home.join("providers.json"), 500); // the live file is never a finding
        write(&home.join("providers.json.bak"), 100);
        write(&home.join("providers.json.bak2"), 200);
        write(&home.join("providers.json.bak-20260417-103425"), 300);

        let f = findings(&home, &tmp.path().join("xdg"));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].what, "3 providers.json backup(s)");
        assert_eq!(f[0].bytes, 600);
    }
}
