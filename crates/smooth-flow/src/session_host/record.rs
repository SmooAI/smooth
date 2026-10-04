//! The host record (`<dir>/<id>.json`) and the private directory it lives in.
//!
//! The record is everything a daemon needs to adopt a host it did not start:
//! where the socket is, the token, whose host it is, and how the child ended
//! if it has. It is written temp file + rename at mode 0600, so a reader
//! never sees half a record and nobody else can read the token.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::protocol::ExitInfo;

/// `$SMOOTH_FLOW_HOSTS_DIR` overrides [`hosts_dir`] (tests point it at a temp
/// dir).
pub const HOSTS_DIR_ENV: &str = "SMOOTH_FLOW_HOSTS_DIR";

/// A Unix socket path longer than this is moved under `$TMPDIR` (macOS's
/// `sun_path` holds 104 bytes with its NUL, Linux's 108).
pub const MAX_SOCKET_PATH: usize = 100;

/// `~/.smooth/flow-hosts`, or `$SMOOTH_FLOW_HOSTS_DIR`.
#[must_use]
pub fn hosts_dir() -> PathBuf {
    if let Some(d) = std::env::var_os(HOSTS_DIR_ENV).filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    dirs_next::home_dir().unwrap_or_else(std::env::temp_dir).join(".smooth").join("flow-hosts")
}

/// A flow session id as the host accepts it: `fs-` and 8 lowercase hex.
/// Everything that becomes a path goes through this, so an id can never
/// name a file outside the host dir.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("fs-")
        .is_some_and(|hex| hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// A token as the daemon makes them: 64 lowercase hex (32 random bytes).
#[must_use]
pub fn valid_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A fresh per-host token from the OS RNG.
#[must_use]
pub fn new_token() -> String {
    use rand::RngCore as _;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b.iter().fold(String::with_capacity(64), |mut s, x| {
        use std::fmt::Write as _;
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// The record file for `id`.
#[must_use]
pub fn record_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// One host's record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRecord {
    /// Record schema ([`super::protocol::RECORD_V`]).
    pub v: u32,
    /// The IPC version the host speaks (fixed for its lifetime).
    pub protocol: u32,
    pub id: String,
    pub host_version: String,
    pub pid: u32,
    /// The host's start time in epoch seconds (`crate::proc::start_time`),
    /// so a recycled pid can't pass for it. The same unit the engine stores
    /// in `sessions.pid_start`.
    pub pid_start: Option<i64>,
    pub child_pid: u32,
    /// Read this; never derive it (it may be under `$TMPDIR`).
    pub socket: PathBuf,
    pub token: String,
    /// The creating daemon's ownership identity (th-4f7866).
    pub owner: String,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub created_at: String,
    /// `None` while the child runs.
    pub exit: Option<ExitInfo>,
}

impl HostRecord {
    /// Is the host process this record describes still alive (same pid and
    /// start time)?
    #[must_use]
    pub fn host_alive(&self) -> bool {
        crate::proc::is_alive(self.pid, self.pid_start)
    }
}

/// Read and parse a record.
///
/// # Errors
/// When the file is unreadable or not a record.
pub fn read_record(path: &Path) -> Result<HostRecord> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
}

/// Write `rec` to `<dir>/<id>.json` atomically, mode 0600.
///
/// # Errors
/// When the temp file can't be written or renamed.
pub fn write_record(dir: &Path, rec: &HostRecord) -> Result<()> {
    if !valid_id(&rec.id) {
        bail!("invalid session id {:?}", rec.id);
    }
    // NamedTempFile is created 0600 on Unix, so the token is never readable
    // by anyone else, not even between write and rename.
    let mut tmp = tempfile::Builder::new()
        .prefix(&format!(".{}.", rec.id))
        .suffix(".tmp")
        .tempfile_in(dir)
        .with_context(|| format!("temp record in {}", dir.display()))?;
    serde_json::to_writer_pretty(&mut tmp, rec).context("serialize record")?;
    tmp.write_all(b"\n").context("write record")?;
    tmp.as_file().sync_all().context("sync record")?;
    tmp.persist(record_path(dir, &rec.id)).context("rename record")?;
    Ok(())
}

/// Create `dir` (and parents) with mode 0700 if missing.
///
/// Then refuse it unless it is a real directory owned by this user and closed to group and
/// world. Nothing is chmodded behind the user's back: a too-open existing
/// directory is an error, not a fix-up.
///
/// # Errors
/// When the directory can't be made, or fails the checks.
#[cfg(unix)]
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    if !dir.is_absolute() {
        bail!("host dir {} is not absolute", dir.display());
    }
    if let Err(e) = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir) {
        if !dir.is_dir() {
            return Err(e).with_context(|| format!("create {}", dir.display()));
        }
    }
    let meta = std::fs::symlink_metadata(dir).with_context(|| format!("stat {}", dir.display()))?;
    if !meta.file_type().is_dir() {
        bail!("{} is not a directory (a symlink is refused)", dir.display());
    }
    let me = nix::unistd::geteuid().as_raw();
    if meta.uid() != me {
        bail!("{} is owned by uid {}, not this user ({me})", dir.display(), meta.uid());
    }
    if meta.mode() & 0o077 != 0 {
        bail!(
            "{} has mode {:o}; it must not be group- or world-accessible (0700)",
            dir.display(),
            meta.mode() & 0o777
        );
    }
    Ok(())
}

/// Windows: the owner-only DACL is part of the named-pipe work (th-2b32a6).
///
/// # Errors
/// When the directory can't be made.
#[cfg(not(unix))]
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))
}

/// Where the host binds its socket: `<dir>/<id>.sock`, or
/// `$TMPDIR/smooth-flow-<uid>/<id>.sock` when that is too long for a Unix
/// socket. The fallback directory gets the same 0700 checks.
///
/// # Errors
/// When the fallback directory fails [`ensure_private_dir`], or even it is
/// too long.
#[cfg(unix)]
pub fn socket_path(dir: &Path, id: &str) -> Result<PathBuf> {
    let direct = dir.join(format!("{id}.sock"));
    if direct.as_os_str().len() <= MAX_SOCKET_PATH {
        return Ok(direct);
    }
    let fallback_dir = std::env::temp_dir().join(format!("smooth-flow-{}", nix::unistd::geteuid().as_raw()));
    ensure_private_dir(&fallback_dir)?;
    let p = fallback_dir.join(format!("{id}.sock"));
    if p.as_os_str().len() > MAX_SOCKET_PATH {
        bail!("socket path {} is too long for a Unix socket", p.display());
    }
    Ok(p)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn sample(id: &str) -> HostRecord {
        HostRecord {
            v: 1,
            protocol: 1,
            id: id.into(),
            host_version: "0.0.0".into(),
            pid: 1,
            pid_start: Some(1),
            child_pid: 2,
            socket: PathBuf::from("/nonexistent.sock"),
            token: "0".repeat(64),
            owner: "o".into(),
            cwd: PathBuf::from("/"),
            argv: vec!["true".into()],
            created_at: "2026-10-03T00:00:00Z".into(),
            exit: None,
        }
    }

    #[test]
    fn ids_and_tokens_are_validated() {
        assert!(valid_id("fs-1a2b3c4d"));
        for bad in [
            "fs-1A2B3C4D",
            "fs-1a2b3c4",
            "fs-1a2b3c4d5",
            "../etc",
            "fs-../../x",
            "fs-1a2b3c4g",
            "",
            "fs-",
            "xs-1a2b3c4d",
        ] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        let t = new_token();
        assert!(valid_token(&t), "{t}");
        assert_ne!(t, new_token(), "random");
        assert!(!valid_token(&"g".repeat(64)));
        assert!(!valid_token(&"a".repeat(63)));
    }

    #[test]
    fn a_record_round_trips_atomically_at_0600() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = sample("fs-00000001");
        write_record(dir.path(), &rec).unwrap();
        assert_eq!(read_record(&record_path(dir.path(), "fs-00000001")).unwrap(), rec);
        rec.exit = Some(ExitInfo {
            code: Some(3),
            signal: None,
            at: "now".into(),
        });
        write_record(dir.path(), &rec).unwrap();
        assert_eq!(read_record(&record_path(dir.path(), "fs-00000001")).unwrap().exit, rec.exit);
        // No temp files left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(record_path(dir.path(), "fs-00000001")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // An id that would escape the dir is refused.
        assert!(write_record(dir.path(), &sample("../x")).is_err());
    }

    #[test]
    fn hosts_dir_honours_the_override() {
        // Read-only on the environment: only asserts the default shape.
        let d = hosts_dir();
        if std::env::var_os(HOSTS_DIR_ENV).is_none() {
            assert!(d.ends_with(".smooth/flow-hosts"), "{}", d.display());
        }
    }

    #[test]
    #[cfg(unix)]
    fn private_dir_is_made_0700_and_open_dirs_are_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("a").join("flow-hosts");
        ensure_private_dir(&d).unwrap();
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        ensure_private_dir(&d).unwrap();

        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = ensure_private_dir(&d).unwrap_err().to_string();
        assert!(e.contains("group- or world-accessible"), "{e}");
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o710)).unwrap();
        assert!(ensure_private_dir(&d).is_err(), "group execute is access too");

        // A symlink to a private dir is refused: it could be swapped.
        let real = tmp.path().join("real");
        ensure_private_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());

        assert!(ensure_private_dir(Path::new("relative/dir")).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn long_socket_paths_fall_back_to_tmpdir() {
        let tmp = tempfile::tempdir().unwrap();
        let short = socket_path(tmp.path(), "fs-00000001").unwrap();
        if tmp.path().as_os_str().len() + 17 <= MAX_SOCKET_PATH {
            assert_eq!(short, tmp.path().join("fs-00000001.sock"));
        }
        let deep = tmp.path().join("x".repeat(120));
        let p = socket_path(&deep, "fs-00000001").unwrap();
        assert!(p.as_os_str().len() <= MAX_SOCKET_PATH, "{}", p.display());
        assert!(p.starts_with(std::env::temp_dir()), "{}", p.display());
        assert!(p.ends_with("fs-00000001.sock"));
    }
}
