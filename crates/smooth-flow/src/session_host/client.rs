//! The daemon's side of a `flow-host`: spawn one, connect, and drive it.
//!
//! [`spawn_host`] starts `smooth-daemon flow-host --id <id>` with the
//! [`SpawnRequest`] on stdin and waits for its ready line. [`HostClient`]
//! connects (to a host it just spawned, or one [`super::adopt`] found),
//! completes the versioned `hello`, and then:
//! - streams `output` / `overrun` / `exit` (and unsolicited `error`s) to an
//!   [`OnEvent`] callback from its reader thread;
//! - turns each request into a blocking call matched on `req`.
//!
//! Blocking std I/O and threads, like `pty.rs`: the engine's `SessionHost`
//! seam is synchronous and called from any thread.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::protocol::{
    classify, code, read_frame, write_frame, Classified, ClientMsg, ExitInfo, HostMsg, KillSignal, Modes, Ready, SpawnRequest, HOST_TYPES, PROTOCOL,
    PROTOCOL_MIN,
};

/// How long [`spawn_host`] waits for the ready line, per the spec.
pub const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the handshake may take.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a request waits for its answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// A host that doesn't read our frames for this long is treated as gone.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The versions this build offers in `hello`.
pub const OFFER: (u32, u32) = (PROTOCOL_MIN, PROTOCOL);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The program and leading arguments that start a host; `--id <id>` follows.
#[derive(Debug, Clone)]
pub struct HostCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl HostCommand {
    /// `<daemon> flow-host`, the shipped form.
    #[must_use]
    pub fn daemon(exe: impl Into<PathBuf>) -> Self {
        Self {
            program: exe.into(),
            args: vec!["flow-host".into()],
        }
    }
}

/// A host that reported ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spawned {
    pub socket: PathBuf,
    pub pid: u32,
}

/// Start a host for session `id` and wait (up to `timeout`) for its ready
/// line. The host is our child until we exit (setsid doesn't change its
/// parent), so a thread reaps it whenever it ends.
///
/// # Errors
/// When it can't be started, reports `ready:false` (the error is its
/// message, for the row's `launch_failed` detail), or says nothing in time.
pub fn spawn_host(cmd: &HostCommand, id: &str, req: &SpawnRequest, timeout: Duration) -> Result<Spawned> {
    let mut child = Command::new(&cmd.program)
        .args(&cmd.args)
        .arg("--id")
        .arg(id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {}", cmd.program.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A host that died early closes stdin; its silence is reported below.
        let _ = serde_json::to_writer(&mut stdin, req);
        let _ = stdin.flush();
    }
    let stdout = child.stdout.take().context("flow-host stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("flow-host-ready".into())
        .spawn(move || {
            let mut line = String::new();
            let r = BufReader::new(stdout.take(64 * 1024)).read_line(&mut line);
            let _ = tx.send(r.map(|_| line));
        })
        .context("spawn ready reader")?;
    let line = match rx.recv_timeout(timeout) {
        Ok(Ok(line)) => line,
        Ok(Err(e)) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e).context("read flow-host's ready line");
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("flow-host did not report ready within {timeout:?}");
        }
    };
    let ready: Option<Ready> = serde_json::from_str(line.trim()).ok();
    let Some(ready) = ready else {
        let _ = child.kill();
        let _ = child.wait();
        bail!("flow-host exited without reporting ready (said {:?})", line.trim());
    };
    if !ready.ready {
        let _ = child.wait();
        bail!("{}", ready.error.unwrap_or_else(|| "flow-host failed to start".into()));
    }
    std::thread::Builder::new()
        .name("flow-host-reap".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .context("spawn reaper")?;
    match (ready.socket, ready.pid) {
        (Some(socket), Some(pid)) => Ok(Spawned { socket, pid }),
        _ => bail!("flow-host's ready line has no socket or pid"),
    }
}

/// What the host sends unasked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// PTY bytes.
    Output { seq: u64, data: Vec<u8> },
    /// Output through `through_seq` was dropped: take a snapshot.
    Overrun { through_seq: u64 },
    /// The child exited.
    Exit { code: Option<i32>, signal: Option<i32>, seq: u64 },
    /// An error not tied to a request (`superseded`, a key without `req`).
    Error { code: String, message: String },
    /// The connection ended (the host may well still be running).
    Closed,
}

/// The event sink; called from the client's reader thread, in order.
pub type OnEvent = Arc<dyn Fn(HostEvent) + Send + Sync>;

/// Why talking to a host failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connect: {0}")]
    Connect(std::io::Error),
    /// The token was refused.
    #[error("authentication refused: {0}")]
    Auth(String),
    /// No protocol version in common: leave the host running (`held`).
    #[error("no common protocol version: {0}")]
    Version(String),
    #[error("protocol violation: {0}")]
    Protocol(String),
    /// The host answered a request with `error`.
    #[error("host error {code}: {message}")]
    Host { code: String, message: String },
    #[error("timed out waiting for the host")]
    Timeout,
    #[error("connection to the host is closed")]
    Closed,
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// The host's `hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostHello {
    pub protocol: u32,
    pub host_version: String,
    pub id: String,
    pub pid: u32,
    pub child_pid: u32,
    pub cols: u16,
    pub rows: u16,
    /// The last output `seq`; every later `output` is greater.
    pub seq: u64,
    pub running: bool,
    pub exit: Option<ExitInfo>,
}

/// A `resize` answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resized {
    pub seq: u64,
    pub changed: bool,
}

/// A `snapshot` answer: feed `data` to a fresh `cols`×`rows` terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotReply {
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub alternate: bool,
    pub fidelity: String,
    pub data: Vec<u8>,
}

/// A `screen` answer: what state scraping reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenReply {
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub alternate_on: bool,
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub title: Option<String>,
    pub modes: Modes,
    pub text: String,
}

type Answer = (HostMsg, Vec<u8>);

#[derive(Default)]
struct Pending {
    waiting: HashMap<u64, mpsc::Sender<Answer>>,
    closed: bool,
}

/// A connection to one host.
pub struct HostClient {
    /// For `shutdown`; reads happen on the reader thread's clone.
    stream: UnixStream,
    writer: Mutex<UnixStream>,
    pending: Arc<Mutex<Pending>>,
    next_req: AtomicU64,
    hello: HostHello,
    timeout: Duration,
}

impl std::fmt::Debug for HostClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostClient").field("hello", &self.hello).finish_non_exhaustive()
    }
}

impl HostClient {
    /// Connect to the host at `socket`, offer `offer`, and authenticate with
    /// `token`. Output and other unasked messages go to `on_event`.
    ///
    /// # Errors
    /// [`ClientError::Auth`] / [`ClientError::Version`] when the host refuses
    /// the handshake; [`ClientError::Connect`] when nobody listens.
    pub fn connect(socket: &Path, token: &str, offer: (u32, u32), on_event: OnEvent) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(socket).map_err(ClientError::Connect)?;
        stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
        stream.set_write_timeout(Some(HELLO_TIMEOUT))?;
        let hello = ClientMsg::Hello {
            protocol: offer,
            token: token.to_string(),
            client: format!("smooth-flow/{}", env!("CARGO_PKG_VERSION")),
        };
        write_frame(&mut &stream, &hello, &[])?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let frame = match read_frame(&mut reader) {
            Ok(Some(f)) => f,
            Ok(None) => return Err(ClientError::Closed),
            Err(e) => return Err(ClientError::Protocol(e.to_string())),
        };
        let hello = match classify::<HostMsg>(&frame, HOST_TYPES) {
            Classified::Known(HostMsg::Hello {
                protocol,
                host_version,
                id,
                pid,
                child_pid,
                cols,
                rows,
                seq,
                running,
                exit,
            }) => {
                if protocol < offer.0 || protocol > offer.1 {
                    return Err(ClientError::Protocol(format!(
                        "host chose protocol {protocol}, outside {}..{}",
                        offer.0, offer.1
                    )));
                }
                HostHello {
                    protocol,
                    host_version,
                    id,
                    pid,
                    child_pid,
                    cols,
                    rows,
                    seq,
                    running,
                    exit,
                }
            }
            Classified::Known(HostMsg::Error { code: c, message, .. }) => {
                return Err(match c.as_str() {
                    code::AUTH => ClientError::Auth(message),
                    code::VERSION => ClientError::Version(message),
                    _ => ClientError::Host { code: c, message },
                })
            }
            other => return Err(ClientError::Protocol(format!("expected hello, got {other:?}"))),
        };
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let pending = Arc::new(Mutex::new(Pending::default()));
        let p = pending.clone();
        std::thread::Builder::new()
            .name("flow-host-client".into())
            .spawn(move || read_loop(reader, &p, &on_event))?;
        Ok(Self {
            writer: Mutex::new(stream.try_clone()?),
            stream,
            pending,
            next_req: AtomicU64::new(1),
            hello,
            timeout: REQUEST_TIMEOUT,
        })
    }

    /// The host's `hello`.
    #[must_use]
    pub const fn hello(&self) -> &HostHello {
        &self.hello
    }

    fn send(&self, msg: &ClientMsg, body: &[u8]) -> Result<(), ClientError> {
        let mut w = lock(&self.writer);
        write_frame(&mut *w, msg, body).map_err(|e| {
            if e.kind() == std::io::ErrorKind::InvalidInput {
                ClientError::Protocol(e.to_string())
            } else {
                ClientError::Io(e)
            }
        })
    }

    fn request(&self, make: impl FnOnce(u64) -> ClientMsg, body: &[u8]) -> Result<Answer, ClientError> {
        let req = self.next_req.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        {
            let mut p = lock(&self.pending);
            if p.closed {
                return Err(ClientError::Closed);
            }
            p.waiting.insert(req, tx);
        }
        if let Err(e) = self.send(&make(req), body) {
            lock(&self.pending).waiting.remove(&req);
            return Err(e);
        }
        match rx.recv_timeout(self.timeout) {
            Ok((HostMsg::Error { code, message, .. }, _)) => Err(ClientError::Host { code, message }),
            Ok(answer) => Ok(answer),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lock(&self.pending).waiting.remove(&req);
                Err(ClientError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ClientError::Closed),
        }
    }

    /// Raw bytes for the PTY (a client's `flow.input`).
    ///
    /// # Errors
    /// When the connection is gone.
    pub fn input(&self, bytes: &[u8]) -> Result<(), ClientError> {
        self.send(&ClientMsg::Input, bytes)
    }

    /// Paste text the way a terminal does (bracketed when the program asked).
    ///
    /// # Errors
    /// When the connection is gone.
    pub fn paste(&self, text: &str) -> Result<(), ClientError> {
        self.send(&ClientMsg::Paste, text.as_bytes())
    }

    /// Press a manifest key name (`Enter`, `C-c`, `Down`) `repeat` times.
    ///
    /// # Errors
    /// [`ClientError::Host`] with `unknown_key` for a name that isn't a key.
    pub fn key(&self, name: &str, repeat: u32) -> Result<(), ClientError> {
        self.request(
            |req| ClientMsg::Key {
                name: name.to_string(),
                repeat: (repeat != 1).then_some(repeat),
                req: Some(req),
            },
            &[],
        )
        .map(|_| ())
    }

    /// Resize the PTY and the VT.
    ///
    /// # Errors
    /// For a zero dimension or a gone connection.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<Resized, ClientError> {
        match self.request(|req| ClientMsg::Resize { req, cols, rows }, &[])? {
            (HostMsg::Resized { seq, changed, .. }, _) => Ok(Resized { seq, changed }),
            (other, _) => Err(ClientError::Protocol(format!("expected resized, got {other:?}"))),
        }
    }

    /// A bounded VT snapshot (the `flow.replay` payload).
    ///
    /// # Errors
    /// When the connection is gone.
    pub fn snapshot(&self, max_bytes: usize) -> Result<SnapshotReply, ClientError> {
        match self.request(|req| ClientMsg::Snapshot { req, max_bytes }, &[])? {
            (
                HostMsg::Snapshot {
                    seq,
                    cols,
                    rows,
                    alternate,
                    fidelity,
                    ..
                },
                data,
            ) => Ok(SnapshotReply {
                seq,
                cols,
                rows,
                alternate,
                fidelity,
                data,
            }),
            (other, _) => Err(ClientError::Protocol(format!("expected snapshot, got {other:?}"))),
        }
    }

    /// The visible screen as text, plus what scraping reads.
    ///
    /// # Errors
    /// When the connection is gone.
    pub fn screen(&self) -> Result<ScreenReply, ClientError> {
        match self.request(|req| ClientMsg::Screen { req }, &[])? {
            (
                HostMsg::Screen {
                    seq,
                    cols,
                    rows,
                    alternate_on,
                    cursor_x,
                    cursor_y,
                    title,
                    modes,
                    ..
                },
                body,
            ) => Ok(ScreenReply {
                seq,
                cols,
                rows,
                alternate_on,
                cursor_x,
                cursor_y,
                title,
                modes,
                text: String::from_utf8_lossy(&body).into_owned(),
            }),
            (other, _) => Err(ClientError::Protocol(format!("expected screen, got {other:?}"))),
        }
    }

    /// Signal the child's process group, escalating to SIGKILL after
    /// `grace`. The `exit` event follows.
    ///
    /// # Errors
    /// When the connection is gone.
    pub fn kill(&self, signal: KillSignal, grace: Duration) -> Result<(), ClientError> {
        let grace_ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX);
        self.request(
            |req| ClientMsg::Kill {
                req,
                signal,
                grace_ms: Some(grace_ms),
            },
            &[],
        )
        .map(|_| ())
    }

    /// After `exit`: the daemon has stored the status and final screen, so
    /// the host may delete its record and exit.
    ///
    /// # Errors
    /// When the connection is gone. A `not_exited` refusal arrives as an
    /// [`HostEvent::Error`].
    pub fn release(&self) -> Result<(), ClientError> {
        self.send(&ClientMsg::Release, &[])
    }

    /// Round-trip a `ping`.
    ///
    /// # Errors
    /// When the host doesn't answer in time.
    pub fn ping(&self) -> Result<(), ClientError> {
        self.request(|req| ClientMsg::Ping { req }, &[]).map(|_| ())
    }

    /// Close the connection. The host keeps running.
    pub fn close(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl Drop for HostClient {
    fn drop(&mut self) {
        self.close();
    }
}

fn read_loop(mut r: BufReader<UnixStream>, pending: &Mutex<Pending>, on_event: &OnEvent) {
    while let Ok(Some(frame)) = read_frame(&mut r) {
        let Classified::Known(msg) = classify::<HostMsg>(&frame, HOST_TYPES) else {
            // A newer host's message this build doesn't know: ignore it.
            continue;
        };
        let body = frame.body;
        let answer_to = match &msg {
            HostMsg::Output { seq } => {
                on_event(HostEvent::Output { seq: *seq, data: body });
                continue;
            }
            HostMsg::Overrun { through_seq } => {
                on_event(HostEvent::Overrun { through_seq: *through_seq });
                continue;
            }
            HostMsg::Exit { code, signal, seq } => {
                on_event(HostEvent::Exit {
                    code: *code,
                    signal: *signal,
                    seq: *seq,
                });
                continue;
            }
            HostMsg::Error { req: None, code, message } => {
                on_event(HostEvent::Error {
                    code: code.clone(),
                    message: message.clone(),
                });
                continue;
            }
            HostMsg::Hello { .. } => continue,
            HostMsg::Error { req: Some(req), .. }
            | HostMsg::Resized { req, .. }
            | HostMsg::Snapshot { req, .. }
            | HostMsg::Screen { req, .. }
            | HostMsg::Ok { req }
            | HostMsg::Pong { req } => *req,
        };
        let waiter = lock(pending).waiting.remove(&answer_to);
        if let Some(tx) = waiter {
            let _ = tx.send((msg, body));
        }
    }
    {
        let mut p = lock(pending);
        p.closed = true;
        // Dropping the senders wakes every waiter with `Closed`.
        p.waiting.clear();
    }
    on_event(HostEvent::Closed);
}
