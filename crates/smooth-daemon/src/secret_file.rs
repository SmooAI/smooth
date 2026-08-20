//! Writing a secret to disk without a window where it isn't one.
//!
//! `fs::write` creates 0644 under the usual umask; a follow-up
//! `set_permissions` closes that only *after* the bytes are already
//! readable, and another local user who opens the file in between keeps
//! their fd across the chmod. Several call sites also dropped the chmod
//! result (`let _ =`), so a failure left the file 0644 permanently and
//! silently.
//!
//! [`write_secret`] has no such window: a `NamedTempFile` is created
//! `O_EXCL` at mode 0600, filled, and renamed over the target, so the
//! secret is owner-only from the instant it exists and the replacement
//! is atomic.
//!
//! ponytail: deliberately duplicated from `smooth-api-client`'s
//! `credentials.rs`. The two crates share no dependency, and a crate edge
//! (or a new crate) to host fifteen lines of stdlib glue costs more than
//! the copy. If a third crate needs it, that is the signal to extract.

use std::io::Write as _;
use std::path::Path;

/// Atomically write `contents` to `path`, owner-only from creation.
/// Creates the parent directory (0700 on unix) if it is missing.
///
/// # Errors
/// Any IO failure creating the directory, the temp file, or renaming it
/// into place — deliberately returned rather than swallowed, because a
/// silently unwritten secret is a secret the next run can't find.
pub fn write_secret(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let parent = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => {
            create_private_dir(dir)?;
            dir
        }
        _ => Path::new("."),
    };
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(contents.as_ref())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// `create_dir_all` that asks for mode 0700 up front on unix. Existing
/// directories keep their mode — the files inside are 0600 regardless.
///
/// # Errors
/// The directory can't be created.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn write_secret_is_owner_only_and_creates_a_0700_parent() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("token");
        write_secret(&path, "s3cret").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "s3cret");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(dir.path().join("nested")).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn rewriting_an_existing_0644_file_tightens_it() {
        // The rename replaces the inode, so a file left 0644 by an older
        // build doesn't stay 0644 forever.
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_secret(&path, "new").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn write_secret_leaves_no_temp_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        write_secret(&dir.path().join("token"), "x").unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().filter_map(Result::ok).map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 1, "expected only the target file, got {names:?}");
    }
}
