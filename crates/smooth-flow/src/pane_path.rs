//! The `PATH` a session's pane runs with (pearl th-9f6814).
//!
//! The tmux server, and so every pane, inherits the daemon's environment. A
//! daemon launched from Finder (Big Smooth.app, the SmoothFlow app) has
//! `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, so its panes can't find `claude`,
//! `node` or `git` from Homebrew, nvm, `~/.local/bin`…
//!
//! [`pane_path`] is what every launch exports instead: the daemon's own
//! `PATH` merged with the user's login-shell `PATH` (captured once, by
//! running `$SHELL -l -i -c` with a timeout) and the well-known tool
//! directories. Order, de-duplicated, first wins:
//!
//! 1. the daemon's non-system entries (a daemon started from a terminal, or
//!    a test rig that puts fake CLIs first, keeps exactly its precedence);
//! 2. the login shell's entries;
//! 3. the daemon's system entries (`/usr/bin`, `/bin`, `/usr/sbin`, `/sbin`)
//!    — after the login ones, so Homebrew's `git`/`python3` beat Apple's
//!    stubs the way they do in the user's terminal;
//! 4. [`KNOWN_DIRS`] and `~/.local/bin`, `~/.cargo/bin` that exist.
//!
//! `SMOOTH_FLOW_LOGIN_PATH=0` skips the login-shell capture.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The knob that turns the login-shell capture off.
pub const LOGIN_PATH_ENV: &str = "SMOOTH_FLOW_LOGIN_PATH";

/// System directories that go AFTER the login shell's entries.
const SYSTEM_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Tool directories appended last, when they exist.
pub const KNOWN_DIRS: &[&str] = &["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin", "/opt/local/bin"];

/// How long the login shell may take to print its `PATH`.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

const MARK_START: &str = "__SMOOTH_PANE_PATH_START__";
const MARK_END: &str = "__SMOOTH_PANE_PATH_END__";

static PANE_PATH: OnceLock<OsString> = OnceLock::new();

/// The pane `PATH` for this daemon, computed once (the first call may run
/// the login shell, up to [`CAPTURE_TIMEOUT`]).
#[must_use]
pub fn pane_path() -> OsString {
    PANE_PATH
        .get_or_init(|| {
            let daemon = std::env::var_os("PATH").unwrap_or_default();
            let login = login_path();
            let home = dirs_next::home_dir();
            let merged = merge(&daemon, login.as_deref(), &existing_known_dirs(home.as_deref()));
            tracing::info!(
                login_shell = login.is_some(),
                path = %merged.to_string_lossy(),
                "flow: pane PATH resolved"
            );
            merged
        })
        .clone()
}

/// Warm [`pane_path`] on a background thread, so the first launch does not
/// wait on the login shell.
pub fn warm() {
    if PANE_PATH.get().is_none() {
        let _ = std::thread::Builder::new().name("flow-pane-path".into()).spawn(|| {
            let _ = pane_path();
        });
    }
}

/// [`KNOWN_DIRS`] plus `~/.local/bin` and `~/.cargo/bin`, the ones that exist.
fn existing_known_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = KNOWN_DIRS.iter().map(PathBuf::from).collect();
    if let Some(h) = home {
        dirs.push(h.join(".local").join("bin"));
        dirs.push(h.join(".cargo").join("bin"));
    }
    dirs.retain(|d| d.is_dir());
    dirs
}

/// The merge described in the module docs (pure).
#[must_use]
pub fn merge(daemon: &OsStr, login: Option<&OsStr>, known: &[PathBuf]) -> OsString {
    let is_system = |d: &Path| SYSTEM_DIRS.iter().any(|s| d == Path::new(s));
    let daemon: Vec<PathBuf> = std::env::split_paths(daemon).filter(|d| !d.as_os_str().is_empty()).collect();
    let login: Vec<PathBuf> = login
        .map(|l| std::env::split_paths(l).filter(|d| !d.as_os_str().is_empty()).collect())
        .unwrap_or_default();
    let mut out: Vec<PathBuf> = Vec::new();
    let ordered = daemon
        .iter()
        .filter(|d| !is_system(d))
        .chain(login.iter())
        .chain(daemon.iter().filter(|d| is_system(d)))
        .chain(known.iter());
    for d in ordered {
        if !out.contains(d) {
            out.push(d.clone());
        }
    }
    std::env::join_paths(out).unwrap_or_default()
}

/// The user's login-shell `PATH`, or `None` (disabled, no shell, timeout,
/// garbage). Never in this crate's own unit tests: they must not run the
/// developer's dotfiles.
fn login_path() -> Option<OsString> {
    if cfg!(test) || cfg!(windows) {
        return None;
    }
    if std::env::var(LOGIN_PATH_ENV).is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("false")) {
        return None;
    }
    let shell = login_shell()?;
    match capture(&shell, CAPTURE_TIMEOUT) {
        Ok(p) => Some(p),
        Err(why) => {
            tracing::warn!(shell = %shell.display(), %why, "flow: could not read the login shell's PATH; panes get the daemon's PATH plus the well-known tool directories");
            None
        }
    }
}

/// `$SHELL`, else (a Finder-launched app may have none) `/bin/zsh` on macOS.
fn login_shell() -> Option<PathBuf> {
    if let Some(s) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(s));
    }
    let fallback = if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/sh" };
    Path::new(fallback).is_file().then(|| PathBuf::from(fallback))
}

/// Run `shell -l -i -c` and pull `$PATH` out from between markers (an
/// interactive shell's rc files may print anything around it).
///
/// # Errors
/// A human reason: spawn failure, timeout, no markers.
pub fn capture(shell: &Path, timeout: Duration) -> Result<OsString, String> {
    let script = format!("printf '\\n{MARK_START}%s{MARK_END}\\n' \"$PATH\"");
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Keep rc files from starting tmux / a prompt theme that waits on a tty.
        .env("TERM", "dumb")
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {timeout:?}"));
            }
            Err(e) => return Err(format!("wait: {e}")),
        }
    }
    let buf = reader.join().unwrap_or_default();
    extract(&String::from_utf8_lossy(&buf))
        .map(OsString::from)
        .ok_or_else(|| "no PATH in its output".to_string())
}

/// The text between the last start marker and the end marker after it.
fn extract(out: &str) -> Option<&str> {
    let start = out.rfind(MARK_START)? + MARK_START.len();
    let len = out[start..].find(MARK_END)?;
    Some(&out[start..start + len]).filter(|p| !p.is_empty())
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn p(s: &str) -> OsString {
        OsString::from(s)
    }

    fn split(s: &OsStr) -> Vec<String> {
        std::env::split_paths(s).map(|d| d.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_finder_daemon_gets_the_login_path_ahead_of_the_system_dirs() {
        let merged = merge(
            &p("/usr/bin:/bin:/usr/sbin:/sbin"),
            Some(&p("/Users/b/.local/bin:/opt/homebrew/bin:/usr/bin:/bin")),
            &[PathBuf::from("/usr/local/bin")],
        );
        assert_eq!(
            split(&merged),
            [
                "/Users/b/.local/bin",
                "/opt/homebrew/bin",
                "/usr/bin",
                "/bin",
                "/usr/sbin",
                "/sbin",
                "/usr/local/bin"
            ]
        );
    }

    #[test]
    fn a_terminal_daemon_keeps_its_own_precedence() {
        // The e2e rig puts a scratch bin dir first; it must stay first.
        let merged = merge(
            &p("/tmp/rig/bin:/opt/homebrew/bin:/usr/bin:/bin"),
            Some(&p("/usr/local/bin:/opt/homebrew/bin:/usr/bin")),
            &[],
        );
        assert_eq!(split(&merged), ["/tmp/rig/bin", "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]);
    }

    #[test]
    fn no_login_shell_still_appends_the_known_dirs() {
        let merged = merge(&p("/usr/bin:/bin"), None, &[PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/bin")]);
        assert_eq!(split(&merged), ["/usr/bin", "/bin", "/opt/homebrew/bin"]);
    }

    #[test]
    fn empty_entries_are_dropped() {
        let merged = merge(&p(":/usr/bin::"), Some(&p("")), &[]);
        assert_eq!(split(&merged), ["/usr/bin"]);
    }

    #[test]
    fn extract_ignores_rc_noise_around_the_markers() {
        let out = format!("welcome!\n{MARK_START}/a:/b{MARK_END}\nbye");
        assert_eq!(extract(&out), Some("/a:/b"));
        assert_eq!(extract("nothing here"), None);
        assert_eq!(extract(&format!("{MARK_START}{MARK_END}")), None);
    }

    #[test]
    fn capture_reads_a_shells_path_and_times_out_on_a_hung_one() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        // A "shell" that ignores its flags and prints a PATH with noise.
        let good = tmp.path().join("good-sh");
        std::fs::write(&good, format!("#!/bin/sh\necho rc-noise\nprintf '\\n{MARK_START}/x/bin:/y/bin{MARK_END}\\n'\n")).unwrap();
        let hung = tmp.path().join("hung-sh");
        std::fs::write(&hung, "#!/bin/sh\nexec sleep 30\n").unwrap();
        for f in [&good, &hung] {
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(capture(&good, Duration::from_secs(10)).unwrap(), p("/x/bin:/y/bin"));
        let err = capture(&hung, Duration::from_millis(300)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(capture(&tmp.path().join("missing"), Duration::from_secs(1)).unwrap_err().starts_with("spawn"));
    }

    #[test]
    fn known_dirs_only_lists_ones_that_exist() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".local").join("bin")).unwrap();
        let dirs = existing_known_dirs(Some(tmp.path()));
        assert!(dirs.contains(&tmp.path().join(".local").join("bin")));
        assert!(!dirs.contains(&tmp.path().join(".cargo").join("bin")));
    }
}
