//! The flow tmux server.
//!
//! ONE long-lived server (`tmux -L smooth-flow`) that outlives the daemon, so
//! an app crash or a daemon restart never kills an agent's PTY. Sessions are
//! named after the flow session id. Every call names its socket explicitly
//! (th-d33afa): a session records the socket it was created on, so a daemon
//! restarted with a different `--tmux-socket` still finds its old panes.
//!
//! `remain-on-exit` is on: a dead pane stays until the engine has read its
//! exit status (`#{pane_dead_status}` — the PTY's own report, which is the
//! only proof of exit 0 the spec accepts, rule 5), then the engine kills it.
//!
//! Send/capture reuse `smooth_tmux::TmuxDriver::open_existing` (non-owning),
//! which gives bracketed-paste sends and scrollback capture for free.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use smooth_tmux::TmuxDriver;

/// The tmux socket name every flow session lives on.
pub const FLOW_SOCKET: &str = "smooth-flow";

/// Scrollback kept per pane.
const HISTORY_LIMIT: &str = "50000";

/// `PANE_WIDTH`/`PANE_HEIGHT` from smooth-tmux are tuned for scraping; a
/// flow session is sized by its first attaching client, so start modest.
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

/// The default socket for NEW sessions.
///
/// `$SMOOTH_FLOW_TMUX_SOCKET` (which `smooth-daemon --tmux-socket` sets)
/// overrides so tests, a second daemon and the macOS shell (`tmux -L
/// smoothflow`, whose server the app starts so TCC grants attribute to it)
/// never share a server.
#[must_use]
pub fn socket_name() -> String {
    std::env::var("SMOOTH_FLOW_TMUX_SOCKET")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| FLOW_SOCKET.to_string())
}

/// Every tmux client the engine runs is `-u`: a client whose environment
/// carries no UTF-8 locale (the SmoothFlow child daemon is launched by a
/// Finder-started app, which has no `LANG`) is otherwise treated as a
/// non-UTF-8 terminal and tmux draws `_` for every non-ASCII cell — the
/// blank Nerd Font prompt icons of th-bcd819. Forcing UTF-8 on the client
/// side needs no locale at all.
const UTF8_FLAG: &str = "-u";

fn tmux(socket: &str, args: &[&str]) -> Result<std::process::Output> {
    let mut full: Vec<&str> = vec![UTF8_FLAG, "-L", socket];
    full.extend_from_slice(args);
    Command::new("tmux").args(&full).output().context("running tmux")
}

fn tmux_ok(socket: &str, args: &[&str]) -> Result<String> {
    let out = tmux(socket, args)?;
    if !out.status.success() {
        return Err(anyhow!("tmux {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Shell-quote one argv element for `sh -c`.
#[must_use]
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The `sh -c` command that `exec`s `argv` — so the pane pid IS the agent's
/// pid, not a wrapper shell's.
#[must_use]
pub fn exec_command(argv: &[String]) -> String {
    exec_command_env(argv, &[])
}

/// [`exec_command`] preceded by `export K=V;` for each env pair.
#[must_use]
pub fn exec_command_env(argv: &[String], env: &[(String, String)]) -> String {
    let quoted: Vec<String> = argv.iter().map(|a| shell_quote(a)).collect();
    let exports = env.iter().fold(String::new(), |mut acc, (k, v)| {
        use std::fmt::Write as _;
        let _ = write!(acc, "export {k}={}; ", shell_quote(v));
        acc
    });
    format!("{exports}exec {}", quoted.join(" "))
}

/// Start the flow server (if needed) with the options every session relies
/// on, in ONE tmux invocation so they hold before the first pane exists —
/// a command that exits instantly would otherwise take the server (and its
/// exit status) with it before `remain-on-exit` was set. `exit-empty off`
/// keeps the server alive with zero sessions so options persist.
fn ensure_server(socket: &str) {
    let _ = tmux(
        socket,
        &[
            "start-server",
            ";",
            "set-option",
            "-s",
            "exit-empty",
            "off",
            ";",
            "set-option",
            "-s",
            "escape-time",
            "0",
            ";",
            "set-option",
            "-g",
            "remain-on-exit",
            "on",
            ";",
            "set-option",
            "-g",
            "status",
            "off",
            ";",
            "set-option",
            "-g",
            "history-limit",
            HISTORY_LIMIT,
            ";",
            "set-option",
            "-g",
            "window-size",
            "latest",
            ";",
            "set-option",
            "-g",
            "mouse",
            "off",
            ";",
            "set-option",
            "-g",
            "set-titles",
            "off",
        ],
    );
}

/// Is `session` present on the flow server?
#[must_use]
pub fn session_alive(socket: &str, session: &str) -> bool {
    tmux(socket, &["has-session", "-t", session]).is_ok_and(|o| o.status.success())
}

/// Create a detached session running `exec argv` in `cwd`. Returns the
/// pane's pid.
///
/// # Errors
/// When tmux is missing or the session cannot be created.
pub fn launch(socket: &str, session: &str, cwd: &Path, argv: &[String]) -> Result<u32> {
    launch_env(socket, session, cwd, argv, &[])
}

/// [`launch`] with extra environment exported into the pane's shell before
/// the `exec` (th-0f6126: a manifest's `launch.env`).
///
/// # Errors
/// When tmux is missing or the session cannot be created.
pub fn launch_env(socket: &str, session: &str, cwd: &Path, argv: &[String], env: &[(String, String)]) -> Result<u32> {
    if argv.is_empty() {
        return Err(anyhow!("cannot launch an empty argv"));
    }
    let cmd = exec_command_env(argv, env);
    let cwd_s = cwd.to_string_lossy();
    ensure_server(socket);
    let out = tmux(
        socket,
        &[
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            &DEFAULT_COLS.to_string(),
            "-y",
            &DEFAULT_ROWS.to_string(),
            "-c",
            &cwd_s,
            "sh",
            "-c",
            &cmd,
        ],
    )?;
    if !out.status.success() {
        return Err(anyhow!("tmux new-session `{session}` failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    pane_pid(socket, session)
}

/// The pane's process id.
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn pane_pid(socket: &str, session: &str) -> Result<u32> {
    let s = tmux_ok(socket, &["display-message", "-p", "-t", session, "#{pane_pid}"])?;
    s.trim().parse::<u32>().with_context(|| format!("pane_pid `{s}`"))
}

/// `Some(exit_status)` once the pane's process has exited (remain-on-exit
/// keeps the pane), `None` while it runs.
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn pane_exit_status(socket: &str, session: &str) -> Result<Option<i32>> {
    let s = tmux_ok(socket, &["display-message", "-p", "-t", session, "#{pane_dead}|#{pane_dead_status}"])?;
    tracing::trace!(socket, session, raw = ?s, "tmux: pane_dead query");
    Ok(parse_pane_dead(&s))
}

/// Parse `#{pane_dead}|#{pane_dead_status}`: `None` while the pane runs,
/// `Some(status)` once it died (`-1` when tmux has no status for it).
///
/// The separator is `|`, NOT a tab: under a non-UTF-8 locale (no `LANG` —
/// a launchd-started daemon, a CI runner, an `env -i`) tmux rewrites every
/// control character in `display-message -p` output to `_`, so a tab-joined
/// format read as `1_2` and the engine never saw a pane die (th-8e3087).
#[must_use]
pub fn parse_pane_dead(raw: &str) -> Option<i32> {
    let mut parts = raw.split('|');
    let dead = parts.next().unwrap_or("0").trim() == "1";
    if !dead {
        return None;
    }
    Some(parts.next().unwrap_or("").trim().parse::<i32>().unwrap_or(-1))
}

/// `(cols, rows)` of the pane.
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn pane_size(socket: &str, session: &str) -> Result<(u16, u16)> {
    let s = tmux_ok(socket, &["display-message", "-p", "-t", session, "#{pane_width}|#{pane_height}"])?;
    Ok(parse_pane_size(&s))
}

/// Parse `#{pane_width}|#{pane_height}` (80×24 when a half is unreadable).
/// `|`-joined for the same locale reason as [`parse_pane_dead`].
#[must_use]
pub fn parse_pane_size(raw: &str) -> (u16, u16) {
    let mut parts = raw.split('|');
    let cols = parts.next().unwrap_or("80").trim().parse().unwrap_or(80);
    let rows = parts.next().unwrap_or("24").trim().parse().unwrap_or(24);
    (cols, rows)
}

/// Plain-text capture of the visible pane.
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn capture_visible(socket: &str, session: &str) -> Result<String> {
    driver(socket, session).capture_visible()
}

/// Capture including scrollback, front-truncated to the driver's budget.
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn capture_scrollback(socket: &str, session: &str) -> Result<String> {
    driver(socket, session).capture()
}

/// Bracketed-paste `text` + Enter into the pane.
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn send_text(socket: &str, session: &str, text: &str) -> Result<()> {
    driver(socket, session).send(text)
}

/// A named key (`Enter`, `Escape`, `C-c`, `1`).
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn send_key(socket: &str, session: &str, key: &str) -> Result<()> {
    driver(socket, session).send_key(key)
}

/// Kill the session (never the server).
pub fn kill_session(socket: &str, session: &str) {
    let _ = tmux(socket, &["kill-session", "-t", session]);
}

/// Kill the whole flow server (tests / `th flow` never call this).
pub fn kill_server(socket: &str) {
    let _ = tmux(socket, &["kill-server"]);
}

/// `tmux attach` argv for the PTY bridge — the whole reason the socket is
/// stable.
#[must_use]
pub fn attach_argv(socket: &str, session: &str) -> Vec<String> {
    vec![
        "tmux".into(),
        UTF8_FLAG.into(),
        "-L".into(),
        socket.into(),
        "attach-session".into(),
        "-t".into(),
        session.into(),
    ]
}

fn driver(socket: &str, session: &str) -> TmuxDriver {
    TmuxDriver::open_existing(socket, session)
}

/// True when a `tmux` binary runs.
#[must_use]
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Tests that touch `SMOOTH_FLOW_TMUX_SOCKET` serialize on this: cargo runs
/// tests on threads and the process env is shared.
#[cfg(test)]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the env lock (crate-wide test helper).
#[cfg(test)]
pub(crate) fn tests_env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_exec_command() {
        assert_eq!(shell_quote("claude"), "claude");
        assert_eq!(shell_quote("--session-id=abc"), "--session-id=abc");
        assert_eq!(shell_quote("say hi"), "'say hi'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(
            exec_command(&["claude".into(), "--session-id".into(), "u".into(), "say hi".into()]),
            "exec claude --session-id u 'say hi'"
        );
    }

    /// th-8e3087: tmux under a C locale turns a tab into `_` — the joined
    /// format must survive that, and the parsers must read what tmux prints.
    #[test]
    fn pane_queries_parse_without_a_tab_separator() {
        assert_eq!(parse_pane_dead("0|"), None);
        assert_eq!(parse_pane_dead("0|0"), None);
        assert_eq!(parse_pane_dead("1|2"), Some(2));
        assert_eq!(parse_pane_dead("1|0"), Some(0));
        assert_eq!(parse_pane_dead("1|"), Some(-1));
        assert_eq!(parse_pane_dead(""), None);
        // What a tab-joined format came back as under `LANG` unset.
        assert_eq!(parse_pane_dead("1_2"), None, "the old format read as alive — the bug");
        assert_eq!(parse_pane_size("120|40"), (120, 40));
        assert_eq!(parse_pane_size("garbage"), (80, 24));
        assert_eq!(parse_pane_size("100|"), (100, 24));
    }

    #[test]
    fn attach_argv_targets_the_flow_socket() {
        let a = attach_argv("smoothflow", "fs-1");
        assert_eq!(a[0], "tmux");
        // th-bcd819: forced UTF-8, or a LANG-less client (the app's child
        // daemon) gets `_` for every Nerd Font glyph.
        assert_eq!(a[1], "-u");
        assert_eq!(a[2], "-L");
        assert_eq!(a[3], "smoothflow");
        assert_eq!(a[4], "attach-session");
        assert_eq!(a[6], "fs-1");
    }

    #[test]
    fn socket_name_env_override() {
        let _g = tests_env_lock();
        std::env::set_var("SMOOTH_FLOW_TMUX_SOCKET", "flow-test-sock");
        assert_eq!(socket_name(), "flow-test-sock");
        std::env::remove_var("SMOOTH_FLOW_TMUX_SOCKET");
        assert_eq!(socket_name(), FLOW_SOCKET);
    }

    #[test]
    fn live_launch_exit_status_and_kill() {
        if !tmux_available() {
            eprintln!("skipping: tmux not available");
            return;
        }
        // The socket is explicit per call (th-d33afa) — no env, no lock.
        let sock = format!("flow-t-{}", std::process::id());
        let dir = tempfile::tempdir().unwrap();
        let session = "fs-livetest";
        let pid = launch(&sock, session, dir.path(), &["sh".into(), "-c".into(), "echo READY; exit 7".into()]).unwrap();
        assert!(pid > 0);
        // remain-on-exit keeps the pane so the exit code is readable.
        // tmux reports the dead status before it has necessarily drained the
        // last output, so poll for both.
        let mut status = None;
        let mut text = String::new();
        for _ in 0..100 {
            status = pane_exit_status(&sock, session).unwrap();
            // A dead pane's last line scrolls into history behind tmux's
            // "Pane is dead" banner, so read the scrollback.
            text = capture_scrollback(&sock, session).unwrap();
            if status.is_some() && text.contains("READY") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(status, Some(7), "PTY-reported exit status");
        assert!(session_alive(&sock, session));
        assert!(!session_alive("flow-t-other-socket", session), "sessions are per socket");
        assert!(text.contains("READY"), "{text}");
        let (cols, rows) = pane_size(&sock, session).unwrap();
        assert!(cols > 0 && rows > 0);
        kill_session(&sock, session);
        assert!(!session_alive(&sock, session));
        kill_server(&sock);
    }
}
