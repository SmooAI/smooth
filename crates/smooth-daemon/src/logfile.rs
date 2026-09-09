//! `SMOOTH_LOG_FILE` — where the daemon writes its tracing output when it has
//! no terminal to speak of.
//!
//! Launched by the desktop app (Finder/`open`), by launchd, or by `nohup`, the
//! daemon's stderr goes nowhere useful. When Big Smooth.app's bundled daemon died
//! on 2026-09-09 there was no log of the exit anywhere (pearl th-4b189c). With
//! `SMOOTH_LOG_FILE=<path>` set, tracing goes to that file (append, no ANSI)
//! instead of stderr, and the file is size-rotated at startup so it can't grow
//! without bound. Unset/blank keeps the stderr default.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// Rotate the log once it passes this many bytes (checked at startup only —
/// the daemon is long-lived, and a rotate-on-write would need a mutex on the
/// hot path for a file that grows a few MB a week).
pub const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// How many rotated generations to keep (`<file>.1` … `<file>.N`).
pub const DEFAULT_KEEP: usize = 3;

/// Resolve the raw `SMOOTH_LOG_FILE` value to a path. Blank ⇒ `None` (stderr);
/// a leading `~/` expands to the home directory so a launchd plist can say
/// `~/Library/Logs/...` without knowing the user.
pub fn resolve_log_file(raw: Option<&str>) -> Option<PathBuf> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs_next::home_dir() {
            return Some(home.join(rest));
        }
    }
    Some(PathBuf::from(raw))
}

/// Size-rotate `path` once it reaches `max_bytes`.
///
/// `path.N-1` → `path.N`, …, `path` → `path.1`, dropping anything past `keep`.
/// A missing file is not an error. `keep == 0` just truncates by removing the file.
///
/// # Errors
/// Any filesystem error other than the log not existing yet.
pub fn rotate_if_large(path: &Path, max_bytes: u64, keep: usize) -> io::Result<()> {
    let size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if size < max_bytes {
        return Ok(());
    }
    if keep == 0 {
        return std::fs::remove_file(path);
    }
    let gen = |n: usize| -> PathBuf {
        let mut s = path.as_os_str().to_owned();
        s.push(format!(".{n}"));
        PathBuf::from(s)
    };
    // Oldest generation falls off the end; everything else shifts up by one.
    let _ = std::fs::remove_file(gen(keep));
    for n in (1..keep).rev() {
        let from = gen(n);
        if from.exists() {
            std::fs::rename(&from, gen(n + 1))?;
        }
    }
    std::fs::rename(path, gen(1))
}

/// Open `path` for appending, creating parent directories as needed.
///
/// # Errors
/// When the directory can't be created or the file can't be opened for append.
pub fn open_append(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    OpenOptions::new().create(true).append(true).open(path)
}

/// Rotate (if large) then open the resolved log file — the one call `init_tracing` makes.
///
/// # Errors
/// See [`rotate_if_large`] and [`open_append`].
pub fn prepare(path: &Path) -> io::Result<File> {
    rotate_if_large(path, DEFAULT_MAX_BYTES, DEFAULT_KEEP)?;
    open_append(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn blank_or_missing_env_means_stderr() {
        assert_eq!(resolve_log_file(None), None);
        assert_eq!(resolve_log_file(Some("")), None);
        assert_eq!(resolve_log_file(Some("   ")), None);
    }

    #[test]
    fn a_path_is_trimmed_and_kept() {
        assert_eq!(
            resolve_log_file(Some("  /var/log/smooth-daemon.log \n")),
            Some(PathBuf::from("/var/log/smooth-daemon.log"))
        );
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = dirs_next::home_dir().unwrap();
        assert_eq!(resolve_log_file(Some("~/Library/Logs/x.log")), Some(home.join("Library/Logs/x.log")));
        // A bare `~x` (not `~/`) is not a home reference; leave it alone.
        assert_eq!(resolve_log_file(Some("~weird/x.log")), Some(PathBuf::from("~weird/x.log")));
    }

    #[test]
    fn open_append_creates_parents_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/daemon.log");
        open_append(&path).unwrap().write_all(b"one\n").unwrap();
        open_append(&path).unwrap().write_all(b"two\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
    }

    #[test]
    fn rotate_is_a_noop_for_missing_or_small_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        rotate_if_large(&path, 10, 3).unwrap();
        assert!(!path.exists());
        std::fs::write(&path, b"tiny").unwrap();
        rotate_if_large(&path, 10, 3).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "tiny");
        assert!(!dir.path().join("daemon.log.1").exists());
    }

    #[test]
    fn rotate_shifts_generations_and_drops_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let gen = |n: usize| dir.path().join(format!("daemon.log.{n}"));
        for round in 1..=4u8 {
            std::fs::write(&path, [b'a' + round - 1; 16]).unwrap();
            rotate_if_large(&path, 8, 2).unwrap();
            assert!(!path.exists(), "round {round}: the live file was rotated away");
        }
        // Four rotations with keep=2: the two newest survive, older ones are gone.
        assert_eq!(std::fs::read(gen(1)).unwrap(), [b'd'; 16]);
        assert_eq!(std::fs::read(gen(2)).unwrap(), [b'c'; 16]);
        assert!(!gen(3).exists());
    }

    #[test]
    fn rotate_with_keep_zero_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        std::fs::write(&path, [0u8; 32]).unwrap();
        rotate_if_large(&path, 8, 0).unwrap();
        assert!(!path.exists());
        assert!(!dir.path().join("daemon.log.1").exists());
    }

    #[test]
    fn prepare_rotates_then_opens_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        // Under the cap: prepare appends to the existing file.
        std::fs::write(&path, b"old\n").unwrap();
        prepare(&path).unwrap().write_all(b"new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\nnew\n");
    }
}
