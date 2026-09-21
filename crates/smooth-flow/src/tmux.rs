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

/// Terminal state a scrape rule may read (th-e77603): the OSC 0/2 title, the
/// alternate screen and the cursor row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaneMeta {
    pub title: String,
    pub alternate_on: bool,
    pub cursor_y: Option<usize>,
}

/// The format [`pane_meta`] asks for. The title goes LAST so a `|` inside it
/// survives the split; no TAB (tmux turns one into `_` without a UTF-8 locale).
const META_FORMAT: &str = "#{alternate_on}|#{cursor_y}|#{pane_title}";

/// Parse a [`META_FORMAT`] line.
#[must_use]
pub fn parse_pane_meta(line: &str) -> PaneMeta {
    let mut parts = line.splitn(3, '|');
    let alternate_on = parts.next().is_some_and(|a| a.trim() == "1");
    let cursor_y = parts.next().and_then(|c| c.trim().parse().ok());
    let title = parts.next().unwrap_or("").to_string();
    PaneMeta { title, alternate_on, cursor_y }
}

/// One `display-message` for [`PaneMeta`].
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn pane_meta(socket: &str, session: &str) -> Result<PaneMeta> {
    Ok(parse_pane_meta(&tmux_ok(socket, &["display-message", "-p", "-t", session, META_FORMAT])?))
}

/// Where a pane's process is in dying (remain-on-exit keeps the pane).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneLife {
    /// The process is running.
    Running,
    /// tmux has seen the pty close but has not reaped the process yet, so
    /// its exit status is not known (th-9d2578). Ask again: this is NOT an
    /// exit, and reading it as one records a bogus status.
    Unreaped,
    /// The process exited with this status, or `-1` when it died of a
    /// signal (or tmux predates `pane_dead_signal` and reports nothing).
    Exited(i32),
}

const DEAD_FORMAT: &str = "#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}";

/// The pane process's [`PaneLife`].
///
/// An [`PaneLife::Unreaped`] read gets one nudge before it is returned:
/// tmux can MISS a pane's SIGCHLD and leave the process a zombie with no
/// recorded status indefinitely (th-9d2578: about 1 in 100–300 fast-exiting
/// panes on Linux tmux 3.3a, the zombie still there seconds later). Its
/// handler reaps with `waitpid(WAIT_ANY)`, so any other child of the server
/// exiting collects the lost one too — `run-shell true` is that child, and
/// the re-read then carries the real status. A process that is genuinely
/// still running (its pty closed, its body not done) stays `Unreaped`.
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn pane_life(socket: &str, session: &str) -> Result<PaneLife> {
    let read = || -> Result<PaneLife> {
        let s = tmux_ok(socket, &["display-message", "-p", "-t", session, DEAD_FORMAT])?;
        tracing::trace!(socket, session, raw = ?s, "tmux: pane_dead query");
        Ok(parse_pane_dead(&s))
    };
    let life = read()?;
    if life != PaneLife::Unreaped {
        return Ok(life);
    }
    let _ = tmux(socket, &["run-shell", "true"]);
    read()
}

/// `Some(exit_status)` once the pane's process has exited and tmux has its
/// status, `None` while it runs or is still being reaped.
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn pane_exit_status(socket: &str, session: &str) -> Result<Option<i32>> {
    Ok(match pane_life(socket, session)? {
        PaneLife::Exited(code) => Some(code),
        PaneLife::Running | PaneLife::Unreaped => None,
    })
}

/// Parse `#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}`.
///
/// `pane_dead` is tmux's "the pty fd is closed", NOT "the process was
/// reaped". tmux closes the fd on the pty's EOF and records the status on
/// SIGCHLD, two separate events; in between — or for good, when tmux
/// misses the SIGCHLD (see [`pane_life`]) — a query reads `1||`. th-9d2578:
/// CI recorded exit `-1` for an agent that exited 2. So a dead pane with
/// neither a status nor a signal is [`PaneLife::Unreaped`], never an exit.
///
/// The separator is `|`, NOT a tab: under a non-UTF-8 locale (no `LANG` —
/// a launchd-started daemon, a CI runner, an `env -i`) tmux rewrites every
/// control character in `display-message -p` output to `_`, so a tab-joined
/// format read as `1_2` and the engine never saw a pane die (th-8e3087).
#[must_use]
pub fn parse_pane_dead(raw: &str) -> PaneLife {
    let mut parts = raw.split('|').map(str::trim);
    if parts.next() != Some("1") {
        return PaneLife::Running;
    }
    let status = parts.next().unwrap_or("");
    let signal = parts.next().unwrap_or("");
    match status.parse::<i32>() {
        Ok(code) => PaneLife::Exited(code),
        Err(_) if !signal.is_empty() => PaneLife::Exited(-1),
        Err(_) => PaneLife::Unreaped,
    }
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

/// Bracketed-paste `text` into the pane without submitting it.
///
/// # Errors
/// When the session is gone or tmux fails.
pub fn paste_text(socket: &str, session: &str, text: &str) -> Result<()> {
    driver(socket, session).paste(text)
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
    fn pane_meta_parses_and_keeps_pipes_in_the_title() {
        assert_eq!(
            parse_pane_meta("1|38|crush /tmp/a|b"),
            PaneMeta {
                title: "crush /tmp/a|b".into(),
                alternate_on: true,
                cursor_y: Some(38)
            }
        );
        assert_eq!(
            parse_pane_meta("0||"),
            PaneMeta {
                title: String::new(),
                alternate_on: false,
                cursor_y: None
            }
        );
        assert_eq!(parse_pane_meta(""), PaneMeta::default());
    }

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
    /// th-9d2578: the pty closed but tmux has not reaped the process yet —
    /// no status, no signal. That is not an exit (it used to read as `-1`).
    /// A signal death has a signal and no status (3.3 prints a number, 3.5
    /// a name).
    #[test]
    fn a_dead_pane_without_a_status_is_unreaped_not_exited() {
        assert_eq!(parse_pane_dead("1||"), PaneLife::Unreaped);
        assert_eq!(parse_pane_dead("1|"), PaneLife::Unreaped);
        assert_eq!(parse_pane_dead("1"), PaneLife::Unreaped);
        assert_eq!(parse_pane_dead("1||term"), PaneLife::Exited(-1));
        assert_eq!(parse_pane_dead("1||1"), PaneLife::Exited(-1));
        assert_eq!(parse_pane_dead("1|2|"), PaneLife::Exited(2), "a status wins");
    }

    #[test]
    fn pane_queries_parse_without_a_tab_separator() {
        assert_eq!(parse_pane_dead("0||"), PaneLife::Running);
        assert_eq!(parse_pane_dead("0|0|"), PaneLife::Running);
        assert_eq!(parse_pane_dead("1|2|"), PaneLife::Exited(2));
        assert_eq!(parse_pane_dead("1|0|"), PaneLife::Exited(0));
        assert_eq!(parse_pane_dead(""), PaneLife::Running);
        // What a tab-joined format came back as under `LANG` unset.
        assert_eq!(parse_pane_dead("1_2"), PaneLife::Running, "the old format read as alive — the bug");
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
        // The process prints, then waits to be told to exit. Printing and
        // exiting at once is a race this test must not depend on: on Linux
        // the pty can report EIO before the last output is readable, and
        // tmux then never sees it (th-9d2578 measured ~1 in 25 runs).
        let go = dir.path().join("go");
        let script = format!("echo READY; while [ ! -e '{}' ]; do sleep 0.05; done; exit 7", go.display());
        let pid = launch(&sock, session, dir.path(), &["sh".into(), "-c".into(), script]).unwrap();
        assert!(pid > 0);
        let mut text = String::new();
        for _ in 0..100 {
            text = capture_scrollback(&sock, session).unwrap();
            if text.contains("READY") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(text.contains("READY"), "{text}");
        assert_eq!(pane_exit_status(&sock, session).unwrap(), None, "still running");
        std::fs::write(&go, b"").unwrap();
        // remain-on-exit keeps the pane so the exit code is readable.
        let mut status = None;
        for _ in 0..100 {
            status = pane_exit_status(&sock, session).unwrap();
            if status.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(status, Some(7), "PTY-reported exit status");
        assert!(session_alive(&sock, session));
        assert!(!session_alive("flow-t-other-socket", session), "sessions are per socket");
        // A dead pane's last line scrolls into history behind tmux's "Pane is
        // dead" banner, so read the scrollback.
        let text = capture_scrollback(&sock, session).unwrap();
        assert!(text.contains("READY"), "the dead pane keeps its output: {text}");
        let (cols, rows) = pane_size(&sock, session).unwrap();
        assert!(cols > 0 && rows > 0);
        kill_session(&sock, session);
        assert!(!session_alive(&sock, session));
        kill_server(&sock);
    }
}
