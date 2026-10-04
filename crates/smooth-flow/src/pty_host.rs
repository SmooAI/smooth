//! `PtyHost` (th-dc9822, ADR-011): the [`SessionHost`] that runs each session
//! under its own `smooth-daemon flow-host` process instead of tmux.
//!
//! The host process (see [`crate::session_host`]) owns the PTY, is the
//! agent's parent and keeps a headless libghostty-vt. This side spawns hosts
//! ([`spawn_host`]), keeps one [`HostClient`] connection per live session, and
//! maps every engine call onto the host protocol
//! (docs/Architecture/SmoothFlow-Session-Host.md, "What the daemon does with
//! it"):
//!
//! | `SessionHost`             | here                                                      |
//! | ------------------------- | --------------------------------------------------------- |
//! | `launch`                  | spawn a host (no `sh` exit-code wrapper), then `hello`    |
//! | `alive`, `pid`            | the host record, and the connection                       |
//! | `exit_status`             | the host's `exit` (exact), or the record's after a restart|
//! | `meta`, `capture_visible` | `screen`                                                  |
//! | `capture_scrollback`      | `snapshot`, replayed into a scratch VT                    |
//! | `paste`, `send_text`      | `paste` (bracketed only under mode 2004), then `Enter`    |
//! | `send_key`                | `key` (the VT's key encoder: DECCKM, Kitty flags)         |
//! | `attach`                  | a subscription to the connection's `output`               |
//! | `kill_session`            | `kill` if it still runs, then `release`                   |
//! | `kill_process_tree`       | `kill` with escalation, waiting for the `exit`            |
//!
//! What tmux gave the engine for free, and how it is kept:
//! - **A dead session stays readable** (tmux's `remain-on-exit`): the host
//!   lingers after its child exits until `release`, so `exit_status`, the
//!   final screen and `alive` keep answering until the engine has settled the
//!   row and called `kill_session`.
//! - **Sessions are found by name after a daemon restart**: a session is found
//!   by its host record (`<dir>/<id>.json`), lazily on first use or eagerly in
//!   [`SessionHost::adopt_existing`].
//! - **One output stream, many attaches**: subscribers are kept per session id,
//!   not per host process, so a relaunch (Kill & Resume, a crash resume) keeps
//!   streaming to the clients that were attached.
//!
//! A session's `seq` continues across hosts: a relaunch passes the previous
//! host's last `seq` + 1 as `seq_start`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::host::{Adopted, AttachStream, HostKind, HostNotice, HostNotify, Launch, PaneDeath, PaneMeta, SessionHost, SessionRef, Snapshot};
use crate::pty::OnOutput;
use crate::session_host::adopt::{self, Found};
use crate::session_host::client::{spawn_host, ClientError, HostClient, HostCommand, HostEvent, HostHello, OnEvent, OFFER, READY_TIMEOUT};
use crate::session_host::protocol::{ExitInfo, KillSignal, SpawnRequest};
use crate::session_host::record::{self, HostRecord};

/// How long `kill_session` waits for a still-running child to exit after
/// SIGTERM before the host escalates to SIGKILL.
const KILL_SESSION_GRACE: Duration = Duration::from_secs(1);
/// After a `kill`, how long beyond its grace to wait for the `exit` event.
const EXIT_WAIT_SLACK: Duration = Duration::from_secs(3);
/// After `release`, how long to wait for the host process to be gone (a
/// relaunch spawns a new host for the same id, which refuses while the old
/// one lives).
const RELEASE_WAIT: Duration = Duration::from_secs(3);
/// The budget [`SessionHost::capture_scrollback`] snapshots with.
const SCROLLBACK_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
/// Rows of history a host keeps.
const SCROLLBACK_ROWS: usize = 10_000;
/// How long a host whose child exited waits, with no daemon connected, for
/// one to read the status (the spec's default).
const LINGER_SECS: u64 = 86_400;
/// Environment a host must not pass on from the daemon: a pane under tmux
/// would have had these set by tmux itself, and here they would point an
/// agent at a tmux it isn't running in.
const DROPPED_ENV: &[&str] = &["TMUX", "TMUX_PANE"];

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a host reported its child's end, as the engine reads it.
const fn death_of(code: Option<i32>, signal: Option<i32>) -> PaneDeath {
    match (code, signal) {
        (Some(c), _) => PaneDeath::Code(c),
        (None, Some(s)) => PaneDeath::Signal(s),
        (None, None) => PaneDeath::Unknown,
    }
}

fn death_of_exit(e: &ExitInfo) -> PaneDeath {
    death_of(e.code, e.signal)
}

/// One output subscriber: an engine bridge.
type Sub = (u64, OnOutput);

/// The `PtyHost`'s state, shared with every connection's event handler.
struct Shared {
    /// The ownership identity: the record's `owner`, and
    /// [`SessionHost::default_socket`].
    owner: String,
    dir: PathBuf,
    cmd: HostCommand,
    /// Live (or lingering) sessions by id.
    sessions: Mutex<HashMap<String, Arc<Entry>>>,
    /// Output subscribers by session id; they outlive a host (a relaunch).
    subs: Mutex<HashMap<String, Vec<Sub>>>,
    next_sub: AtomicU64,
    /// The last output `seq` seen per session id, across hosts.
    seqs: Mutex<HashMap<String, u64>>,
    notify: Mutex<Option<HostNotify>>,
}

impl Shared {
    fn notify(&self, n: HostNotice) {
        let f = lock(&self.notify).clone();
        if let Some(f) = f {
            f(n);
        }
    }

    fn saw_seq(&self, id: &str, seq: u64) {
        let mut seqs = lock(&self.seqs);
        let s = seqs.entry(id.to_string()).or_insert(0);
        *s = (*s).max(seq);
    }

    fn deliver(&self, id: &str, seq: u64, data: Vec<u8>) {
        let subs: Vec<OnOutput> = lock(&self.subs).get(id).map(|v| v.iter().map(|(_, f)| f.clone()).collect()).unwrap_or_default();
        match subs.as_slice() {
            [] => {}
            [one] => one(seq, data),
            many => {
                for f in many {
                    f(seq, data.clone());
                }
            }
        }
    }

    /// Is `e` still the session registered under its id?
    fn is_current(&self, e: &Entry) -> bool {
        lock(&self.sessions).get(&e.id).is_some_and(|cur| std::ptr::eq(Arc::as_ptr(cur), e))
    }
}

/// The connection slot of an entry: the client, and its generation (a
/// `Closed` from an older connection must not clear a newer one).
#[derive(Default)]
struct Slot {
    client: Option<Arc<HostClient>>,
    generation: u64,
}

#[derive(Debug, Clone, Copy)]
struct Live {
    /// `Some` once the child has exited (or its host died).
    death: Option<PaneDeath>,
    cols: u16,
    rows: u16,
}

/// One session's host, as the daemon knows it.
struct Entry {
    id: String,
    /// What the record said when we found (or spawned) the host.
    record: HostRecord,
    slot: Mutex<Slot>,
    live: Mutex<Live>,
    /// Signalled when `death` is set.
    exited: Condvar,
}

impl Entry {
    fn new(record: HostRecord, cols: u16, rows: u16) -> Arc<Self> {
        Arc::new(Self {
            id: record.id.clone(),
            live: Mutex::new(Live {
                death: record.exit.as_ref().map(death_of_exit),
                cols,
                rows,
            }),
            record,
            slot: Mutex::new(Slot::default()),
            exited: Condvar::new(),
        })
    }

    fn death(&self) -> Option<PaneDeath> {
        lock(&self.live).death
    }

    fn set_death(&self, death: PaneDeath) {
        let mut l = lock(&self.live);
        if l.death.is_none() {
            l.death = Some(death);
        }
        drop(l);
        self.exited.notify_all();
    }

    fn set_size(&self, cols: u16, rows: u16) {
        let mut l = lock(&self.live);
        l.cols = cols;
        l.rows = rows;
    }

    fn size(&self) -> (u16, u16) {
        let l = lock(&self.live);
        (l.cols, l.rows)
    }

    /// Wait up to `timeout` for the child to have exited.
    fn wait_exit(&self, timeout: Duration) -> Option<PaneDeath> {
        let l = lock(&self.live);
        let (l, _) = self
            .exited
            .wait_timeout_while(l, timeout, |l| l.death.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        l.death
    }

    fn take_client(&self) -> Option<Arc<HostClient>> {
        lock(&self.slot).client.take()
    }

    /// Take in what a `hello` says: size, `seq`, and an exit that happened
    /// while nobody was connected.
    fn on_hello(&self, shared: &Shared, h: &HostHello) {
        self.set_size(h.cols, h.rows);
        shared.saw_seq(&self.id, h.seq);
        if !h.running {
            self.set_death(h.exit.as_ref().map_or(PaneDeath::Unknown, death_of_exit));
        }
    }

    /// The host process is gone without telling us how its child ended:
    /// settle from what its record says (it is written before `exit` is
    /// sent), else as unknown, and clear its files.
    fn host_died(&self, dir: &Path) {
        let on_disk = record::read_record(&record::record_path(dir, &self.id)).ok();
        let same_host = on_disk
            .as_ref()
            .is_some_and(|r| r.pid == self.record.pid && r.pid_start == self.record.pid_start);
        let exit = on_disk.filter(|_| same_host).and_then(|r| r.exit).or_else(|| self.record.exit.clone());
        self.set_death(exit.as_ref().map_or(PaneDeath::Unknown, death_of_exit));
        if same_host {
            remove_files(dir, &self.record);
        }
    }
}

/// Delete a dead host's record and socket, but only while the record on disk
/// is still that host's: a relaunch may already have a new host under the
/// same id. A socket is only removed when it is a socket named `<id>.sock`.
fn remove_files(dir: &Path, rec: &HostRecord) {
    use std::os::unix::fs::FileTypeExt as _;
    let path = record::record_path(dir, &rec.id);
    let still_ours = record::read_record(&path).is_ok_and(|r| r.pid == rec.pid && r.pid_start == rec.pid_start);
    if !still_ours {
        return;
    }
    let sock_name = format!("{}.sock", rec.id);
    if rec.socket.file_name().is_some_and(|n| n.to_string_lossy() == sock_name)
        && std::fs::symlink_metadata(&rec.socket).is_ok_and(|m| m.file_type().is_socket())
    {
        let _ = std::fs::remove_file(&rec.socket);
    }
    let _ = std::fs::remove_file(path);
}

/// The event sink for one connection to `entry`'s host.
fn handler(shared: Weak<Shared>, entry: Weak<Entry>, generation: u64) -> OnEvent {
    Arc::new(move |ev| {
        let (Some(sh), Some(e)) = (shared.upgrade(), entry.upgrade()) else { return };
        match ev {
            HostEvent::Output { seq, data } => {
                sh.saw_seq(&e.id, seq);
                sh.deliver(&e.id, seq, data);
            }
            HostEvent::Overrun { through_seq } => {
                sh.saw_seq(&e.id, through_seq);
                sh.notify(HostNotice::Overrun { name: e.id.clone() });
            }
            HostEvent::Exit { code, signal, seq } => {
                sh.saw_seq(&e.id, seq);
                e.set_death(death_of(code, signal));
                sh.notify(HostNotice::Exited { name: e.id.clone() });
            }
            HostEvent::Error { code, message } => {
                tracing::debug!(session = %e.id, %code, %message, "flow: session host error");
            }
            HostEvent::Closed => {
                {
                    let mut slot = lock(&e.slot);
                    if slot.generation == generation {
                        slot.client = None;
                    }
                }
                // A closed connection is not a death: the host normally
                // outlives it. Only a host that is really gone settles it.
                if e.death().is_none() && !e.record.host_alive() && sh.is_current(&e) {
                    tracing::warn!(session = %e.id, pid = e.record.pid, "flow: session host died");
                    e.host_died(&sh.dir);
                    sh.notify(HostNotice::Exited { name: e.id.clone() });
                }
            }
        }
    })
}

/// A session host per session (ADR-011). See the module docs.
pub struct PtyHost {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for PtyHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyHost")
            .field("owner", &self.shared.owner)
            .field("dir", &self.shared.dir)
            .finish_non_exhaustive()
    }
}

impl PtyHost {
    /// A host that starts `cmd` per session, keeps records in `dir`, and
    /// supervises under `owner` (the daemon's ownership identity — the same
    /// one its tmux host reports, so flipping hosts never orphans a row).
    #[must_use]
    pub fn new(owner: impl Into<String>, dir: impl Into<PathBuf>, cmd: HostCommand) -> Self {
        Self {
            shared: Arc::new(Shared {
                owner: owner.into(),
                dir: dir.into(),
                cmd,
                sessions: Mutex::new(HashMap::new()),
                subs: Mutex::new(HashMap::new()),
                next_sub: AtomicU64::new(1),
                seqs: Mutex::new(HashMap::new()),
                notify: Mutex::new(None),
            }),
        }
    }

    /// The shipped form: `<daemon exe> flow-host`, records in
    /// [`record::hosts_dir`], owned by `owner`.
    #[must_use]
    pub fn for_daemon(owner: impl Into<String>, daemon_exe: impl Into<PathBuf>) -> Self {
        Self::new(owner, record::hosts_dir(), HostCommand::daemon(daemon_exe))
    }

    /// The directory host records live in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.shared.dir
    }

    /// The registered entry for `id`, finding a host a previous daemon left
    /// running (or a dead one's record) on a miss.
    fn entry(&self, id: &str) -> Option<Arc<Entry>> {
        if let Some(e) = lock(&self.shared.sessions).get(id) {
            return Some(e.clone());
        }
        self.find(id)
    }

    fn require(&self, s: &SessionRef) -> Result<Arc<Entry>> {
        self.entry(&s.name).ok_or_else(|| anyhow!("session {} has no session host", s.name))
    }

    /// Register the host a record describes, if it is ours and speaks a
    /// protocol we do. A dead host's record settles its session.
    fn find(&self, id: &str) -> Option<Arc<Entry>> {
        if !record::valid_id(id) {
            return None;
        }
        let rec = record::read_record(&record::record_path(&self.shared.dir, id)).ok()?;
        if rec.id != id || rec.owner != self.shared.owner {
            return None;
        }
        let alive = rec.host_alive();
        if alive && (rec.protocol < OFFER.0 || rec.protocol > OFFER.1) {
            // Held: left running, reached only by `kill_session`.
            return None;
        }
        let e = Entry::new(rec, crate::tmux::DEFAULT_COLS, crate::tmux::DEFAULT_ROWS);
        if alive {
            if let Err(err) = self.connect(&e) {
                tracing::debug!(session = %id, error = %err, "flow: session host found but not reachable yet");
            }
        } else {
            e.host_died(&self.shared.dir);
        }
        let mut sessions = lock(&self.shared.sessions);
        // Lost a race with another lookup: keep the first.
        Some(sessions.entry(id.to_string()).or_insert(e).clone())
    }

    /// The session whose host's child is `pid`, from the records (a session
    /// not looked at since this daemon started).
    fn find_by_child(&self, pid: u32) -> Option<Arc<Entry>> {
        let entries = std::fs::read_dir(&self.shared.dir).ok()?;
        let id = entries.flatten().map(|e| e.path()).find_map(|p| {
            let named = p.extension().is_some_and(|x| x == "json") && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
            let rec = named.then(|| record::read_record(&p).ok()).flatten()?;
            (rec.child_pid == pid && rec.owner == self.shared.owner).then_some(rec.id)
        })?;
        self.entry(&id)
    }

    /// Connect to `e`'s host (a fresh connection).
    fn connect(&self, e: &Arc<Entry>) -> Result<Arc<HostClient>, ClientError> {
        let mut slot = lock(&e.slot);
        if let Some(c) = &slot.client {
            return Ok(c.clone());
        }
        slot.generation += 1;
        let on_event = handler(Arc::downgrade(&self.shared), Arc::downgrade(e), slot.generation);
        let client = Arc::new(HostClient::connect(&e.record.socket, &e.record.token, OFFER, on_event)?);
        e.on_hello(&self.shared, client.hello());
        slot.client = Some(client.clone());
        Ok(client)
    }

    /// The connection to `s`'s host, reconnecting when it dropped.
    fn client(&self, s: &SessionRef) -> Result<(Arc<Entry>, Arc<HostClient>)> {
        let e = self.require(s)?;
        let connected = lock(&e.slot).client.clone();
        if let Some(c) = connected {
            return Ok((e, c));
        }
        // `host_alive` shells out to `ps`: only on a reconnect.
        if !e.record.host_alive() {
            bail!("session {}'s host is gone", s.name);
        }
        let c = self.connect(&e).with_context(|| format!("connect to session {}'s host", s.name))?;
        Ok((e, c))
    }

    /// A request on `s`'s host. A connection found dead is retried once on
    /// a fresh one; one that timed out is dropped (the next call reconnects)
    /// but not retried, since the host may have acted on it (a key pressed
    /// twice is worse than an error).
    fn call<T>(&self, s: &SessionRef, f: impl Fn(&HostClient) -> Result<T, ClientError>) -> Result<T> {
        let (entry, conn) = self.client(s)?;
        match f(&conn) {
            Err(ClientError::Closed | ClientError::Io(_)) => {
                if let Some(old) = entry.take_client() {
                    old.close();
                }
                let (_, fresh) = self.client(s)?;
                f(&fresh).map_err(anyhow::Error::from)
            }
            Err(err @ ClientError::Timeout) => {
                if let Some(old) = entry.take_client() {
                    old.close();
                }
                Err(err.into())
            }
            r => r.map_err(anyhow::Error::from),
        }
    }

    /// The daemon's environment, minus what only made sense under tmux,
    /// plus the launch's extras: a host's `env` is the child's complete
    /// environment.
    fn child_env(base: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>, extra: &[(String, String)]) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> = base
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .filter(|(k, _)| !DROPPED_ENV.contains(&k.as_str()))
            .collect();
        env.extend(extra.iter().cloned());
        env
    }

    /// End the host `rec` describes without its protocol (one we can't speak
    /// to, or can't reach).
    fn kill_unreachable(&self, rec: &HostRecord) {
        if !adopt::kill_host(&self.shared.dir, rec, KILL_SESSION_GRACE) {
            tracing::warn!(session = %rec.id, pid = rec.pid, "flow: session host survived SIGKILL");
        }
    }

    /// Stop `e`'s child if it still runs, then let its host go.
    fn finish(&self, e: &Arc<Entry>) {
        let current = lock(&e.slot).client.clone();
        // A host we lost the connection to gets one more try, so it can be
        // released rather than signalled.
        let client = match current {
            Some(c) => Some(c),
            None if e.record.host_alive() => self.connect(e).ok(),
            None => None,
        };
        let Some(c) = client else {
            if e.record.host_alive() {
                self.kill_unreachable(&e.record);
            } else {
                remove_files(&self.shared.dir, &e.record);
            }
            return;
        };
        if e.death().is_none() {
            // An error here is a child already gone: its `exit` is in flight.
            let _ = c.kill(KillSignal::Term, KILL_SESSION_GRACE);
            e.wait_exit(KILL_SESSION_GRACE + EXIT_WAIT_SLACK);
        }
        let released = e.death().is_some() && c.release().is_ok();
        let deadline = Instant::now() + RELEASE_WAIT;
        while released && e.record.host_alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        c.close();
        if e.record.host_alive() {
            self.kill_unreachable(&e.record);
        }
    }

    /// A second handle on the same state (for attach streams).
    fn clone_handle(&self) -> Self {
        Self { shared: self.shared.clone() }
    }
}

impl SessionHost for PtyHost {
    fn kind(&self) -> HostKind {
        HostKind::Pty
    }

    fn available(&self) -> bool {
        true
    }

    fn default_socket(&self) -> String {
        self.shared.owner.clone()
    }

    fn launch(&self, s: &SessionRef, spec: &Launch<'_>) -> Result<u32> {
        if spec.argv.is_empty() {
            bail!("cannot launch an empty argv");
        }
        if !record::valid_id(&s.name) {
            bail!("session id {:?} is not a flow session id (fs- and 8 lowercase hex)", s.name);
        }
        crate::tmux::check_program(&spec.argv[0], spec.env)?;
        record::ensure_private_dir(&self.shared.dir)?;
        if lock(&self.shared.sessions).contains_key(&s.name) {
            self.kill_session(s);
        }
        let seq_start = lock(&self.shared.seqs).get(&s.name).map_or(0, |last| last + 1);
        let (cols, rows) = (crate::tmux::DEFAULT_COLS, crate::tmux::DEFAULT_ROWS);
        let req = SpawnRequest {
            token: record::new_token(),
            dir: self.shared.dir.clone(),
            argv: spec.argv.to_vec(),
            cwd: spec.cwd.to_path_buf(),
            env: Self::child_env(std::env::vars_os(), spec.env),
            cols,
            rows,
            seq_start,
            scrollback_rows: SCROLLBACK_ROWS,
            linger_secs: LINGER_SECS,
            owner: self.shared.owner.clone(),
            max_queue_bytes: None,
        };
        let spawned = spawn_host(&self.shared.cmd, &s.name, &req, READY_TIMEOUT)?;
        let rec = record::read_record(&record::record_path(&self.shared.dir, &s.name)).context("read the new host's record")?;
        if rec.pid != spawned.pid {
            bail!(
                "the host record for {} names pid {}, not the host that just started ({})",
                s.name,
                rec.pid,
                spawned.pid
            );
        }
        let e = Entry::new(rec, cols, rows);
        if let Err(err) = self.connect(&e) {
            self.kill_unreachable(&e.record);
            return Err(anyhow::Error::from(err).context(format!("connect to the new host for {}", s.name)));
        }
        lock(&self.shared.seqs).insert(s.name.clone(), seq_start);
        let child = e.record.child_pid;
        lock(&self.shared.sessions).insert(s.name.clone(), e);
        Ok(child)
    }

    fn alive(&self, s: &SessionRef) -> bool {
        self.entry(&s.name).is_some()
    }

    fn pid(&self, s: &SessionRef) -> Result<u32> {
        Ok(self.require(s)?.record.child_pid)
    }

    fn meta(&self, s: &SessionRef) -> Result<PaneMeta> {
        let r = self.call(s, HostClient::screen)?;
        Ok(PaneMeta {
            title: r.title.unwrap_or_default(),
            alternate_on: r.alternate_on,
            cursor_y: Some(usize::from(r.cursor_y)),
        })
    }

    fn exit_status(&self, s: &SessionRef) -> Result<Option<PaneDeath>> {
        let e = self.require(s)?;
        if let Some(d) = e.death() {
            return Ok(Some(d));
        }
        if lock(&e.slot).client.is_some() {
            // Connected: an exit would have arrived as an event.
            return Ok(None);
        }
        if !e.record.host_alive() {
            e.host_died(&self.shared.dir);
            return Ok(e.death());
        }
        // Keep the output stream up: a connection that dropped (a host
        // restart of the daemon's side, a timeout) comes back here.
        let _ = self.connect(&e);
        Ok(e.death())
    }

    fn recorded_exit(&self, _exit_prefix: &Path, _pid: u32) -> Option<i32> {
        // No exit-code wrapper: the host is the parent and reports the
        // status itself.
        None
    }

    fn clear_recorded_exits(&self, _exit_prefix: &Path) {}

    fn size(&self, s: &SessionRef) -> Result<(u16, u16)> {
        Ok(self.require(s)?.size())
    }

    fn capture_visible(&self, s: &SessionRef) -> Result<String> {
        let r = self.call(s, HostClient::screen)?;
        if let Some(e) = self.entry(&s.name) {
            e.set_size(r.cols, r.rows);
        }
        Ok(r.text)
    }

    fn capture_scrollback(&self, s: &SessionRef) -> Result<String> {
        let snap = self.call(s, |c| c.snapshot(SCROLLBACK_SNAPSHOT_BYTES))?;
        let mut vt = smooth_flow_vt::Vt::new(snap.cols, snap.rows, SCROLLBACK_ROWS).context("scratch terminal")?;
        vt.feed(&snap.data);
        Ok(vt.plain_scrollback())
    }

    fn paste(&self, s: &SessionRef, text: &str) -> Result<()> {
        self.call(s, |c| c.paste(text))
    }

    fn send_text(&self, s: &SessionRef, text: &str) -> Result<()> {
        self.paste(s, text)?;
        self.send_key(s, "Enter")
    }

    fn send_key(&self, s: &SessionRef, key: &str) -> Result<()> {
        match self.call(s, |c| c.key(key, 1)) {
            Ok(()) => Ok(()),
            // tmux's send-keys types a name it doesn't know literally;
            // keep that for anything printable.
            Err(e)
                if e.downcast_ref::<ClientError>()
                    .is_some_and(|c| matches!(c, ClientError::Host { code, .. } if code == crate::session_host::protocol::code::UNKNOWN_KEY))
                    && !key.is_empty()
                    && !key.chars().any(char::is_control) =>
            {
                self.call(s, |c| c.input(key.as_bytes()))
            }
            Err(e) => Err(e),
        }
    }

    fn kill_session(&self, s: &SessionRef) {
        let taken = lock(&self.shared.sessions).remove(&s.name);
        match taken {
            Some(e) => self.finish(&e),
            None => {
                // Never registered (a held host, or one we never looked at):
                // end it by its record.
                if let Ok(rec) = record::read_record(&record::record_path(&self.shared.dir, &s.name)) {
                    if rec.id == s.name && rec.owner == self.shared.owner {
                        self.kill_unreachable(&rec);
                    }
                }
            }
        }
    }

    fn kill_server(&self, socket: &str) {
        if socket != self.shared.owner {
            return;
        }
        let ids: Vec<String> = lock(&self.shared.sessions).keys().cloned().collect();
        for id in ids {
            self.kill_session(&SessionRef::new(socket, id));
        }
        let Ok(entries) = std::fs::read_dir(&self.shared.dir) else { return };
        for p in entries.flatten().map(|e| e.path()) {
            let is_record = p.extension().is_some_and(|x| x == "json") && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
            if let Some(rec) = is_record.then(|| record::read_record(&p).ok()).flatten() {
                if rec.owner == self.shared.owner && record::record_path(&self.shared.dir, &rec.id) == p {
                    self.kill_unreachable(&rec);
                }
            }
        }
    }

    fn attach(&self, s: &SessionRef, cols: u16, rows: u16, on_output: OnOutput) -> Result<Arc<dyn AttachStream>> {
        let (e, _) = self.client(s)?;
        let sub = self.shared.next_sub.fetch_add(1, Ordering::Relaxed);
        lock(&self.shared.subs).entry(s.name.clone()).or_default().push((sub, on_output));
        let stream = Arc::new(PtyStream {
            pty: self.clone_handle(),
            target: s.clone(),
            sub,
            closed: AtomicBool::new(false),
            clients: AtomicU64::new(0),
        });
        // The engine sizes the stream right after; a size that already
        // matches answers `changed:false` and costs nothing.
        if e.size() != (cols, rows) {
            stream.resize(cols, rows)?;
        }
        Ok(stream)
    }

    fn process_start(&self, pid: u32) -> Option<i64> {
        crate::proc::start_time(pid)
    }

    fn process_alive(&self, pid: u32, recorded_start: Option<i64>) -> bool {
        crate::proc::is_alive(pid, recorded_start)
    }

    fn kill_process_tree(&self, pid: u32, grace: Duration) {
        let registered = lock(&self.shared.sessions).values().find(|e| e.record.child_pid == pid).cloned();
        let Some(e) = registered.or_else(|| self.find_by_child(pid)) else {
            crate::proc::kill_tree(pid, grace);
            return;
        };
        if e.death().is_some() {
            return;
        }
        let target = SessionRef::new(self.shared.owner.clone(), e.id.clone());
        match self.call(&target, |c| c.kill(KillSignal::Term, grace)) {
            // The host escalates to SIGKILL after `grace`; the `exit` follows.
            Ok(()) => {
                e.wait_exit(grace + EXIT_WAIT_SLACK);
            }
            Err(err) => {
                tracing::warn!(session = %e.id, error = %err, "flow: session host refused the kill; signalling the process group");
                crate::proc::kill_tree(pid, grace);
            }
        }
    }

    fn snapshot(&self, s: &SessionRef, max_bytes: usize) -> Result<Option<Snapshot>> {
        let r = self.call(s, |c| c.snapshot(max_bytes))?;
        if let Some(e) = self.entry(&s.name) {
            e.set_size(r.cols, r.rows);
        }
        Ok(Some(Snapshot {
            seq: r.seq,
            cols: r.cols,
            rows: r.rows,
            data: r.data,
        }))
    }

    fn set_notify(&self, notify: HostNotify) {
        *lock(&self.shared.notify) = Some(notify);
    }

    fn adopt_existing(&self) -> Vec<Adopted> {
        let made: Mutex<HashMap<String, Arc<Entry>>> = Mutex::new(HashMap::new());
        let shared = Arc::downgrade(&self.shared);
        let events = |rec: &HostRecord| -> OnEvent {
            let e = Entry::new(rec.clone(), crate::tmux::DEFAULT_COLS, crate::tmux::DEFAULT_ROWS);
            // Adoption's connection is the entry's first, generation 1.
            lock(&e.slot).generation = 1;
            let h = handler(shared.clone(), Arc::downgrade(&e), 1);
            lock(&made).insert(rec.id.clone(), e);
            h
        };
        let found = adopt::adopt(&self.shared.dir, &self.shared.owner, OFFER, &events);
        let mut made = made.into_inner().unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::new();
        for f in found {
            match f {
                Found::Live { record, client } => {
                    let Some(e) = made.remove(&record.id) else { continue };
                    let client = Arc::new(client);
                    e.on_hello(&self.shared, client.hello());
                    lock(&e.slot).client = Some(client);
                    tracing::info!(session = %record.id, pid = record.pid, "flow: adopted a running session host");
                    lock(&self.shared.sessions).insert(record.id.clone(), e);
                    out.push(Adopted::Live { name: record.id });
                }
                Found::Stale { record } => {
                    let death = record.exit.as_ref().map_or(PaneDeath::Unknown, death_of_exit);
                    let e = Entry::new(record.clone(), crate::tmux::DEFAULT_COLS, crate::tmux::DEFAULT_ROWS);
                    e.set_death(death);
                    tracing::info!(session = %record.id, ?death, "flow: a session host died while the daemon was down");
                    lock(&self.shared.sessions).insert(record.id.clone(), e);
                    out.push(Adopted::Settled { name: record.id, death });
                }
                Found::Held { record, reason } => {
                    tracing::warn!(session = %record.id, %reason, "flow: a session host speaks a protocol this daemon doesn't; leaving it running");
                    out.push(Adopted::Held { name: record.id, reason });
                }
                Found::Unreachable { record, reason } => {
                    tracing::warn!(session = %record.id, %reason, "flow: a running session host is not reachable; leaving it running");
                    out.push(Adopted::Unreachable { name: record.id, reason });
                }
                Found::Foreign { record } => {
                    tracing::debug!(session = %record.id, owner = %record.owner, "flow: another daemon's session host");
                }
                Found::Unreadable { path, reason } => {
                    tracing::warn!(path = %path.display(), %reason, "flow: unreadable session host record; leaving it alone");
                }
            }
        }
        out
    }

    fn forget(&self, s: &SessionRef) {
        let taken = lock(&self.shared.sessions).remove(&s.name);
        if let Some(c) = taken.and_then(|e| e.take_client()) {
            c.close();
        }
    }
}

/// An engine bridge on a `pty` session: a subscription to its output by
/// session id (so it survives a relaunch), plus input and resize through the
/// current host.
struct PtyStream {
    pty: PtyHost,
    target: SessionRef,
    sub: u64,
    closed: AtomicBool,
    clients: AtomicU64,
}

impl AttachStream for PtyStream {
    fn write(&self, data: &[u8]) -> Result<()> {
        if self.is_closed() {
            bail!("attach stream for {} is closed", self.target.name);
        }
        self.pty.call(&self.target, |c| c.input(data))
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        if self.is_closed() {
            bail!("attach stream for {} is closed", self.target.name);
        }
        let r = self.pty.call(&self.target, |c| c.resize(cols, rows))?;
        if let Some(e) = self.pty.entry(&self.target.name) {
            e.set_size(cols, rows);
        }
        tracing::trace!(session = %self.target.name, cols, rows, changed = r.changed, "flow: pty resized");
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut subs = lock(&self.pty.shared.subs);
        if let Some(v) = subs.get_mut(&self.target.name) {
            v.retain(|(id, _)| *id != self.sub);
            if v.is_empty() {
                subs.remove(&self.target.name);
            }
        }
    }

    fn clients(&self) -> &AtomicU64 {
        &self.clients
    }
}

impl Drop for PtyStream {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn exits_map_onto_pane_deaths() {
        assert_eq!(death_of(Some(0), None), PaneDeath::Code(0));
        assert_eq!(death_of(Some(7), Some(9)), PaneDeath::Code(7), "a code wins");
        assert_eq!(death_of(None, Some(15)), PaneDeath::Signal(15));
        assert_eq!(death_of(None, None), PaneDeath::Unknown, "nobody knows: never guessed");
    }

    #[test]
    fn the_childs_env_drops_tmux_and_takes_the_launch_extras() {
        let base = [("TMUX", "/tmp/tmux-1/default,1,0"), ("TMUX_PANE", "%1"), ("PATH", "/usr/bin"), ("HOME", "/h")]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()));
        let env = PtyHost::child_env(base, &[("SMOOTH_FLOW_ID".into(), "fs-00000001".into()), ("PATH".into(), "/x".into())]);
        assert!(!env.contains_key("TMUX") && !env.contains_key("TMUX_PANE"), "an agent here is not under tmux");
        assert_eq!(env.get("SMOOTH_FLOW_ID").map(String::as_str), Some("fs-00000001"));
        assert_eq!(env.get("PATH").map(String::as_str), Some("/x"), "the launch's PATH wins");
        assert_eq!(env.get("HOME").map(String::as_str), Some("/h"), "the rest is the daemon's");
    }

    #[test]
    fn an_unknown_id_or_a_missing_record_is_not_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let h = PtyHost::new("me", dir.path(), HostCommand::daemon("/nonexistent"));
        assert!(!h.alive(&SessionRef::new("me", "fs-0000000a")));
        assert!(!h.alive(&SessionRef::new("me", "../../etc/passwd")), "an id is never a path");
        assert!(h.pid(&SessionRef::new("me", "fs-0000000a")).is_err());
        assert_eq!(h.kind(), HostKind::Pty);
        assert_eq!(h.default_socket(), "me");
        assert_eq!(h.recorded_exit(dir.path(), 1), None, "no wrapper, no exit files");
    }

    #[test]
    fn launching_a_missing_program_fails_before_any_host_starts() {
        let dir = tempfile::tempdir().unwrap();
        let h = PtyHost::new("me", dir.path().join("hosts"), HostCommand::daemon("/nonexistent"));
        let err = h
            .launch(
                &SessionRef::new("me", "fs-0000000b"),
                &Launch {
                    cwd: dir.path(),
                    argv: &["/nonexistent/th-dc9822-agent".to_string()],
                    env: &[],
                    exit_prefix: None,
                },
            )
            .unwrap_err();
        assert!(format!("{err:#}").contains("th-dc9822-agent"), "{err:#}");
        let bad = h.launch(
            &SessionRef::new("me", "not-an-id"),
            &Launch {
                cwd: dir.path(),
                argv: &["sh".to_string()],
                env: &[],
                exit_prefix: None,
            },
        );
        assert!(bad.is_err());
    }

    /// A dead host's record settles its session from the recorded exit, and
    /// only that host's files are removed.
    #[test]
    fn a_dead_hosts_record_reads_as_an_exited_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let dead = c.id();
        c.wait().unwrap();
        let rec = HostRecord {
            v: 1,
            protocol: 1,
            id: "fs-0000000c".into(),
            host_version: "0".into(),
            pid: dead,
            pid_start: Some(1),
            child_pid: dead,
            socket: dir.path().join("fs-0000000c.sock"),
            token: "0".repeat(64),
            owner: "me".into(),
            cwd: PathBuf::from("/"),
            argv: vec!["x".into()],
            created_at: "t".into(),
            exit: Some(ExitInfo {
                code: Some(5),
                signal: None,
                at: "t".into(),
            }),
        };
        record::write_record(dir.path(), &rec).unwrap();
        let h = PtyHost::new("me", dir.path(), HostCommand::daemon("/nonexistent"));
        let s = SessionRef::new("me", "fs-0000000c");
        assert!(h.alive(&s), "a dead session stays readable until it is killed");
        assert_eq!(h.exit_status(&s).unwrap(), Some(PaneDeath::Code(5)));
        assert!(!record::record_path(dir.path(), "fs-0000000c").exists(), "the stale record is cleared");
        h.kill_session(&s);
        assert!(!h.alive(&s));
        // Another owner's record is never ours.
        record::write_record(dir.path(), &HostRecord { owner: "them".into(), ..rec }).unwrap();
        assert!(!h.alive(&s));
    }
}
