//! Finding the `tmux` executable (pearl th-9f6814).
//!
//! Every tmux call used to be `Command::new("tmux")`, a bare-name `PATH`
//! lookup. A daemon launched from Finder (Big Smooth.app, the SmoothFlow
//! app) gets `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, and Homebrew's tmux lives
//! in `/opt/homebrew/bin`, so every SmoothFlow session sat in `starting`
//! forever with nothing logged. [`tmux_bin`] resolves the binary once, in
//! this order:
//!
//! 1. `$SMOOTH_TMUX_BIN` — an explicit override. When it is set, it is the
//!    ONLY answer: a broken override is an error, never a silent fallback.
//! 2. `tmux` on this process's `PATH`.
//! 3. The well-known install locations ([`KNOWN_TMUX_PATHS`]).
//!
//! A hit is cached for the life of the process; a miss is not, so installing
//! tmux while the daemon runs is picked up by the next launch.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};

/// The override knob: a path to (or a bare name of) the tmux to run.
pub const TMUX_BIN_ENV: &str = "SMOOTH_TMUX_BIN";

/// Where tmux lives when `PATH` doesn't say: Homebrew (Apple silicon, then
/// Intel), MacPorts, the system.
pub const KNOWN_TMUX_PATHS: &[&str] = &["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux", "/opt/local/bin/tmux", "/usr/bin/tmux"];

static RESOLVED: OnceLock<PathBuf> = OnceLock::new();

/// The tmux executable, resolved as the module docs describe and cached
/// after the first hit.
///
/// On Windows this is the bare name `tmux` (unchanged behavior: tmux there
/// only exists inside WSL, which resolves it itself).
///
/// # Errors
/// When no tmux can be found; the message names every place that was
/// searched and how to install it.
pub fn tmux_bin() -> Result<PathBuf> {
    if cfg!(windows) {
        return Ok(PathBuf::from("tmux"));
    }
    if let Some(p) = RESOLVED.get() {
        return Ok(p.clone());
    }
    let found = find_tmux(std::env::var_os(TMUX_BIN_ENV).as_deref(), std::env::var_os("PATH").as_deref(), KNOWN_TMUX_PATHS)?;
    Ok(RESOLVED.get_or_init(|| found).clone())
}

/// A [`Command`] for the resolved tmux. When none resolves it falls back to
/// the bare name, so the spawn fails exactly as it always did; callers that
/// must explain a missing tmux call [`tmux_bin`] first.
#[must_use]
pub fn tmux_command() -> Command {
    Command::new(tmux_bin().unwrap_or_else(|_| PathBuf::from("tmux")))
}

/// [`tmux_bin`] as a pure function of its inputs (the testable core; nothing
/// is cached).
///
/// # Errors
/// When the override names nothing runnable, or (without an override)
/// neither `path` nor `known` holds an executable `tmux`.
pub fn find_tmux(override_bin: Option<&OsStr>, path: Option<&OsStr>, known: &[&str]) -> Result<PathBuf> {
    if let Some(o) = override_bin.filter(|o| !o.is_empty()) {
        let p = Path::new(o);
        let hit = if p.components().count() > 1 {
            is_executable(p).then(|| p.to_path_buf())
        } else {
            path.and_then(|path| on_path(o, path))
        };
        return hit.ok_or_else(|| {
            anyhow!(
                "tmux not found: ${TMUX_BIN_ENV}={} is not an executable (unset it to search PATH and {})",
                p.display(),
                known_dirs(known)
            )
        });
    }
    if let Some(p) = path.and_then(|path| on_path(OsStr::new("tmux"), path)) {
        return Ok(p);
    }
    if let Some(p) = known.iter().map(PathBuf::from).find(|p| is_executable(p)) {
        return Ok(p);
    }
    Err(anyhow!("tmux not found (looked in PATH, {}): {}", known_dirs(known), install_hint()))
}

/// `name` in the first `path` directory where it is executable.
fn on_path(name: &OsStr, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

fn known_dirs(known: &[&str]) -> String {
    let dirs: Vec<String> = known
        .iter()
        .map(|k| Path::new(k).parent().map_or_else(|| (*k).to_string(), |d| d.display().to_string()))
        .collect();
    dirs.join(", ")
}

/// How to install tmux on this OS.
#[must_use]
pub fn install_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "install it with `brew install tmux`"
    } else {
        "install it with your package manager (e.g. `apt install tmux`)"
    }
}

/// Whether `p` is a file this user may execute.
#[must_use]
pub fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Join directories into a `PATH` value (test helper and caller convenience).
#[must_use]
pub fn join_path<I: IntoIterator<Item = P>, P: AsRef<Path>>(dirs: I) -> OsString {
    std::env::join_paths(dirs.into_iter().map(|d| d.as_ref().to_path_buf())).unwrap_or_default()
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    /// A scratch dir holding an executable `tmux` script.
    fn fake_tmux(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join("tmux");
        std::fs::write(&p, "#!/bin/sh\necho 'tmux 9.9'\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn override_wins_over_path_and_known() {
        let tmp = tempfile::tempdir().unwrap();
        let on_path = fake_tmux(&tmp.path().join("onpath"));
        let pinned = fake_tmux(&tmp.path().join("pinned"));
        let path = join_path([on_path.parent().unwrap()]);
        let got = find_tmux(Some(pinned.as_os_str()), Some(&path), &[on_path.to_str().unwrap()]).unwrap();
        assert_eq!(got, pinned);
    }

    #[test]
    fn a_broken_override_is_an_error_not_a_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let on_path = fake_tmux(&tmp.path().join("onpath"));
        let path = join_path([on_path.parent().unwrap()]);
        let missing = tmp.path().join("nope").join("tmux");
        let err = find_tmux(Some(missing.as_os_str()), Some(&path), &[]).unwrap_err().to_string();
        assert!(err.contains(TMUX_BIN_ENV) && err.contains("nope"), "{err}");
    }

    #[test]
    fn a_bare_name_override_is_looked_up_on_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("bin");
        fake_tmux(&dir);
        std::fs::rename(dir.join("tmux"), dir.join("tmux-next")).unwrap();
        let path = join_path([&dir]);
        assert_eq!(find_tmux(Some(OsStr::new("tmux-next")), Some(&path), &[]).unwrap(), dir.join("tmux-next"));
    }

    #[test]
    fn path_wins_over_known_locations() {
        let tmp = tempfile::tempdir().unwrap();
        let on_path = fake_tmux(&tmp.path().join("onpath"));
        let known = fake_tmux(&tmp.path().join("brew"));
        let path = join_path([on_path.parent().unwrap()]);
        assert_eq!(find_tmux(None, Some(&path), &[known.to_str().unwrap()]).unwrap(), on_path);
    }

    #[test]
    fn a_minimal_path_falls_back_to_known_locations() {
        let tmp = tempfile::tempdir().unwrap();
        let brew = fake_tmux(&tmp.path().join("opt-homebrew-bin"));
        // Finder's PATH, minus anything that might really hold a tmux.
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let path = join_path([&empty]);
        let missing = tmp.path().join("missing").join("tmux");
        let got = find_tmux(None, Some(&path), &[missing.to_str().unwrap(), brew.to_str().unwrap()]).unwrap();
        assert_eq!(got, brew);
    }

    #[test]
    fn a_non_executable_tmux_is_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let plain = fake_tmux(&tmp.path().join("plain"));
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let good = fake_tmux(&tmp.path().join("good"));
        let path = join_path([plain.parent().unwrap()]);
        assert_eq!(find_tmux(None, Some(&path), &[good.to_str().unwrap()]).unwrap(), good);
    }

    #[test]
    fn nothing_found_names_every_place_searched_and_the_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let path = join_path([tmp.path()]);
        let err = find_tmux(
            None,
            Some(&path),
            KNOWN_TMUX_PATHS.iter().map(|_| "/nonexistent/x/tmux").collect::<Vec<_>>().as_slice(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.starts_with("tmux not found (looked in PATH, /nonexistent/x"), "{err}");
        assert!(err.contains("install it"), "{err}");
        let real = find_tmux(None, None, &[]).unwrap_err().to_string();
        assert!(real.contains("tmux not found"), "{real}");
    }

    #[test]
    fn the_known_list_covers_homebrew_first() {
        assert_eq!(KNOWN_TMUX_PATHS[0], "/opt/homebrew/bin/tmux");
        assert!(KNOWN_TMUX_PATHS.contains(&"/usr/local/bin/tmux"));
        assert!(KNOWN_TMUX_PATHS.contains(&"/opt/local/bin/tmux"));
        assert!(KNOWN_TMUX_PATHS.contains(&"/usr/bin/tmux"));
    }
}
