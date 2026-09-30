//! The session host seam (th-64d4ab): everything the engine needs from
//! whatever actually runs a session's terminal.
//!
//! Today that is tmux ([`TmuxHost`]): one long-lived server per socket, a
//! session per flow session, `remain-on-exit` so a dead pane's status is
//! readable, and a `tmux attach` client on a `portable-pty` for streaming.
//! The engine used to call `crate::tmux` directly. It now holds an
//! `Arc<dyn SessionHost>` and never names tmux, so a future `PtyHost`
//! (portable-pty + ConPTY + libghostty-vt, sessions native on Windows) can
//! slot in without touching supervision, and tests can drive the engine
//! against an in-memory host.
//!
//! A session is addressed by [`SessionRef`]: the host namespace it was
//! created in (a tmux socket, th-d33afa) plus its name there (the flow
//! session id). A row records the socket it was launched on, so a daemon
//! restarted with a different default still finds its old panes.
//!
//! Process-level operations (liveness by pid + start time, tree kill) are
//! host operations too: the host launched the process, and routing them
//! here keeps an engine under test from ever signalling a real pid.

use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::pty::{OnOutput, PtyAttach};
use crate::{proc, tmux};

/// A session on a host: the namespace (tmux socket) and its name there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionRef {
    pub socket: String,
    pub name: String,
}

impl SessionRef {
    #[must_use]
    pub fn new(socket: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            socket: socket.into(),
            name: name.into(),
        }
    }
}

/// Terminal state a scrape rule may read (th-e77603): the OSC 0/2 title, the
/// alternate screen and the cursor row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaneMeta {
    pub title: String,
    pub alternate_on: bool,
    pub cursor_y: Option<usize>,
}

/// How a dead pane's process ended, as the host reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneDeath {
    /// Exited with this status.
    Code(i32),
    /// Killed by this signal. `0` when the host names one we don't know.
    /// tmux 3.3 prints a number, 3.5 a name (`term`, `kill`).
    Signal(i32),
    /// The pty is closed but the host has no status or signal for the
    /// process yet: it has not reaped it (th-7ff336). This is NOT an exit.
    /// Ask again.
    Unreaped,
}

/// What to run in a new session.
#[derive(Debug, Clone, Copy)]
pub struct Launch<'a> {
    /// Working directory of the session's process.
    pub cwd: &'a Path,
    /// The command; never empty.
    pub argv: &'a [String],
    /// Extra environment for the process (a manifest's `launch.env`, the
    /// flow id, the hook token file).
    pub env: &'a [(String, String)],
    /// `Some(prefix)`: run `argv` under the exit-code wrapper, which records
    /// the code at `<prefix>.<pid>.exit` (th-7ff336) — see
    /// [`SessionHost::recorded_exit`]. The returned pid is the wrapper's.
    pub exit_prefix: Option<&'a Path>,
}

/// A live attach stream: a client on the session whose output is being
/// streamed to `on_output`, and which takes keystrokes and resizes.
pub trait AttachStream: Send + Sync {
    /// Raw bytes (keystrokes) into the session.
    ///
    /// # Errors
    /// When the stream is closed.
    fn write(&self, data: &[u8]) -> Result<()>;

    /// Resize; the session follows.
    ///
    /// # Errors
    /// When the stream is closed.
    fn resize(&self, cols: u16, rows: u16) -> Result<()>;

    /// Whether the attach client has gone (its `on_output` saw EOF).
    fn is_closed(&self) -> bool;

    /// End the attach client. The session itself is untouched.
    fn close(&self);

    /// Flow clients currently counted onto this stream (engine bookkeeping,
    /// kept with the stream so a lookup and its count share one lock).
    fn clients(&self) -> &AtomicU64;
}

impl AttachStream for PtyAttach {
    fn write(&self, data: &[u8]) -> Result<()> {
        Self::write(self, data)
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        Self::resize(self, cols, rows)
    }

    fn is_closed(&self) -> bool {
        Self::is_closed(self)
    }

    fn close(&self) {
        Self::close(self);
    }

    fn clients(&self) -> &AtomicU64 {
        &self.clients
    }
}

/// Everything the flow engine asks of the thing that runs sessions.
///
/// Every session operation takes a [`SessionRef`]. Implementations must be
/// cheap to call from any thread; the engine calls them from supervision
/// ticks and request handlers alike.
pub trait SessionHost: Send + Sync {
    /// Whether this host can run sessions on this machine at all.
    fn available(&self) -> bool;

    /// The namespace NEW sessions are created in, and the identity a daemon
    /// supervises under (th-4f7866: a row is owned by the daemon whose
    /// default namespace created it).
    fn default_socket(&self) -> String;

    /// Start a detached session. Returns the session process's pid.
    ///
    /// # Errors
    /// When the host is missing, `argv` is empty or the session cannot be
    /// created.
    fn launch(&self, s: &SessionRef, spec: &Launch<'_>) -> Result<u32>;

    /// Is the session present (running, or dead but not yet cleaned up)?
    fn alive(&self, s: &SessionRef) -> bool;

    /// The session process's pid.
    ///
    /// # Errors
    /// When the session is gone.
    fn pid(&self, s: &SessionRef) -> Result<u32>;

    /// Title, alternate screen and cursor row.
    ///
    /// # Errors
    /// When the session is gone.
    fn meta(&self, s: &SessionRef) -> Result<PaneMeta>;

    /// `Some(death)` once the session's process is dead (the host keeps the
    /// session until the engine has read this), `None` while it runs.
    ///
    /// # Errors
    /// When the session is gone.
    fn exit_status(&self, s: &SessionRef) -> Result<Option<PaneDeath>>;

    /// The exit code the wrapper of the launch whose pid is `pid` recorded
    /// under `exit_prefix`, if it has.
    fn recorded_exit(&self, exit_prefix: &Path, pid: u32) -> Option<i32>;

    /// Forget every exit code recorded under `exit_prefix` (all launches).
    fn clear_recorded_exits(&self, exit_prefix: &Path);

    /// `(cols, rows)`.
    ///
    /// # Errors
    /// When the session is gone.
    fn size(&self, s: &SessionRef) -> Result<(u16, u16)>;

    /// Plain text of the visible screen.
    ///
    /// # Errors
    /// When the session is gone.
    fn capture_visible(&self, s: &SessionRef) -> Result<String>;

    /// Plain text including scrollback.
    ///
    /// # Errors
    /// When the session is gone.
    fn capture_scrollback(&self, s: &SessionRef) -> Result<String>;

    /// Bracketed-paste `text` without submitting it.
    ///
    /// # Errors
    /// When the session is gone.
    fn paste(&self, s: &SessionRef, text: &str) -> Result<()>;

    /// Bracketed-paste `text`, then Enter.
    ///
    /// # Errors
    /// When the session is gone.
    fn send_text(&self, s: &SessionRef, text: &str) -> Result<()>;

    /// A named key (`Enter`, `Escape`, `C-c`, `1`).
    ///
    /// # Errors
    /// When the session is gone.
    fn send_key(&self, s: &SessionRef, key: &str) -> Result<()>;

    /// Remove the session (best effort; never the whole host).
    fn kill_session(&self, s: &SessionRef);

    /// Tear down the whole namespace `socket` (tests and scratch engines).
    fn kill_server(&self, socket: &str);

    /// Attach a streaming client of `cols`×`rows` whose output goes to
    /// `on_output` (`(seq, bytes)`, an empty chunk exactly once at EOF).
    ///
    /// # Errors
    /// When the client cannot be started.
    fn attach(&self, s: &SessionRef, cols: u16, rows: u16, on_output: OnOutput) -> Result<Arc<dyn AttachStream>>;

    /// The start time of `pid`, `None` when it is gone.
    fn process_start(&self, pid: u32) -> Option<i64>;

    /// `pid` is alive AND started when recorded (`None`: a bare pid check).
    fn process_alive(&self, pid: u32, recorded_start: Option<i64>) -> bool;

    /// End `pid` and its descendants: ask, wait up to `grace`, then force.
    fn kill_process_tree(&self, pid: u32, grace: Duration);
}

impl std::fmt::Debug for dyn SessionHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionHost")
    }
}

/// The tmux host: every call delegates to [`crate::tmux`] / [`crate::pty`] /
/// [`crate::proc`] unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct TmuxHost;

/// The default host: [`TmuxHost`].
#[must_use]
pub fn default_host() -> Arc<dyn SessionHost> {
    Arc::new(TmuxHost)
}

impl SessionHost for TmuxHost {
    fn available(&self) -> bool {
        tmux::tmux_available()
    }

    fn default_socket(&self) -> String {
        tmux::socket_name()
    }

    fn launch(&self, s: &SessionRef, spec: &Launch<'_>) -> Result<u32> {
        tmux::launch_with(&s.socket, &s.name, spec.cwd, spec.argv, spec.env, spec.exit_prefix)
    }

    fn alive(&self, s: &SessionRef) -> bool {
        tmux::session_alive(&s.socket, &s.name)
    }

    fn pid(&self, s: &SessionRef) -> Result<u32> {
        tmux::pane_pid(&s.socket, &s.name)
    }

    fn meta(&self, s: &SessionRef) -> Result<PaneMeta> {
        tmux::pane_meta(&s.socket, &s.name)
    }

    fn exit_status(&self, s: &SessionRef) -> Result<Option<PaneDeath>> {
        tmux::pane_exit_status(&s.socket, &s.name)
    }

    fn recorded_exit(&self, exit_prefix: &Path, pid: u32) -> Option<i32> {
        tmux::read_exit_file(&tmux::exit_file(exit_prefix, pid))
    }

    fn clear_recorded_exits(&self, exit_prefix: &Path) {
        clear_exit_files(exit_prefix);
    }

    fn size(&self, s: &SessionRef) -> Result<(u16, u16)> {
        tmux::pane_size(&s.socket, &s.name)
    }

    fn capture_visible(&self, s: &SessionRef) -> Result<String> {
        tmux::capture_visible(&s.socket, &s.name)
    }

    fn capture_scrollback(&self, s: &SessionRef) -> Result<String> {
        tmux::capture_scrollback(&s.socket, &s.name)
    }

    fn paste(&self, s: &SessionRef, text: &str) -> Result<()> {
        tmux::paste_text(&s.socket, &s.name, text)
    }

    fn send_text(&self, s: &SessionRef, text: &str) -> Result<()> {
        tmux::send_text(&s.socket, &s.name, text)
    }

    fn send_key(&self, s: &SessionRef, key: &str) -> Result<()> {
        tmux::send_key(&s.socket, &s.name, key)
    }

    fn kill_session(&self, s: &SessionRef) {
        tmux::kill_session(&s.socket, &s.name);
    }

    fn kill_server(&self, socket: &str) {
        tmux::kill_server(socket);
    }

    fn attach(&self, s: &SessionRef, cols: u16, rows: u16, on_output: OnOutput) -> Result<Arc<dyn AttachStream>> {
        let pty: Arc<dyn AttachStream> = PtyAttach::spawn(&tmux::attach_argv(&s.socket, &s.name), cols, rows, on_output)?;
        Ok(pty)
    }

    fn process_start(&self, pid: u32) -> Option<i64> {
        proc::start_time(pid)
    }

    fn process_alive(&self, pid: u32, recorded_start: Option<i64>) -> bool {
        proc::is_alive(pid, recorded_start)
    }

    fn kill_process_tree(&self, pid: u32, grace: Duration) {
        proc::kill_tree(pid, grace);
    }
}

/// Remove every exit-code file (and half-written temp) recorded under
/// `exit_prefix` — `<dir>/<id>.<pid>.exit[.tmp]` for any pid.
fn clear_exit_files(exit_prefix: &Path) {
    let (Some(dir), Some(id)) = (exit_prefix.parent(), exit_prefix.file_name()) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let prefix = format!("{}.", id.to_string_lossy());
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && (name.ends_with(".exit") || name.ends_with(".exit.tmp")) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// An in-memory host for engine tests: sessions are records, nothing runs,
/// and every operation is logged to [`FakeHost::calls`]. Tests move a
/// session's process through its life with [`FakeHost::die`],
/// [`FakeHost::vanish`] and [`FakeHost::record_exit`].
///
/// Public behind the `test-util` feature (th-8b3918) so a host crate's tests
/// (smooth-daemon's in-process flow tools) can drive a real engine without
/// tmux or a single real process.
#[cfg(any(test, feature = "test-util"))]
pub mod fake {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    use anyhow::{anyhow, bail};

    use super::{Arc, AtomicU64, AttachStream, Duration, Launch, OnOutput, PaneDeath, PaneMeta, Path, Result, SessionHost, SessionRef};

    /// Pids the fake hands out start here: far above any real pid_max, and
    /// never signalled anyway — process calls stay in memory.
    const FIRST_PID: u32 = 900_000_000;

    #[derive(Debug, Clone)]
    pub struct FakeSession {
        pub pid: u32,
        pub cwd: PathBuf,
        pub argv: Vec<String>,
        pub env: Vec<(String, String)>,
        pub exit_prefix: Option<PathBuf>,
        pub death: Option<PaneDeath>,
        pub screen: String,
        pub meta: PaneMeta,
        pub size: (u16, u16),
        /// Everything typed in: `paste:…`, `text:…`, `key:…`.
        pub input: Vec<String>,
    }

    #[derive(Default)]
    struct State {
        sessions: HashMap<SessionRef, FakeSession>,
        /// `(exit_prefix, pid)` → the code a wrapper recorded.
        recorded: HashMap<(PathBuf, u32), i32>,
        next_pid: u32,
        calls: Vec<String>,
    }

    /// The fake's one namespace.
    pub const FAKE_SOCKET: &str = "fake-host";

    #[derive(Default)]
    pub struct FakeHost {
        state: Mutex<State>,
    }

    impl FakeHost {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn st(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn log(&self, call: String) {
            self.st().calls.push(call);
        }

        /// Every call so far, in order (`launch fs-…`, `kill_session fs-…`).
        pub fn calls(&self) -> Vec<String> {
            self.st().calls.clone()
        }

        /// The session record, when the session exists.
        pub fn session(&self, s: &SessionRef) -> Option<FakeSession> {
            self.st().sessions.get(s).cloned()
        }

        /// The session's process ends this way; the session stays (remain-on-exit).
        pub fn die(&self, s: &SessionRef, death: PaneDeath) {
            if let Some(f) = self.st().sessions.get_mut(s) {
                f.death = Some(death);
            }
        }

        /// The session disappears outright (the host lost it).
        pub fn vanish(&self, s: &SessionRef) {
            self.st().sessions.remove(s);
        }

        /// The exit-code wrapper of launch `pid` recorded `code`.
        pub fn record_exit(&self, exit_prefix: &Path, pid: u32, code: i32) {
            self.st().recorded.insert((exit_prefix.to_path_buf(), pid), code);
        }

        /// What the visible screen reads.
        pub fn set_screen(&self, s: &SessionRef, text: &str) {
            if let Some(f) = self.st().sessions.get_mut(s) {
                f.screen = text.to_string();
            }
        }

        fn with<T>(&self, s: &SessionRef, f: impl FnOnce(&mut FakeSession) -> T) -> Result<T> {
            self.st().sessions.get_mut(s).map(f).ok_or_else(|| anyhow!("fake host: no session {}", s.name))
        }
    }

    impl SessionHost for FakeHost {
        fn available(&self) -> bool {
            true
        }

        fn default_socket(&self) -> String {
            FAKE_SOCKET.to_string()
        }

        fn launch(&self, s: &SessionRef, spec: &Launch<'_>) -> Result<u32> {
            if spec.argv.is_empty() {
                bail!("cannot launch an empty argv");
            }
            self.log(format!("launch {} {}", s.name, spec.argv.join(" ")));
            let mut st = self.st();
            if st.sessions.contains_key(s) {
                bail!("fake host: duplicate session {}", s.name);
            }
            let pid = FIRST_PID + st.next_pid;
            st.next_pid += 1;
            st.sessions.insert(
                s.clone(),
                FakeSession {
                    pid,
                    cwd: spec.cwd.to_path_buf(),
                    argv: spec.argv.to_vec(),
                    env: spec.env.to_vec(),
                    exit_prefix: spec.exit_prefix.map(Path::to_path_buf),
                    death: None,
                    screen: String::new(),
                    meta: PaneMeta::default(),
                    size: (120, 40),
                    input: Vec::new(),
                },
            );
            Ok(pid)
        }

        fn alive(&self, s: &SessionRef) -> bool {
            self.st().sessions.contains_key(s)
        }

        fn pid(&self, s: &SessionRef) -> Result<u32> {
            self.with(s, |f| f.pid)
        }

        fn meta(&self, s: &SessionRef) -> Result<PaneMeta> {
            self.with(s, |f| f.meta.clone())
        }

        fn exit_status(&self, s: &SessionRef) -> Result<Option<PaneDeath>> {
            self.with(s, |f| f.death)
        }

        fn recorded_exit(&self, exit_prefix: &Path, pid: u32) -> Option<i32> {
            self.st().recorded.get(&(exit_prefix.to_path_buf(), pid)).copied()
        }

        fn clear_recorded_exits(&self, exit_prefix: &Path) {
            self.st().recorded.retain(|(p, _), _| p != exit_prefix);
        }

        fn size(&self, s: &SessionRef) -> Result<(u16, u16)> {
            self.with(s, |f| f.size)
        }

        fn capture_visible(&self, s: &SessionRef) -> Result<String> {
            self.with(s, |f| f.screen.clone())
        }

        fn capture_scrollback(&self, s: &SessionRef) -> Result<String> {
            self.with(s, |f| f.screen.clone())
        }

        fn paste(&self, s: &SessionRef, text: &str) -> Result<()> {
            self.with(s, |f| f.input.push(format!("paste:{text}")))
        }

        fn send_text(&self, s: &SessionRef, text: &str) -> Result<()> {
            self.with(s, |f| f.input.push(format!("text:{text}")))
        }

        fn send_key(&self, s: &SessionRef, key: &str) -> Result<()> {
            self.with(s, |f| f.input.push(format!("key:{key}")))
        }

        fn kill_session(&self, s: &SessionRef) {
            self.log(format!("kill_session {}", s.name));
            self.st().sessions.remove(s);
        }

        fn kill_server(&self, socket: &str) {
            self.log(format!("kill_server {socket}"));
            self.st().sessions.retain(|r, _| r.socket != socket);
        }

        fn attach(&self, s: &SessionRef, cols: u16, rows: u16, _on_output: OnOutput) -> Result<Arc<dyn AttachStream>> {
            self.with(s, |f| f.size = (cols, rows))?;
            self.log(format!("attach {} {cols}x{rows}", s.name));
            Ok(Arc::new(FakeAttach::default()))
        }

        fn process_start(&self, pid: u32) -> Option<i64> {
            self.process_alive(pid, None).then_some(i64::from(pid))
        }

        fn process_alive(&self, pid: u32, _recorded_start: Option<i64>) -> bool {
            self.st().sessions.values().any(|f| f.pid == pid && f.death.is_none())
        }

        fn kill_process_tree(&self, pid: u32, _grace: Duration) {
            self.log(format!("kill_process_tree {pid}"));
            // Like SIGTERM to the wrapper's group: the pane goes dead.
            for f in self.st().sessions.values_mut().filter(|f| f.pid == pid) {
                f.death = Some(PaneDeath::Signal(15));
            }
        }
    }

    /// An attach stream that records what was typed and its size.
    #[derive(Default)]
    pub struct FakeAttach {
        pub written: Mutex<Vec<u8>>,
        pub size: Mutex<(u16, u16)>,
        closed: AtomicBool,
        clients: AtomicU64,
    }

    impl AttachStream for FakeAttach {
        fn write(&self, data: &[u8]) -> Result<()> {
            if self.closed.load(Ordering::Acquire) {
                bail!("fake attach closed");
            }
            self.written.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend_from_slice(data);
            Ok(())
        }

        fn resize(&self, cols: u16, rows: u16) -> Result<()> {
            *self.size.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (cols, rows);
            Ok(())
        }

        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Acquire)
        }

        fn close(&self) {
            self.closed.store(true, Ordering::Release);
        }

        fn clients(&self) -> &AtomicU64 {
            &self.clients
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn clearing_exit_files_removes_only_that_sessions_records() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["fs-a.1.exit", "fs-a.2.exit.tmp", "fs-ab.3.exit", "fs-b.4.exit", "fs-a.note"] {
            std::fs::write(dir.path().join(name), "0").unwrap();
        }
        TmuxHost.clear_recorded_exits(&dir.path().join("fs-a"));
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["fs-a.note", "fs-ab.3.exit", "fs-b.4.exit"]);
        // A missing directory is not an error.
        TmuxHost.clear_recorded_exits(&dir.path().join("nowhere/fs-a"));
    }

    #[test]
    fn tmux_host_reads_the_wrappers_exit_file() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("fs-x");
        assert_eq!(TmuxHost.recorded_exit(&prefix, 42), None);
        std::fs::write(tmux::exit_file(&prefix, 42), "7\n").unwrap();
        assert_eq!(TmuxHost.recorded_exit(&prefix, 42), Some(7));
        assert_eq!(TmuxHost.recorded_exit(&prefix, 43), None, "another launch's pid never answers");
    }

    #[test]
    fn tmux_host_default_socket_is_the_flow_socket() {
        let _g = tmux::tests_env_lock();
        std::env::remove_var("SMOOTH_FLOW_TMUX_SOCKET");
        assert_eq!(TmuxHost.default_socket(), tmux::FLOW_SOCKET);
        assert_eq!(format!("{:?}", default_host()), "SessionHost");
    }
}
