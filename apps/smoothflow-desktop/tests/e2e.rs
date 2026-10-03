//! SmoothFlow Desktop end to end (th-032792): the app's own core against a
//! REAL `smooth-daemon`.
//!
//! Brent created two sessions in the desktop app and never got a terminal:
//! the daemon couldn't find tmux on a Finder-style PATH (th-9f6814). Nothing
//! covered app ⇄ daemon, so it was invisible. This test drives exactly what
//! the GUI runs — `discovery`, `net`, `frames`, the New Session sheet and its
//! HTTP reads, `app_core::Core` (attach/resize, keystrokes), `keys` and
//! `TerminalModel` — with only the GPUI drawing left out. It never opens a
//! window.
//!
//! **Isolation.** Each daemon gets a scratch `$HOME` (so `~/.smooth/flow.addr`,
//! `operator-token` and `flow.db` are throwaway), an ephemeral port, its own
//! tmux socket (`tmux -L sfd-e2e-<pid>-<n>`), no relay, no `tailscale serve`
//! and no single-instance lock. Discovery reads only that HOME — never the
//! developer's environment — so it cannot reach a real daemon.
//!
//! **The daemon binary.** `$SMOOTHFLOW_E2E_DAEMON` when set (CI builds it in
//! a step and sets this). Otherwise the test builds it from the main
//! workspace: `cargo build -p smooai-smooth-daemon --bin smooth-daemon
//! --manifest-path <repo>/Cargo.toml`, into `$CARGO_TARGET_DIR` when set
//! (see [`daemon_target_dir`]), else `<repo>/target`.
//!
//! **Prerequisites.** tmux. Without it the tests skip, unless
//! `SMOOTH_E2E_STRICT=1` (CI), which turns every skip into a failure.
//! Unix only: Windows has no native tmux (the engine runs in WSL2 there).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::channel::mpsc::UnboundedReceiver;
use smooth_flow_client::diff::{Base, LineKind};
use smooth_flow_client::gate::CenterTab;
use smooth_flow_client::keymap::{Action, Keymap, Platform};
use smooth_flow_client::pane::Rect;
use smooth_flow_client::SessionState;
use smoothflow_desktop::app_core::{Confirmed, Connection, Core};
use smoothflow_desktop::discovery;
use smoothflow_desktop::keys::Key;
use smoothflow_desktop::layout::CellMetrics;
use smoothflow_desktop::net::{self, Event};

/// The machine running this is shared with many agents; every wait polls.
const BOOT_TIMEOUT: Duration = Duration::from_secs(90);
const WAIT: Duration = Duration::from_secs(60);

/// What Finder (launchd) gives an app: no Homebrew, no `~/.cargo/bin`.
const FINDER_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// A cell size and a pane area whose grid is exactly 80×24 at it — what the
/// window computes from the font and its size.
const METRICS: CellMetrics = CellMetrics { width: 8.0, height: 16.0 };
const AREA: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 80.0 * 8.0 + 2.0 * smoothflow_desktop::layout::PANE_PADDING as f64,
    h: 24.0 * 16.0 + 2.0 * smoothflow_desktop::layout::PANE_PADDING as f64,
};

static SEQ: AtomicU32 = AtomicU32::new(0);

// ── prerequisites ───────────────────────────────────────────────────────

fn strict() -> bool {
    std::env::var("SMOOTH_E2E_STRICT").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Skip (or, strict, fail). Returns `false` so a test can `return`.
fn skip(why: &str) -> bool {
    assert!(!strict(), "SMOOTH_E2E_STRICT is set and a prerequisite is missing: {why}");
    eprintln!("[skip] {why}");
    false
}

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
        || skip("tmux is not installed")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

/// `$SMOOTHFLOW_E2E_DAEMON`, else build it from the main workspace.
fn daemon_bin() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(p) = std::env::var_os("SMOOTHFLOW_E2E_DAEMON").filter(|p| !p.is_empty()) {
            let p = PathBuf::from(p);
            assert!(p.is_file(), "SMOOTHFLOW_E2E_DAEMON={} is not a file", p.display());
            return p;
        }
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        eprintln!("building smooth-daemon from {} (set SMOOTHFLOW_E2E_DAEMON to skip this)", repo_root().display());
        let mut cmd = Command::new(cargo);
        if let Some(dir) = daemon_target_dir() {
            cmd.env("CARGO_TARGET_DIR", dir);
        }
        let out = cmd
            .args([
                "build",
                "-p",
                "smooai-smooth-daemon",
                "--bin",
                "smooth-daemon",
                "--message-format=json-render-diagnostics",
                "--manifest-path",
            ])
            .arg(repo_root().join("Cargo.toml"))
            .stderr(Stdio::inherit())
            .output()
            .expect("run cargo build for smooth-daemon");
        assert!(out.status.success(), "building smooth-daemon failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == "smooth-daemon")
            .find_map(|v| v["executable"].as_str().map(PathBuf::from))
            .expect("cargo reported no smooth-daemon executable")
    })
    .clone()
}

/// Where the fallback daemon build goes: `$CARGO_TARGET_DIR` when set, but
/// never the target dir this test is running from. The outer `cargo test`
/// can still hold that dir's build lock, and a nested build would wait for it
/// forever, so the daemon then builds into a `smoothflow-e2e-daemon`
/// subdirectory. `None` = cargo's default (`<repo>/target`).
fn daemon_target_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").filter(|d| !d.is_empty())?);
    let dir = if dir.is_absolute() { dir } else { std::env::current_dir().ok()?.join(dir) };
    let running_from_it = std::env::current_exe().is_ok_and(|exe| dir.canonicalize().is_ok_and(|d| exe.starts_with(d)));
    Some(if running_from_it { dir.join("smoothflow-e2e-daemon") } else { dir })
}

// ── the daemon ──────────────────────────────────────────────────────────

/// Which `PATH` the daemon runs with.
#[derive(Clone, Copy)]
enum DaemonPath {
    /// This test's own `PATH` (a terminal-launched daemon).
    Inherited,
    /// [`FINDER_PATH`]: how Big Smooth.app / a Finder-launched daemon runs.
    Finder,
}

struct Daemon {
    /// Held for its Drop: the scratch HOME, workspace and log go with it.
    _root: tempfile::TempDir,
    home: PathBuf,
    /// The daemon's workspace (`SMOOTH_WORKSPACE`): where New Session opens.
    ws: PathBuf,
    socket: String,
    child: Child,
    log_path: PathBuf,
}

impl Daemon {
    /// Boot with `extra` env on top of the isolation set.
    fn boot(path: DaemonPath, extra: &[(&str, &str)]) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = tempfile::Builder::new().prefix("sfd-e2e-").tempdir().expect("tempdir");
        let home = root.path().join("home");
        let ws = root.path().join("ws");
        std::fs::create_dir_all(home.join(".smooth")).expect("home");
        std::fs::create_dir_all(&ws).expect("workspace");
        let socket = format!("sfd-e2e-{}-{n}", std::process::id());
        let log_path = root.path().join("daemon.log");
        let log = std::fs::File::create(&log_path).expect("daemon log");
        let err = log.try_clone().expect("daemon log");
        let path = match path {
            DaemonPath::Inherited => std::env::var("PATH").unwrap_or_else(|_| FINDER_PATH.into()),
            DaemonPath::Finder => FINDER_PATH.into(),
        };
        let child = Command::new(daemon_bin())
            .args(["operator", "--addr", "127.0.0.1:0", "--tmux-socket", &socket])
            .env_clear()
            .env("PATH", path)
            .env("HOME", &home)
            .env("TMPDIR", root.path())
            .env("SMOOTH_ALLOW_SECOND_DAEMON", "1")
            .env("SMOOTH_RELAY", "0")
            .env("SMOOTH_TAILSCALE_SERVE", "0")
            .env("SMOOTH_FLOW_HARNESS_DOCTOR", "0")
            .env("SMOOTH_WORKSPACE", &ws)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("RUST_LOG", "info,smooth_flow=debug,smooth_daemon::flow_route=debug")
            .env("TERM", "xterm-256color")
            .envs(extra.iter().copied())
            .current_dir(&ws)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .expect("spawn smooth-daemon");
        let mut d = Self {
            _root: root,
            home,
            ws,
            socket,
            child,
            log_path,
        };
        d.wait_advertised();
        d
    }

    fn smooth_dir(&self) -> PathBuf {
        self.home.join(".smooth")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    /// Wait for the daemon to listen and advertise itself the way the app
    /// discovers it: `~/.smooth/flow.addr` (a fresh HOME's flow daemon claims
    /// it) and `~/.smooth/operator-token`.
    fn wait_advertised(&mut self) {
        let start = Instant::now();
        let flow_addr = self.smooth_dir().join("flow.addr");
        let token = self.smooth_dir().join("operator-token");
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                panic!("smooth-daemon exited during boot ({status}):\n{}", self.log());
            }
            let listening = listening_addr(&self.log());
            let advertised = std::fs::read_to_string(&flow_addr).ok().filter(|a| !a.trim().is_empty());
            let have_token = std::fs::read_to_string(&token).is_ok_and(|t| !t.trim().is_empty());
            if let (Some(addr), Some(adv), true) = (&listening, &advertised, have_token) {
                assert_eq!(adv.trim(), addr, "flow.addr names the daemon's listening address");
                return;
            }
            assert!(
                start.elapsed() < BOOT_TIMEOUT,
                "smooth-daemon did not listen + advertise flow.addr + write operator-token within {BOOT_TIMEOUT:?} \
                 (listening={listening:?} flow.addr={advertised:?} token={have_token}):\n{}",
                tail(&self.log(), 60)
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-TERM", &self.child.id().to_string()]).status();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new("tmux").args(["-L", &self.socket, "kill-server"]).output();
        if std::thread::panicking() {
            eprintln!("--- daemon log ({}) ---\n{}", self.log_path.display(), tail(&self.log(), 80));
        }
    }
}

/// The address on the daemon's `operator listening addr=…` log line.
fn listening_addr(log: &str) -> Option<String> {
    let line = strip_ansi(log.lines().find(|l| l.contains("operator listening"))?);
    let rest = line.split("addr=").nth(1)?;
    let addr: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
    (addr.contains(':') && !addr.ends_with(":0")).then_some(addr)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

// ── the app ─────────────────────────────────────────────────────────────

/// The app's core, connected the way `main` connects it, with every event
/// applied the way the window applies it and a "render" (layout → attach)
/// after each batch.
struct App {
    core: Core,
    events: UnboundedReceiver<Event>,
    saw_hello: bool,
}

impl App {
    fn connect(smooth_dir: PathBuf) -> Self {
        // Only the scratch HOME's files: never `$SMOOTH_FLOW_ADDR` & co.
        let (out, events) = net::start_with(move || discovery::discover_in(&smooth_dir, |_| None));
        Self {
            core: Core::new(out, Keymap::defaults(Platform::current()), String::new()),
            events,
            saw_hello: false,
        }
    }

    /// Apply what has arrived, then lay out like a frame render.
    fn pump(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            if matches!(ev, Event::Frame(smoothflow_desktop::frames::Inbound::Hello { .. })) {
                self.saw_hello = true;
            }
            self.core.apply(ev);
        }
        self.core.layout(AREA, METRICS);
    }

    /// Pump until `pred` holds; panics with `what`, the app's view of the
    /// fleet and the daemon log tail.
    fn wait(&mut self, d: &Daemon, what: &str, timeout: Duration, mut pred: impl FnMut(&Self) -> bool) {
        let start = Instant::now();
        loop {
            self.pump();
            if pred(self) {
                return;
            }
            if start.elapsed() > timeout {
                panic!(
                    "{what}: not within {timeout:?}\nconnection: {:?}\nnotice: {:?}\nsessions: {:#?}\nscreens:\n{}\ndaemon log tail:\n{}",
                    self.core.connection,
                    self.core.notice,
                    self.core.ordered(),
                    self.screens(),
                    tail(&d.log(), 60)
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The visible text of `id`'s terminal, one string per row.
    fn screen(&self, id: &str) -> Vec<String> {
        self.core
            .terminals
            .get(id)
            .map_or_else(Vec::new, |t| (0..t.size().1).map(|l| t.line_text(l)).collect())
    }

    fn screens(&self) -> String {
        self.core
            .terminals
            .keys()
            .map(|id| format!("[{id}]\n{}", self.screen(id).join("\n")))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Type `text` the way GPUI hands keystrokes to the window.
    fn type_text(&mut self, text: &str) {
        let mut buf = [0u8; 4];
        for ch in text.chars() {
            let s: &str = ch.encode_utf8(&mut buf);
            let key = if ch == ' ' { "space".to_string() } else { s.to_lowercase() };
            assert!(
                self.core
                    .key(Key {
                        key: &key,
                        key_char: Some(s),
                        shift: ch.is_ascii_uppercase(),
                        ..Key::default()
                    })
                    .is_empty(),
                "typing into a pane fetches nothing"
            );
        }
    }

    fn press(&mut self, key: &str) -> Vec<smoothflow_desktop::app_core::Fetch> {
        self.core.key(Key { key, ..Key::default() })
    }
}

fn nonce() -> u64 {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (u64::from(t.subsec_nanos()) ^ u64::from(std::process::id()) << 7) % 1_000_000
}

/// Discover the daemon from its scratch HOME, connect, and get `flow.hello`.
fn connect(d: &Daemon) -> App {
    let mut app = App::connect(d.smooth_dir());
    app.wait(d, "connect and receive flow.hello", WAIT, |a| {
        a.core.connection == Connection::Connected && a.saw_hello
    });
    assert!(app.core.endpoint.as_ref().is_some_and(|e| e.token.is_some()), "discovery found the token");
    assert_eq!(
        app.core.home,
        d.home.to_string_lossy(),
        "hello carries the daemon's home (titles abbreviate against it)"
    );
    app
}

/// New Session, exactly as the user does it: open the sheet (its HTTP reads
/// go to the real daemon), pick Shell with the arrow keys, press Enter. The
/// engine's `flow.session` for it opens in the focused (empty) pane. Returns
/// its id.
fn start_shell(app: &mut App, d: &Daemon) -> String {
    let fetches = app.core.act(Action::NewSession);
    assert!(!fetches.is_empty(), "the sheet loads inference and repos");
    for f in fetches {
        let loaded = f.run();
        app.core.loaded(loaded);
    }
    let sheet = app.core.sheet.as_ref().expect("the New Session sheet is open");
    assert_eq!(sheet.error, None, "the sheet's reads against the daemon succeeded");
    let rows = sheet.rows.len();
    assert!(sheet.rows.iter().any(|r| r.kind == "shell" && r.enabled), "Shell is offered: {:?}", sheet.rows);
    for _ in 0..rows {
        if app.core.sheet.as_ref().and_then(|s| s.selected()).is_some_and(|r| r.kind == "shell") {
            break;
        }
        assert!(app.press("down").is_empty());
    }
    assert_eq!(app.core.sheet.as_ref().and_then(|s| s.selected()).map(|r| r.kind.as_str()), Some("shell"));
    assert!(app.press("enter").is_empty());
    assert!(app.core.sheet.is_none(), "Start sent flow.new and closed the sheet");
    app.wait(d, "the new shell session to open in the focused pane", WAIT, |a| {
        a.core.surfaces.focused_session().is_some()
    });
    let id = app.core.surfaces.focused_session().map(str::to_string).expect("focused session");
    assert_eq!(app.core.sessions[&id].kind, "shell");
    id
}

/// The whole user path: discover → connect → hello → New Session (Shell) →
/// the session opens in the focused pane → attach at 80×24 → live output →
/// type a command → see its output in the `TerminalModel` → Kill.
fn shell_session_round_trip(path: DaemonPath) {
    if !have_tmux() {
        return;
    }
    let d = Daemon::boot(path, &[]);
    let mut app = connect(&d);
    let id = start_shell(&mut app, &d);
    // The next layout attaches it at that pane's grid.
    app.wait(&d, "the session to attach at 80x24", WAIT, |a| a.core.attached_size(&id) == Some((80, 24)));
    assert_eq!(app.core.pane_hint(app.core.surfaces.focused_pane()), None, "the pane shows the terminal");

    // A terminal, not a blank pane forever (the bug): the shell's prompt.
    app.wait(&d, "live output from the shell (a prompt)", WAIT, |a| {
        a.screen(&id).iter().any(|l| !l.trim().is_empty())
    });
    assert_eq!(app.core.terminals[&id].size(), (80, 24), "the TerminalModel is the attached size");

    // Type a command through the key encoder → flow.input. The shell does
    // the arithmetic, so the answer appears only if it really ran.
    let n = nonce();
    let want = format!("smoothflow-e2e-{}", n + 1);
    app.type_text(&format!("echo smoothflow-e2e-$(({n}+1))"));
    assert!(app.press("enter").is_empty());
    app.wait(&d, &format!("`{want}` on the terminal"), WAIT, |a| {
        a.screen(&id).iter().any(|l| l.trim() == want)
    });

    // Kill, through the same confirmation the window shows.
    assert!(app.core.act(Action::Kill).is_empty());
    let dialog = app.core.dialog.clone().expect("Kill asks first");
    let (_, kill) = dialog
        .buttons
        .into_iter()
        .find(|(_, c)| matches!(c, Confirmed::Kill { .. }))
        .expect("a Kill button");
    app.core.confirm(kill);
    app.wait(&d, "the killed session to end", WAIT, |a| {
        a.core
            .sessions
            .get(&id)
            .is_none_or(|s| matches!(s.state, SessionState::Dead | SessionState::Done))
    });
    app.wait(&d, "the ended session to be detached", WAIT, |a| a.core.attached_size(&id).is_none());
}

/// A daemon started from a terminal (it inherits this PATH, tmux on it).
#[test]
fn a_shell_session_gets_a_live_terminal() {
    shell_session_round_trip(DaemonPath::Inherited);
}

/// A daemon started the way Big Smooth.app / Finder starts it, with
/// `PATH=/usr/bin:/bin:/usr/sbin:/sbin` — no Homebrew. On macOS tmux is in
/// /opt/homebrew/bin; before #707 (th-9f6814) the engine looked for it on
/// PATH only, so the session sat in `starting` with no terminal — the bug
/// this suite exists for.
#[test]
fn a_shell_session_gets_a_live_terminal_from_a_finder_launched_daemon() {
    shell_session_round_trip(DaemonPath::Finder);
}

/// A launch that fails must reach the app as a `dead` session that says
/// why, never a `starting` row with a blank pane (th-9f6814). The daemon
/// gets a tmux override that doesn't exist, which is an error, not a
/// fallback.
#[test]
fn a_session_that_cannot_launch_shows_why() {
    if !have_tmux() {
        return;
    }
    const MISSING: &str = "/nonexistent/th-9f6814/tmux";
    let d = Daemon::boot(DaemonPath::Finder, &[("SMOOTH_TMUX_BIN", MISSING)]);
    let mut app = connect(&d);
    let id = start_shell(&mut app, &d);
    app.wait(&d, "the failed session to be dead with launch_failed", WAIT, |a| {
        a.core.sessions.get(&id).is_some_and(|s| s.state == SessionState::Dead) && a.core.attention.get(&id).is_some_and(|x| x.reason == "launch_failed")
    });
    let detail = app.core.attention[&id].detail.clone().unwrap_or_default();
    assert!(
        detail.contains("tmux not found") && detail.contains(MISSING),
        "the detail names the problem and where it looked: {detail}"
    );
    assert!(
        app.core.sessions.values().all(|s| s.state != SessionState::Starting),
        "nothing is left in `starting`: {:#?}",
        app.core.ordered()
    );
    assert_eq!(app.core.attached_size(&id), None, "a dead session is not attached");
    // What the window draws in the session's pane: the reason, not a blank terminal.
    let hint = app
        .core
        .pane_hint(app.core.surfaces.focused_pane())
        .expect("the pane shows text, not a terminal");
    assert!(hint.starts_with("shell is dead: ") && hint.contains(&detail), "{hint}");
}

/// Close Out a live shell, the way a middle-click on its fleet row does it
/// (`Core::close_out` with the row's id, th-f958f2): the confirmation says it
/// kills the session first, confirming sends `flow.close`, and the real
/// engine kills it and drops the row. A shell has no pearl and no worktree of
/// its own, so nothing else happens and nothing is refused.
#[test]
fn closing_out_a_shell_drops_it_from_the_fleet() {
    if !have_tmux() {
        return;
    }
    let d = Daemon::boot(DaemonPath::Inherited, &[]);
    let mut app = connect(&d);
    let id = start_shell(&mut app, &d);
    app.wait(&d, "the shell to attach", WAIT, |a| a.core.attached_size(&id).is_some());

    app.core.close_out(&id);
    let dialog = app.core.dialog.clone().expect("Close Out asks first");
    assert!(dialog.message.contains("kills it first"), "a live session says so: {}", dialog.message);
    let (label, close) = dialog
        .buttons
        .into_iter()
        .find(|(_, c)| matches!(c, Confirmed::CloseOut { .. }))
        .expect("a Close Out button");
    assert_eq!(label, "Kill and Close Out");
    assert!(
        matches!(
            &close,
            Confirmed::CloseOut {
                close_pearl: false,
                remove_worktree: false,
                ..
            }
        ),
        "a shell has no pearl or worktree of its own: {close:?}"
    );
    app.core.confirm(close);
    app.wait(&d, "the closed-out shell to leave the fleet", WAIT, |a| {
        !a.core.sessions.contains_key(&id) && !a.core.order.contains(&id)
    });
    assert!(app.core.dialog.is_none(), "no refusal: {:?}", app.core.dialog);
    assert_eq!(app.core.surfaces.focused_session(), None, "no pane still shows it");
    assert!(app.core.notice.as_deref().is_some_and(|n| n.starts_with("Closed out")), "{:?}", app.core.notice);
}

/// The wheel against a real session (th-1977a8). The engine streams a
/// `tmux attach` client, and tmux draws on the alternate screen, so by
/// Ghostty's rules (the Mac's too) the wheel is arrow keys for the program —
/// here the shell, which recalls its last command — and the viewport never
/// leaves the screen. History scrolling on the primary screen is covered by
/// `terminal.rs` and `app_core.rs` unit tests.
#[test]
fn the_wheel_over_a_tmux_session_is_arrow_keys() {
    if !have_tmux() {
        return;
    }
    let d = Daemon::boot(DaemonPath::Inherited, &[]);
    let mut app = connect(&d);
    let id = start_shell(&mut app, &d);
    app.wait(&d, "the shell to attach at 80x24", WAIT, |a| a.core.attached_size(&id) == Some((80, 24)));
    app.wait(&d, "a prompt", WAIT, |a| a.screen(&id).iter().any(|l| !l.trim().is_empty()));
    let n = nonce();
    let cmd = format!("echo sfd-{n}-marker");
    app.type_text(&cmd);
    assert!(app.press("enter").is_empty());
    let out = format!("sfd-{n}-marker");
    app.wait(&d, "the echo", WAIT, |a| a.screen(&id).iter().any(|l| l.trim() == out));
    assert!(app.core.terminals[&id].alt_screen(), "a tmux attach client draws on the alternate screen");

    let pane = app.core.surfaces.focused_pane();
    assert!(app.core.wheel(pane, 1.0, 0, 0));
    app.wait(&d, "the wheel's Up arrow to recall the command", WAIT, |a| {
        a.screen(&id).iter().filter(|l| l.contains(&cmd)).count() >= 2
    });
    assert!(app.core.terminals[&id].at_bottom(), "no history scrolled on the alternate screen");
}

/// Close Out a shell in a dirty linked worktree (th-1977a8): the real engine
/// refuses and touches nothing; the refusal offers Force close (Keep it is
/// the default), and Force close removes the worktree and drops the row.
#[test]
fn a_refused_close_out_offers_force_and_force_removes_the_worktree() {
    if !have_tmux() {
        return;
    }
    let d = Daemon::boot(DaemonPath::Inherited, &[]);
    let ws = scratch_repo(&d);
    let wt = ws.with_file_name("ws-feature");
    git(&ws, &["worktree", "add", "-q", "-b", "feature", wt.to_str().expect("utf-8 path")]);
    std::fs::write(wt.join("scratch.txt"), "uncommitted\n").expect("dirty the worktree");

    let mut app = connect(&d);
    let fetches = app
        .core
        .run_effects(vec![smoothflow_desktop::sheet::Effect::Start(smoothflow_desktop::frames::NewSession {
            kind: "shell".into(),
            worktree: Some(wt.to_string_lossy().into_owned()),
            ..Default::default()
        })]);
    assert!(fetches.is_empty());
    app.wait(&d, "the shell in the worktree to open", WAIT, |a| a.core.surfaces.focused_session().is_some());
    let id = app.core.surfaces.focused_session().map(str::to_string).expect("focused session");
    let s = app.core.sessions[&id].clone();
    assert!(smoothflow_desktop::app_core::has_own_worktree(&s), "a linked worktree is its own: {s:?}");

    app.core.close_out(&id);
    let ask = app.core.dialog.clone().expect("Close Out asks");
    let close = ask.buttons[0].1.clone();
    assert!(
        matches!(
            close,
            Confirmed::CloseOut {
                remove_worktree: true,
                force: false,
                ..
            }
        ),
        "it offers to remove the worktree, unforced: {close:?}"
    );
    app.core.confirm(close);
    app.wait(&d, "the engine's refusal", WAIT, |a| a.core.dialog.is_some());
    let refusal = app.core.dialog.clone().expect("refusal");
    assert!(
        refusal.message.contains("uncommitted changes"),
        "the engine's reason, verbatim: {}",
        refusal.message
    );
    assert_eq!(refusal.cancel, "Keep it");
    let (label, force) = refusal.buttons[0].clone();
    assert_eq!(label, "Force close");
    assert!(
        matches!(
            force,
            Confirmed::CloseOut {
                remove_worktree: true,
                force: true,
                ..
            }
        ),
        "{force:?}"
    );
    assert!(wt.join("scratch.txt").is_file(), "nothing was touched");
    assert!(app.core.sessions.contains_key(&id), "the row stays");

    app.core.confirm(force);
    app.wait(&d, "the forced close to drop the row", WAIT, |a| !a.core.sessions.contains_key(&id));
    assert!(app.core.dialog.is_none(), "no second refusal: {:?}", app.core.dialog);
    assert!(!wt.exists(), "force removed the worktree");
    assert!(app.core.notice.as_deref().is_some_and(|n| n.starts_with("Closed out")), "{:?}", app.core.notice);
}

// ── Diff (th-26f5b9) ────────────────────────────────────────────────────

/// `git` in `dir`, isolated from the developer's config; panics on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=sfd-e2e", "-c", "user.email=sfd-e2e@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `src/lib.rs`, twenty functions, with function `n` returning `ret`.
fn lib_rs(n: u32, ret: &str) -> String {
    (1..=20)
        .map(|i| {
            if i == n {
                format!("pub fn f{i}() -> u32 {{ {ret} }}\n")
            } else {
                format!("pub fn f{i}() -> u32 {{ {i} }}\n")
            }
        })
        .collect()
}

/// Make the daemon's workspace a repo on `main` with one commit.
fn scratch_repo(d: &Daemon) -> PathBuf {
    let ws = d.ws.clone();
    std::fs::create_dir_all(ws.join("src")).expect("src");
    std::fs::write(ws.join("src/lib.rs"), lib_rs(0, "")).expect("lib.rs");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "-A"]);
    git(&ws, &["commit", "-q", "-m", "init"]);
    ws
}

/// A shell in the scratch repo with its Diff tab open, and the first diff in.
fn shell_with_diff(d: &Daemon, ws: &Path) -> (App, String) {
    let mut app = connect(d);
    let id = start_shell(&mut app, d);
    let s = &app.core.sessions[&id];
    assert_eq!(
        Path::new(&s.worktree).canonicalize().ok(),
        ws.canonicalize().ok(),
        "the shell runs in the scratch repo: {s:?}"
    );
    assert_eq!(s.branch.as_deref(), Some("main"), "the row carries the branch, so Diff is enabled");
    assert!(app.core.act(Action::ViewDiff).is_empty());
    assert_eq!(app.core.center, CenterTab::Diff, "notice: {:?}", app.core.notice);
    assert_eq!(app.core.diff.base, Base::Uncommitted, "a shell opens on Uncommitted");
    app.wait(d, "the first diff", WAIT, |a| a.core.diff.diff.is_some() && !a.core.diff.loading());
    (app, id)
}

/// Refresh and wait for the diff to settle on `files` files.
fn refreshed(app: &mut App, d: &Daemon, files: usize) {
    app.core.diff.request();
    app.wait(d, &format!("a refreshed diff with {files} file(s)"), WAIT, |a| {
        !a.core.diff.loading() && a.core.diff.diff.as_ref().is_some_and(|x| x.files.len() == files)
    });
}

/// The Diff tab against a real engine: a shell in a git repo gets the
/// structured diff of its uncommitted work — files in tree order, hunks with
/// line numbers, word and syntax spans — and a hunk stages and unstages.
#[test]
fn the_diff_tab_shows_a_shells_uncommitted_work_and_stages_a_hunk() {
    if !have_tmux() {
        return;
    }
    let d = Daemon::boot(DaemonPath::Inherited, &[]);
    let ws = scratch_repo(&d);
    std::fs::write(ws.join("src/lib.rs"), lib_rs(5, "500")).expect("edit");
    std::fs::write(ws.join("notes.md"), "# notes\n").expect("new file");
    let (mut app, _id) = shell_with_diff(&d, &ws);
    // A fresh engine's first diff can come back without syntax spans: the
    // highlight budget's clock starts before the syntax set's cold load
    // (pearl th-35271b). The second one is highlighted.
    refreshed(&mut app, &d, 2);
    let diff = app.core.diff.diff.clone().expect("diff");
    assert_eq!(diff.base, Base::Uncommitted);
    assert_eq!(diff.legend.len(), 17, "the legend names every syntax kind");
    assert_eq!((diff.added, diff.deleted), (2, 1), "{diff:#?}");
    let paths: Vec<&str> = app.core.diff.tree.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(paths, ["src", "lib.rs", "notes.md"], "directories first");
    let lib = diff.files.iter().find(|f| f.path == "src/lib.rs").expect("src/lib.rs");
    assert_eq!(
        (lib.status.as_str(), lib.added, lib.deleted, lib.language.as_deref()),
        ("modified", 1, 1, Some("Rust"))
    );
    let h = &lib.hunks[0];
    let del = h.lines.iter().find(|l| l.kind == LineKind::Del).expect("a deleted line");
    let add = h.lines.iter().find(|l| l.kind == LineKind::Add).expect("an added line");
    assert_eq!((del.old, add.new), (Some(5), Some(5)));
    assert_eq!(add.text, "pub fn f5() -> u32 { 500 }");
    assert!(!add.words.is_empty(), "the changed word is marked: {add:?}");
    assert!(!add.syntax.is_empty(), "the engine highlighted the Rust: {add:?}");
    assert!(!h.staged);
    let notes = diff.files.iter().find(|f| f.path == "notes.md").expect("notes.md");
    assert_eq!(notes.status, "added");
    assert!(app.core.diff.rows.iter().any(|r| r.is_line()), "the body has lines to draw");

    // `n` lands on the first hunk (src/lib.rs, tree order); `s` stages it.
    app.core.diff.cursor = None;
    app.press("n");
    app.press("s");
    app.wait(&d, "the hunk to show as staged", WAIT, |a| {
        a.core.diff.status.as_deref() == Some("Staged a hunk of src/lib.rs.")
            && !a.core.diff.loading()
            && a.core
                .diff
                .diff
                .as_ref()
                .is_some_and(|x| x.files.iter().any(|f| f.path == "src/lib.rs" && f.hunks.first().is_some_and(|h| h.staged)))
    });
    // `s` on a staged hunk unstages it.
    app.core.diff.cursor = None;
    app.press("n");
    app.press("s");
    app.wait(&d, "the hunk to be unstaged", WAIT, |a| {
        a.core.diff.status.as_deref() == Some("Unstaged a hunk of src/lib.rs.")
            && !a.core.diff.loading()
            && a.core
                .diff
                .diff
                .as_ref()
                .is_some_and(|x| x.files.iter().any(|f| f.path == "src/lib.rs" && f.hunks.first().is_some_and(|h| !h.staged)))
    });
}

/// Revert, the way the user does it (`r`, then the confirmation): the real
/// engine restores the file. Then a hunk that changed under the viewer is
/// refused as `stale`: the refusal is shown verbatim, the file is left
/// alone (no retry, no force), and the diff refreshes.
#[test]
fn reverting_a_hunk_restores_the_file_and_a_stale_hunk_is_refused() {
    if !have_tmux() {
        return;
    }
    let d = Daemon::boot(DaemonPath::Inherited, &[]);
    let ws = scratch_repo(&d);
    let lib = ws.join("src/lib.rs");
    std::fs::write(&lib, lib_rs(5, "500")).expect("edit");
    let (mut app, _id) = shell_with_diff(&d, &ws);
    assert_eq!(app.core.diff.diff.as_ref().map(|x| x.files.len()), Some(1));

    app.core.diff.cursor = None;
    app.press("n");
    app.press("r");
    let dialog = app.core.dialog.clone().expect("Revert asks first");
    assert_eq!(dialog.cancel, "Cancel");
    assert!(dialog.title.contains("src/lib.rs"), "{}", dialog.title);
    let (_, revert) = dialog
        .buttons
        .into_iter()
        .find(|(_, c)| matches!(c, Confirmed::DiffRevert { .. }))
        .expect("a Revert button");
    app.core.confirm(revert);
    app.wait(&d, "the revert to land and the diff to empty", WAIT, |a| {
        a.core.diff.status.as_deref() == Some("Reverted a hunk of src/lib.rs.")
            && !a.core.diff.loading()
            && a.core.diff.diff.as_ref().is_some_and(|x| x.files.is_empty())
    });
    assert_eq!(std::fs::read_to_string(&lib).expect("lib.rs"), lib_rs(0, ""), "the worktree is back at HEAD");

    // The viewer holds a hunk; the file then changes under it (no
    // flow.diff.changed: the engine didn't do it).
    std::fs::write(&lib, lib_rs(5, "501")).expect("edit");
    refreshed(&mut app, &d, 1);
    std::fs::write(&lib, lib_rs(5, "777")).expect("edit under the viewer");
    app.core.diff.cursor = None;
    app.press("n");
    app.press("r");
    let dialog = app.core.dialog.clone().expect("Revert asks first");
    let (_, revert) = dialog
        .buttons
        .into_iter()
        .find(|(_, c)| matches!(c, Confirmed::DiffRevert { .. }))
        .expect("a Revert button");
    app.core.confirm(revert);
    app.wait(&d, "the stale refusal, shown verbatim, then a refresh", WAIT, |a| {
        a.core.diff.error
            && a.core.diff.status.as_deref().is_some_and(|s| s.starts_with("stale:"))
            && !a.core.diff.loading()
            && a.core
                .diff
                .diff
                .as_ref()
                .is_some_and(|x| x.files.iter().flat_map(|f| &f.hunks).flat_map(|h| &h.lines).any(|l| l.text.contains("777")))
    });
    assert_eq!(
        std::fs::read_to_string(&lib).expect("lib.rs"),
        lib_rs(5, "777"),
        "a refused revert touches nothing"
    );
    assert!(app.core.dialog.is_none());
}
