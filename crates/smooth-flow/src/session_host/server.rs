//! The `flow-host` process: one per session, the agent's parent and its
//! terminal (ADR-011; protocol in docs/Architecture/SmoothFlow-Session-Host.md).
//!
//! [`run`] is the whole process. `smooth-daemon flow-host --id <id>` calls it,
//! as does the test-support `smooth-flow-host` binary. It reads the
//! [`SpawnRequest`] from stdin, detaches into its own session, binds the
//! socket, starts the child on a fresh PTY, writes the record, prints the
//! ready line, and then serves until the child has exited and the daemon
//! releases it (or nobody connects for `linger_secs`).
//!
//! Threads:
//! - **pty-out** reads the PTY, feeds every chunk to the [`Vt`], writes the
//!   VT's replies (DA, DSR) back, assigns the next `seq` and queues an
//!   `output` for the connected daemon, all under the state lock, so a
//!   snapshot is always current through exactly the last assigned `seq`.
//! - **pty-in** is the only writer to the PTY. Input, pastes, keys and VT
//!   replies all go through its channel, so neither the reader nor a
//!   connection ever blocks on a child that isn't reading its input.
//! - **wait** `waitpid`s the child (the exact status, no wrapper), drains
//!   the PTY to EOF, records the exit in the record, then notifies.
//! - **accept** plus one reader and one writer thread per connection; the
//!   newest authenticated connection supersedes the previous one.
//! - the main thread ticks: deferred snapshots and linger.
//!
//! The writer-drop hazard (th-6d8f84, `pty.rs`): portable-pty's writer sends
//! `\n` + `VEOF` when dropped. Here the child is the agent itself, so a drop
//! while it runs would type an empty line and `^D` into it. pty-in never
//! drops the writer: it lives until the process exits (`process::exit` runs
//! no destructors), and the process only exits on its own once the child
//! has. If the host is killed, the kernel closes the master, which hangs the
//! child up (SIGHUP) without typing anything.

#[cfg(unix)]
pub use imp::run;

/// Not built yet on this platform (th-2b32a6): reports `ready:false`.
#[cfg(not(unix))]
#[must_use]
pub fn run(_id: &str) -> std::process::ExitCode {
    let line = serde_json::json!({"ready": false, "error": "the SmoothFlow session host is not implemented on this platform yet (th-2b32a6)"});
    println!("{line}");
    std::process::ExitCode::FAILURE
}

#[cfg(unix)]
mod imp {
    use std::collections::VecDeque;
    use std::io::{BufReader, Read, Write};
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, PoisonError};
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};
    use nix::sys::signal::{kill, killpg, Signal};
    use nix::sys::wait::{waitpid, WaitStatus};
    use nix::unistd::Pid;
    use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
    use smooth_flow_vt::{Fidelity, Vt};

    use super::super::protocol::{
        classify, code, constant_time_eq, negotiate, read_frame, write_frame, Classified, ClientMsg, ExitInfo, FrameError, HostMsg, KillSignal, Modes,
        SpawnRequest, CLIENT_TYPES, MAX_BODY, PROTOCOL, PROTOCOL_MIN, RECORD_V,
    };
    use super::super::record::{ensure_private_dir, read_record, record_path, socket_path, valid_id, valid_token, write_record, HostRecord};

    /// A connection must say `hello` within this.
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
    /// A daemon that doesn't read for this long is dropped (its stream is
    /// then mid-frame, so it can only be closed).
    const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
    /// The output queued for a daemon before it is dropped for `overrun`.
    const DEFAULT_QUEUE: usize = 8 * 1024 * 1024;
    /// After the child exits, how long to wait for the PTY's EOF (a
    /// background grandchild can hold the PTY open forever).
    const EOF_DRAIN: Duration = Duration::from_secs(2);
    /// A snapshot waits at most this long for the stream to reach a sequence
    /// boundary (see [`Host::snapshot`]).
    const SNAPSHOT_GROUND_WAIT: Duration = Duration::from_millis(500);
    const TICK: Duration = Duration::from_millis(50);
    /// `key{repeat}` is capped, so one message can't queue unbounded input.
    const MAX_KEY_REPEAT: u32 = 1000;
    /// The spawn request is small; refuse a runaway stdin.
    const MAX_STDIN: u64 = 1024 * 1024;

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now_rfc3339() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// The process entry point. Prints one ready line on stdout; on success it
    /// never returns.
    pub fn run(id: &str) -> ExitCode {
        let mut stdout = std::io::stdout();
        match setup(id) {
            Ok(started) => {
                let line = serde_json::json!({"ready": true, "socket": started.host.socket, "pid": std::process::id()});
                let _ = writeln!(stdout, "{line}");
                let _ = stdout.flush();
                detach_stdio();
                started.serve()
            }
            Err(e) => {
                let line = serde_json::json!({"ready": false, "error": format!("{e:#}")});
                let _ = writeln!(stdout, "{line}");
                let _ = stdout.flush();
                ExitCode::FAILURE
            }
        }
    }

    /// The host before its threads start.
    struct Started {
        host: Arc<Host>,
        listener: UnixListener,
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        pty_in: mpsc::Receiver<Vec<u8>>,
    }

    /// Removes what setup made if setup fails part-way.
    struct Undo(Vec<PathBuf>);

    impl Drop for Undo {
        fn drop(&mut self) {
            for p in &self.0 {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    fn setup(id: &str) -> Result<Started> {
        if !valid_id(id) {
            bail!("invalid session id {id:?} (want fs- and 8 lowercase hex)");
        }
        let mut raw = Vec::new();
        std::io::stdin()
            .take(MAX_STDIN + 1)
            .read_to_end(&mut raw)
            .context("read the spawn request from stdin")?;
        if raw.len() as u64 > MAX_STDIN {
            bail!("spawn request is over {MAX_STDIN} bytes");
        }
        let req: SpawnRequest = serde_json::from_slice(&raw).context("parse the spawn request")?;
        validate(&req)?;

        detach_session()?;
        std::env::set_current_dir("/").context("chdir /")?;
        ensure_private_dir(&req.dir)?;

        let rec_path = record_path(&req.dir, id);
        if let Ok(old) = read_record(&rec_path) {
            if old.host_alive() {
                bail!("a host for {id} is already running (pid {})", old.pid);
            }
        }
        let socket = socket_path(&req.dir, id)?;
        let listener = bind_private(&socket)?;
        let mut undo = Undo(vec![socket.clone(), rec_path.clone()]);

        let (master, child_pid) = spawn_child(&req)?;
        let reader = master.try_clone_reader().context("clone pty reader")?;
        let writer = master.take_writer().context("take pty writer")?;
        let vt = Vt::new(req.cols, req.rows, req.scrollback_rows).context("headless terminal")?;

        let me = std::process::id();
        let record = HostRecord {
            v: RECORD_V,
            protocol: PROTOCOL,
            id: id.to_string(),
            host_version: env!("CARGO_PKG_VERSION").to_string(),
            pid: me,
            pid_start: crate::proc::start_time(me),
            child_pid,
            socket: socket.clone(),
            token: req.token.clone(),
            owner: req.owner.clone(),
            cwd: req.cwd.clone(),
            argv: req.argv.clone(),
            created_at: now_rfc3339(),
            exit: None,
        };
        write_record(&req.dir, &record)?;
        undo.0.clear();

        let (tx, rx) = mpsc::channel();
        let host = Arc::new(Host {
            token: req.token.clone(),
            dir: req.dir.clone(),
            socket,
            rec_path,
            child_pid,
            linger: Duration::from_secs(req.linger_secs),
            max_queue: req.max_queue_bytes.unwrap_or(DEFAULT_QUEUE),
            master: Mutex::new(master),
            pty_in: tx,
            state: Mutex::new(State {
                vt,
                seq: req.seq_start,
                exit: None,
                reaped: false,
                eof: false,
                conn: None,
                idle_since: Instant::now(),
                pending: Vec::new(),
                record,
            }),
            eof_cv: Condvar::new(),
        });
        Ok(Started {
            host,
            listener,
            reader,
            writer,
            pty_in: rx,
        })
    }

    /// Start the child on a fresh PTY. The child becomes a session leader on
    /// it (portable-pty runs setsid + TIOCSCTTY), so its pid is also its
    /// process group: `kill` signals the whole tree with one killpg.
    fn spawn_child(req: &SpawnRequest) -> Result<(Box<dyn MasterPty + Send>, u32)> {
        let portable_pty::PtyPair { master, slave } = native_pty_system()
            .openpty(PtySize {
                rows: req.rows,
                cols: req.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;
        let mut cmd = CommandBuilder::new(&req.argv[0]);
        cmd.args(&req.argv[1..]);
        cmd.cwd(&req.cwd);
        cmd.env_clear();
        for (k, v) in &req.env {
            cmd.env(k, v);
        }
        if !has_utf8_locale(&req.env) {
            // Without it tmux turned TABs into `_` and Nerd glyphs went blank;
            // agents misjudge their terminal the same way.
            cmd.env("LANG", "en_US.UTF-8");
        }
        if !req.env.contains_key("TERM") {
            cmd.env("TERM", "xterm-256color");
        }
        let child = slave.spawn_command(cmd).with_context(|| format!("spawn {:?}", req.argv[0]))?;
        drop(slave);
        let child_pid = child.process_id().context("child has no pid")?;
        // Reaped by waitpid in the wait thread, not through this handle.
        drop(child);
        Ok((master, child_pid))
    }

    fn validate(req: &SpawnRequest) -> Result<()> {
        if !valid_token(&req.token) {
            bail!("token must be 64 lowercase hex");
        }
        if req.argv.first().is_none_or(String::is_empty) {
            bail!("argv is empty");
        }
        if req.cols == 0 || req.rows == 0 {
            bail!("size {}x{} is invalid", req.cols, req.rows);
        }
        if !req.dir.is_absolute() || !req.cwd.is_absolute() {
            bail!("dir and cwd must be absolute");
        }
        if req.owner.is_empty() {
            bail!("owner is empty");
        }
        Ok(())
    }

    /// Does the environment already pick a UTF-8 locale?
    fn has_utf8_locale(env: &std::collections::BTreeMap<String, String>) -> bool {
        ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|k| env.get(*k).filter(|v| !v.is_empty()))
            .is_some_and(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("utf-8") || v.contains("utf8")
            })
    }

    /// A new session and process group: a daemon crash, or a `^C` to the
    /// daemon's terminal, can't reach us.
    fn detach_session() -> Result<()> {
        if nix::unistd::setsid().is_ok() {
            return Ok(());
        }
        // Already a session leader (someone ran us that way): fine.
        let me = nix::unistd::getpid();
        if nix::unistd::getsid(None).ok() == Some(me) {
            return Ok(());
        }
        bail!("setsid failed: this process leads a process group; spawn the host without process_group(0)")
    }

    /// Bind `path` at mode 0600 with no window where it is wider.
    fn bind_private(path: &Path) -> Result<UnixListener> {
        match std::fs::symlink_metadata(path) {
            // A dead host's socket. Never remove anything that isn't a socket.
            Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path).with_context(|| format!("remove stale {}", path.display()))?,
            Ok(_) => bail!("{} exists and is not a socket", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("stat {}", path.display())),
        }
        // Single-threaded here, so the process-wide umask can't race.
        let old = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
        let bound = UnixListener::bind(path);
        nix::sys::stat::umask(old);
        let listener = bound.with_context(|| format!("bind {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).with_context(|| format!("chmod {}", path.display()))?;
        Ok(listener)
    }

    /// Point stdin, stdout and stderr at /dev/null once the daemon has the
    /// ready line, so the daemon's pipes can close.
    fn detach_stdio() {
        if let Ok(null) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/null") {
            use std::os::fd::AsRawFd as _;
            for fd in 0..=2 {
                let _ = nix::unistd::dup2(null.as_raw_fd(), fd);
            }
        }
    }

    /// The uid of the peer on `s`.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn peer_uid(s: &UnixStream) -> Option<u32> {
        nix::sys::socket::getsockopt(s, nix::sys::socket::sockopt::PeerCredentials)
            .ok()
            .map(|c| c.uid())
    }

    /// The uid of the peer on `s`.
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn peer_uid(s: &UnixStream) -> Option<u32> {
        nix::unistd::getpeereid(s).ok().map(|(uid, _)| uid.as_raw())
    }

    /// Only this user may talk to the host. An unknown peer is refused.
    pub(super) fn peer_allowed(peer: Option<u32>, me: u32) -> bool {
        peer == Some(me)
    }

    struct Host {
        token: String,
        dir: PathBuf,
        socket: PathBuf,
        rec_path: PathBuf,
        child_pid: u32,
        linger: Duration,
        max_queue: usize,
        master: Mutex<Box<dyn MasterPty + Send>>,
        /// To the pty-in thread, the PTY's only writer.
        pty_in: mpsc::Sender<Vec<u8>>,
        state: Mutex<State>,
        eof_cv: Condvar,
    }

    struct State {
        vt: Vt,
        /// The last assigned output `seq`.
        seq: u64,
        exit: Option<ExitInfo>,
        /// The child has been waited for (its pid may now be reused).
        reaped: bool,
        /// The PTY reader saw EOF.
        eof: bool,
        conn: Option<Arc<Conn>>,
        /// Since when no daemon has been connected.
        idle_since: Instant,
        /// Snapshots waiting for the stream to reach a sequence boundary.
        pending: Vec<PendingSnap>,
        record: HostRecord,
    }

    struct PendingSnap {
        conn: Arc<Conn>,
        req: u64,
        max: usize,
        since: Instant,
    }

    impl Started {
        fn serve(self) -> ! {
            let Self {
                host,
                listener,
                reader,
                writer,
                pty_in,
            } = self;
            spawn("flow-host-pty-in", move || pty_in_loop(writer, &pty_in));
            let h = host.clone();
            spawn("flow-host-pty-out", move || h.pty_out_loop(reader));
            let h = host.clone();
            spawn("flow-host-wait", move || h.wait_loop());
            let h = host.clone();
            spawn("flow-host-accept", move || {
                for s in listener.incoming().flatten() {
                    let h = h.clone();
                    spawn("flow-host-conn", move || h.serve_conn(&s));
                }
            });
            loop {
                std::thread::sleep(TICK);
                host.tick();
            }
        }
    }

    fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
        // A host that can't start a thread can't serve: better to die (and
        // leave a stale record a daemon cleans up) than to limp on.
        if std::thread::Builder::new().name(name.into()).spawn(f).is_err() {
            std::process::exit(70);
        }
    }

    /// The PTY's only writer. It never drops `writer`: see the module docs.
    fn pty_in_loop(mut writer: Box<dyn Write + Send>, rx: &mpsc::Receiver<Vec<u8>>) {
        while let Ok(bytes) = rx.recv() {
            // A failed write means the child is gone; later ones fail too.
            let _ = writer.write_all(&bytes).and_then(|()| writer.flush());
        }
        std::mem::forget(writer);
    }

    impl Host {
        fn state(&self) -> MutexGuard<'_, State> {
            lock(&self.state)
        }

        fn pty_out_loop(&self, mut reader: Box<dyn Read + Send>) {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    // EOF, or EIO once every slave fd is closed (Linux).
                    Ok(0) => break,
                    Ok(n) => self.on_output(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let mut st = self.state();
            st.eof = true;
            Self::answer_pending(&mut st, true);
            drop(st);
            self.eof_cv.notify_all();
        }

        /// Feed, answer the VT's queries, assign the `seq`, queue the output:
        /// in that order and under one lock.
        fn on_output(&self, bytes: &[u8]) {
            let mut st = self.state();
            st.vt.feed(bytes);
            let replies = st.vt.take_replies();
            if !replies.is_empty() {
                let _ = self.pty_in.send(replies);
            }
            st.seq += 1;
            let seq = st.seq;
            if let Some(c) = &st.conn {
                c.push_output(seq, bytes, self.max_queue);
            }
            if !st.pending.is_empty() {
                Self::answer_pending(&mut st, false);
            }
        }

        fn wait_loop(&self) {
            let pid = Pid::from_raw(i32::try_from(self.child_pid).unwrap_or(i32::MAX));
            let (code, signal) = loop {
                match waitpid(pid, None) {
                    Ok(WaitStatus::Exited(_, c)) => break (Some(c), None),
                    Ok(WaitStatus::Signaled(_, s, _)) => break (None, Some(s as i32)),
                    Ok(_) | Err(nix::errno::Errno::EINTR) => {}
                    // Nobody else reaps it, so this "can't happen"; report an
                    // unknown status rather than hang.
                    Err(_) => break (None, None),
                }
            };
            let st = self.state();
            let (mut st, _) = self
                .eof_cv
                .wait_timeout_while(st, EOF_DRAIN, |s| !s.eof)
                .unwrap_or_else(PoisonError::into_inner);
            st.reaped = true;
            let exit = ExitInfo {
                code,
                signal,
                at: now_rfc3339(),
            };
            st.exit = Some(exit.clone());
            st.record.exit = Some(exit);
            // The record carries the status BEFORE anyone is told, so a daemon
            // that is down right now still learns it.
            let _ = write_record(&self.dir, &st.record);
            st.idle_since = Instant::now();
            let seq = st.seq;
            if let Some(c) = &st.conn {
                c.push_control(&HostMsg::Exit { code, signal, seq }, &[]);
            }
            Self::answer_pending(&mut st, true);
        }

        /// Deferred snapshots and linger.
        fn tick(&self) {
            let mut st = self.state();
            if st.pending.iter().any(|p| p.since.elapsed() >= SNAPSHOT_GROUND_WAIT) {
                Self::answer_pending(&mut st, true);
            }
            if st.exit.is_some() && st.conn.is_none() && st.idle_since.elapsed() >= self.linger {
                self.finish(st);
            }
        }

        /// Delete the record and socket and exit, holding the state lock so
        /// nothing rewrites the record after it is gone.
        fn finish(&self, st: MutexGuard<'_, State>) -> ! {
            let _ = std::fs::remove_file(&self.rec_path);
            let _ = std::fs::remove_file(&self.socket);
            let _guard = st;
            std::process::exit(0)
        }

        fn serve_conn(self: &Arc<Self>, stream: &UnixStream) {
            let me = nix::unistd::geteuid().as_raw();
            if !peer_allowed(peer_uid(stream), me) {
                // Closed before reading a byte.
                return;
            }
            let Some(conn) = self.handshake(stream) else { return };
            let mut r = match stream.try_clone() {
                Ok(s) => BufReader::new(s),
                Err(_) => return,
            };
            loop {
                match read_frame(&mut r) {
                    Ok(Some(f)) => {
                        if !self.handle(&conn, f) {
                            break;
                        }
                    }
                    Ok(None) | Err(FrameError::Io(_)) => break,
                    Err(e) => {
                        conn.push_control(&HostMsg::error(None, e.code(), e.to_string()), &[]);
                        break;
                    }
                }
            }
            conn.close();
            let mut st = self.state();
            if st.conn.as_ref().is_some_and(|c| Arc::ptr_eq(c, &conn)) {
                st.conn = None;
                st.idle_since = Instant::now();
            }
        }

        /// `hello` in, `hello` (or an error and a close) out. On success this
        /// connection becomes the current one.
        fn handshake(self: &Arc<Self>, stream: &UnixStream) -> Option<Arc<Conn>> {
            let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
            let _ = stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
            let mut r = stream;
            let mut w = stream;
            let reject = |w: &mut &UnixStream, code: &str, message: String| {
                let _ = write_frame(w, &HostMsg::error(None, code, message), &[]);
                let _ = w.shutdown(std::net::Shutdown::Both);
            };
            let frame = match read_frame(&mut r) {
                Ok(Some(f)) => f,
                Ok(None) | Err(FrameError::Io(_)) => return None,
                Err(e) => {
                    reject(&mut w, e.code(), e.to_string());
                    return None;
                }
            };
            let Classified::Known(ClientMsg::Hello { protocol, token, .. }) = classify::<ClientMsg>(&frame, CLIENT_TYPES) else {
                reject(&mut w, code::BAD_REQUEST, "the first message must be a valid hello".into());
                return None;
            };
            if !constant_time_eq(token.as_bytes(), self.token.as_bytes()) {
                reject(&mut w, code::AUTH, "bad token".into());
                return None;
            }
            let Some(version) = negotiate(protocol, (PROTOCOL_MIN, PROTOCOL)) else {
                reject(&mut w, code::VERSION, format!("host speaks {PROTOCOL_MIN}..{PROTOCOL}"));
                return None;
            };
            let _ = stream.set_read_timeout(None);
            let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
            let conn = Arc::new(Conn::new(stream.try_clone().ok()?));
            let writer = conn.clone();
            let out = stream.try_clone().ok()?;
            spawn("flow-host-conn-out", move || writer.write_loop(out));

            let mut st = self.state();
            if let Some(old) = st.conn.replace(conn.clone()) {
                old.push_control(&HostMsg::error(None, code::SUPERSEDED, "another connection took over this host"), &[]);
                old.close();
            }
            let (cols, rows) = st.vt.size();
            conn.push_control(
                &HostMsg::Hello {
                    protocol: version,
                    host_version: st.record.host_version.clone(),
                    id: st.record.id.clone(),
                    pid: st.record.pid,
                    child_pid: self.child_pid,
                    cols,
                    rows,
                    seq: st.seq,
                    running: st.exit.is_none(),
                    exit: st.exit.clone(),
                },
                &[],
            );
            Some(conn)
        }

        /// One request. `false` closes the connection.
        fn handle(self: &Arc<Self>, conn: &Arc<Conn>, frame: super::super::protocol::Frame) -> bool {
            let req = frame.req();
            let msg = match classify::<ClientMsg>(&frame, CLIENT_TYPES) {
                Classified::Known(m) => m,
                Classified::Unknown(t) => {
                    conn.push_control(&HostMsg::error(req, code::UNKNOWN_TYPE, format!("unknown message type {t:?}")), &[]);
                    return true;
                }
                Classified::Bad(e) => {
                    conn.push_control(&HostMsg::error(req, code::BAD_REQUEST, e), &[]);
                    return true;
                }
            };
            match msg {
                ClientMsg::Hello { .. } => conn.push_control(&HostMsg::error(None, code::BAD_REQUEST, "already said hello"), &[]),
                ClientMsg::Input => {
                    if !frame.body.is_empty() {
                        let _ = self.pty_in.send(frame.body);
                    }
                }
                ClientMsg::Paste => {
                    let text = String::from_utf8_lossy(&frame.body);
                    let bytes = self.state().vt.encode_paste(&text);
                    let _ = self.pty_in.send(bytes);
                }
                ClientMsg::Key { name, repeat, req } => {
                    let encoded = self.state().vt.encode_key(&name);
                    match encoded {
                        Some(bytes) => {
                            for _ in 0..repeat.unwrap_or(1).clamp(1, MAX_KEY_REPEAT) {
                                let _ = self.pty_in.send(bytes.clone());
                            }
                            if let Some(req) = req {
                                conn.push_control(&HostMsg::Ok { req }, &[]);
                            }
                        }
                        None => conn.push_control(&HostMsg::error(req, code::UNKNOWN_KEY, format!("unknown key {name:?}")), &[]),
                    }
                }
                ClientMsg::Resize { req, cols, rows } => self.resize(conn, req, cols, rows),
                ClientMsg::Snapshot { req, max_bytes } => self.snapshot(conn, req, max_bytes),
                ClientMsg::Screen { req } => self.screen(conn, req),
                ClientMsg::Kill { req, signal, grace_ms } => {
                    self.kill(signal, Duration::from_millis(grace_ms.unwrap_or(3000)));
                    conn.push_control(&HostMsg::Ok { req }, &[]);
                }
                ClientMsg::Release => {
                    let st = self.state();
                    if st.exit.is_none() {
                        conn.push_control(&HostMsg::error(None, code::NOT_EXITED, "release is only valid after exit"), &[]);
                    } else {
                        self.finish(st);
                    }
                }
                ClientMsg::Ping { req } => conn.push_control(&HostMsg::Pong { req }, &[]),
            }
            true
        }

        fn resize(&self, conn: &Conn, req: u64, cols: u16, rows: u16) {
            if cols == 0 || rows == 0 {
                conn.push_control(&HostMsg::error(Some(req), code::BAD_REQUEST, format!("size {cols}x{rows} is invalid")), &[]);
                return;
            }
            let mut st = self.state();
            let changed = st.vt.size() != (cols, rows);
            if changed {
                if let Err(e) = st.vt.resize(cols, rows) {
                    conn.push_control(&HostMsg::error(Some(req), code::BAD_REQUEST, e.to_string()), &[]);
                    return;
                }
                // SIGWINCH reaches the child's foreground group.
                let _ = lock(&self.master).resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            let seq = st.seq;
            conn.push_control(&HostMsg::Resized { req, seq, changed }, &[]);
        }

        /// A snapshot taken while the last chunk ended mid-escape would let
        /// the next output start with the tail of that escape on a fresh
        /// terminal. So it waits for a chunk that ends at a sequence
        /// boundary, at most [`SNAPSHOT_GROUND_WAIT`] (a program that stops
        /// mid-sequence must not stall an attach).
        fn snapshot(&self, conn: &Arc<Conn>, req: u64, max_bytes: usize) {
            let mut st = self.state();
            let max = max_bytes.min(MAX_BODY);
            if st.vt.stream_is_ground() || st.eof || st.exit.is_some() {
                answer_snapshot(&mut st, conn, req, max);
            } else {
                st.pending.push(PendingSnap {
                    conn: conn.clone(),
                    req,
                    max,
                    since: Instant::now(),
                });
            }
        }

        fn answer_pending(st: &mut State, force: bool) {
            if !force && !st.vt.stream_is_ground() {
                return;
            }
            for p in std::mem::take(&mut st.pending) {
                answer_snapshot(st, &p.conn, p.req, p.max);
            }
        }

        fn screen(&self, conn: &Conn, req: u64) {
            let mut st = self.state();
            let seq = st.seq;
            let vt = &mut st.vt;
            let text = vt.plain_screen();
            let (cols, rows) = vt.size();
            let (cursor_x, cursor_y) = vt.cursor();
            let msg = HostMsg::Screen {
                req,
                seq,
                cols,
                rows,
                alternate_on: vt.alternate_on(),
                cursor_x,
                cursor_y,
                title: vt.title(),
                modes: modes(vt),
            };
            conn.push_control(&msg, text.as_bytes());
        }

        /// Signal the child's process group; escalate to SIGKILL after
        /// `grace` if the child is still there.
        fn kill(self: &Arc<Self>, signal: KillSignal, grace: Duration) {
            let pid = Pid::from_raw(i32::try_from(self.child_pid).unwrap_or(i32::MAX));
            let sig = match signal {
                KillSignal::Term => Signal::SIGTERM,
                KillSignal::Kill => Signal::SIGKILL,
            };
            {
                // Until the wait thread reaps the child its pid (and so its
                // group id) can't be reused, so signalling it is safe.
                let st = self.state();
                if st.reaped {
                    return;
                }
                let _ = killpg(pid, sig);
                let _ = kill(pid, sig);
            }
            if sig == Signal::SIGKILL {
                return;
            }
            let deadline = Instant::now() + grace;
            let me = self.clone();
            spawn("flow-host-kill", move || {
                while Instant::now() < deadline {
                    if me.state().reaped {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                let st = me.state();
                if !st.reaped {
                    let _ = killpg(pid, Signal::SIGKILL);
                    let _ = kill(pid, Signal::SIGKILL);
                }
            });
        }
    }

    fn answer_snapshot(st: &mut State, conn: &Conn, req: u64, max: usize) {
        let snap = st.vt.snapshot(max);
        let fidelity = match snap.screen {
            Fidelity::Full => "full",
            Fidelity::History { .. } => "history",
            Fidelity::Plain => "plain",
            Fidelity::Empty => "empty",
        };
        conn.push_control(
            &HostMsg::Snapshot {
                req,
                seq: st.seq,
                cols: snap.cols,
                rows: snap.rows,
                alternate: snap.primary.is_some(),
                fidelity: fidelity.into(),
            },
            &snap.bytes,
        );
    }

    fn modes(vt: &mut Vt) -> Modes {
        let on = |vt: &mut Vt, m: u16| vt.dec_mode(m) == Some(true);
        let mouse_tracking = [(1003, "any"), (1002, "button"), (1000, "normal"), (9, "x10")]
            .into_iter()
            .find(|(m, _)| on(vt, *m))
            .map(|(_, n)| n.to_string());
        let mouse_format = [(1016, "sgr_pixels"), (1006, "sgr"), (1015, "urxvt"), (1005, "utf8")]
            .into_iter()
            .find(|(m, _)| on(vt, *m))
            .map_or("x10", |(_, n)| n)
            .to_string();
        Modes {
            bracketed_paste: vt.bracketed_paste(),
            cursor_keys_app: vt.cursor_keys_application(),
            mouse_tracking,
            mouse_format,
            alt_scroll: on(vt, 1007),
            kitty_keyboard: vt.kitty_keyboard_flags(),
        }
    }

    /// What is queued for one daemon connection.
    #[derive(Default)]
    pub(super) struct Outbox {
        frames: VecDeque<Queued>,
        /// Bytes of `output` bodies queued (the overrun bound).
        output_bytes: usize,
        closed: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Kind {
        Output,
        Overrun,
        Control,
    }

    struct Queued {
        kind: Kind,
        body_len: usize,
        bytes: Vec<u8>,
    }

    impl Outbox {
        /// Queue an `output`, or, past `max` queued bytes, drop every queued
        /// output (and any earlier `overrun`, which this one supersedes) and
        /// queue `overrun{through_seq: seq}` instead. Answers are never
        /// dropped: the daemon asked for them.
        pub(super) fn push_output(&mut self, seq: u64, data: &[u8], max: usize) {
            if self.closed {
                return;
            }
            if self.output_bytes + data.len() > max {
                self.frames.retain(|q| q.kind == Kind::Control);
                self.output_bytes = 0;
                if let Ok(bytes) = super::super::protocol::encode(&HostMsg::Overrun { through_seq: seq }, &[]) {
                    self.frames.push_back(Queued {
                        kind: Kind::Overrun,
                        body_len: 0,
                        bytes,
                    });
                }
                return;
            }
            if let Ok(bytes) = super::super::protocol::encode(&HostMsg::Output { seq }, data) {
                self.output_bytes += data.len();
                self.frames.push_back(Queued {
                    kind: Kind::Output,
                    body_len: data.len(),
                    bytes,
                });
            }
        }

        pub(super) fn push_control(&mut self, msg: &HostMsg, body: &[u8]) {
            if self.closed {
                return;
            }
            let bytes = super::super::protocol::encode(msg, body)
                .or_else(|e| super::super::protocol::encode(&HostMsg::error(None, code::BAD_REQUEST, format!("answer did not fit a frame: {e}")), &[]));
            if let Ok(bytes) = bytes {
                self.frames.push_back(Queued {
                    kind: Kind::Control,
                    body_len: 0,
                    bytes,
                });
            }
        }

        fn pop(&mut self) -> Option<Vec<u8>> {
            let q = self.frames.pop_front()?;
            if q.kind == Kind::Output {
                self.output_bytes -= q.body_len;
            }
            Some(q.bytes)
        }

        #[cfg(test)]
        pub(super) fn kinds(&self) -> Vec<Kind> {
            self.frames.iter().map(|q| q.kind).collect()
        }
    }

    /// One daemon connection: its queue and its socket.
    struct Conn {
        outbox: Mutex<Outbox>,
        cv: Condvar,
        stream: UnixStream,
    }

    impl Conn {
        fn new(stream: UnixStream) -> Self {
            Self {
                outbox: Mutex::new(Outbox::default()),
                cv: Condvar::new(),
                stream,
            }
        }

        fn push_output(&self, seq: u64, data: &[u8], max: usize) {
            lock(&self.outbox).push_output(seq, data, max);
            self.cv.notify_one();
        }

        fn push_control(&self, msg: &HostMsg, body: &[u8]) {
            lock(&self.outbox).push_control(msg, body);
            self.cv.notify_one();
        }

        /// Stop taking frames; the writer flushes what is queued (a
        /// `superseded` error, say) and then closes the socket.
        fn close(&self) {
            lock(&self.outbox).closed = true;
            self.cv.notify_one();
            let _ = self.stream.shutdown(std::net::Shutdown::Read);
        }

        fn write_loop(&self, mut out: UnixStream) {
            loop {
                let next = {
                    let mut ob = lock(&self.outbox);
                    loop {
                        if let Some(b) = ob.pop() {
                            break Some(b);
                        }
                        if ob.closed {
                            break None;
                        }
                        ob = self.cv.wait(ob).unwrap_or_else(PoisonError::into_inner);
                    }
                };
                let Some(bytes) = next else { break };
                if out.write_all(&bytes).is_err() {
                    break;
                }
            }
            lock(&self.outbox).closed = true;
            let _ = out.shutdown(std::net::Shutdown::Both);
        }
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::imp::{peer_allowed, Kind, Outbox};
    use crate::session_host::protocol::{code, HostMsg};

    #[test]
    fn only_this_user_is_allowed() {
        assert!(peer_allowed(Some(501), 501));
        assert!(!peer_allowed(Some(0), 501), "not even root");
        assert!(!peer_allowed(Some(502), 501));
        assert!(!peer_allowed(None, 501), "an unknown peer is refused");
    }

    #[test]
    fn overrun_drops_queued_output_but_keeps_answers() {
        let mut ob = Outbox::default();
        ob.push_output(1, &[0; 40], 100);
        ob.push_control(&HostMsg::Pong { req: 1 }, &[]);
        ob.push_output(2, &[0; 40], 100);
        assert_eq!(ob.kinds(), [Kind::Output, Kind::Control, Kind::Output]);
        // 80 + 40 > 100: everything queued for output goes, an overrun stands in.
        ob.push_output(3, &[0; 40], 100);
        assert_eq!(ob.kinds(), [Kind::Control, Kind::Overrun]);
        // Output resumes after it…
        ob.push_output(4, &[0; 10], 100);
        assert_eq!(ob.kinds(), [Kind::Control, Kind::Overrun, Kind::Output]);
        // … and a second overrun replaces the first rather than piling up.
        ob.push_output(5, &[0; 200], 100);
        assert_eq!(ob.kinds(), [Kind::Control, Kind::Overrun]);
    }

    #[test]
    fn an_answer_too_big_for_a_frame_becomes_an_error() {
        let mut ob = Outbox::default();
        ob.push_control(&HostMsg::Ok { req: 1 }, &vec![0; crate::session_host::protocol::MAX_FRAME]);
        assert_eq!(ob.kinds(), [Kind::Control]);
        let _ = code::BAD_REQUEST;
    }
}
