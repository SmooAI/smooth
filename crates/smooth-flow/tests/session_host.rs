//! The session host end to end (th-e4aef9): real `smooth-flow-host`
//! processes, spawned the way the daemon spawns `smooth-daemon flow-host`,
//! driven through `HostClient` and, for the adversarial cases, raw frames.
//!
//! Every test uses its own temp host dir, so nothing touches
//! `~/.smooth/flow-hosts`, and a guard kills whatever hosts a test leaves.
//! Waits poll against deadlines (load on a shared box is high), never fixed
//! sleeps, except where a test proves something does NOT happen.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "test assertions")]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use smooth_flow::session_host::adopt::{adopt, kill_host, Found};
use smooth_flow::session_host::client::{spawn_host, ClientError, HostClient, HostCommand, HostEvent, OnEvent, Spawned, OFFER, READY_TIMEOUT};
use smooth_flow::session_host::protocol::{code, read_frame, write_frame, ClientMsg, Frame, KillSignal, SpawnRequest};
use smooth_flow::session_host::record::{new_token, read_record, record_path, HostRecord};
use smooth_flow::vt::Vt;

const OWNER: &str = "test-owner";
const WAIT: Duration = Duration::from_secs(30);

fn host_cmd() -> HostCommand {
    HostCommand {
        program: env!("CARGO_BIN_EXE_smooth-flow-host").into(),
        args: vec![],
    }
}

fn new_id() -> String {
    format!("fs-{:08x}", rand_u32())
}

fn rand_u32() -> u32 {
    // Unique enough across one test run: time, pid and a counter.
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
    t ^ std::process::id().rotate_left(16) ^ N.fetch_add(0x9e37_79b9, Ordering::Relaxed)
}

/// Poll `f` until it holds or `WAIT` passes.
fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

/// The events a client delivered, with a condvar to wait on.
#[derive(Clone, Default)]
struct Events(Arc<(Mutex<Vec<HostEvent>>, Condvar)>);

impl Events {
    fn sink(&self) -> OnEvent {
        let me = self.clone();
        Arc::new(move |e| {
            me.0 .0.lock().unwrap().push(e);
            me.0 .1.notify_all();
        })
    }

    fn wait(&self, what: &str, pred: impl Fn(&[HostEvent]) -> bool) {
        let deadline = Instant::now() + WAIT;
        let mut g = self.0 .0.lock().unwrap();
        while !pred(&g) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for {what}; got {:?}", summary(&g));
            g = self.0 .1.wait_timeout(g, left).unwrap().0;
        }
    }

    fn all(&self) -> Vec<HostEvent> {
        self.0 .0.lock().unwrap().clone()
    }

    fn text(&self) -> String {
        output_text(&self.all())
    }

    fn wait_text(&self, needle: &str) {
        self.wait(needle, |e| output_text(e).contains(needle));
    }

    fn output_seqs(&self) -> Vec<u64> {
        self.all()
            .iter()
            .filter_map(|e| match e {
                HostEvent::Output { seq, .. } => Some(*seq),
                _ => None,
            })
            .collect()
    }

    fn exit(&self) -> (Option<i32>, Option<i32>, u64) {
        self.wait("exit", |e| e.iter().any(|x| matches!(x, HostEvent::Exit { .. })));
        self.all()
            .into_iter()
            .find_map(|e| match e {
                HostEvent::Exit { code, signal, seq } => Some((code, signal, seq)),
                _ => None,
            })
            .unwrap()
    }
}

fn output_text(events: &[HostEvent]) -> String {
    let mut out = Vec::new();
    for e in events {
        if let HostEvent::Output { data, .. } = e {
            out.extend_from_slice(data);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn summary(events: &[HostEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            HostEvent::Output { seq, data } => format!("output#{seq}({:?})", String::from_utf8_lossy(data)),
            other => format!("{other:?}"),
        })
        .collect()
}

/// A temp host dir; on drop, kills every host still recorded in it.
struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("fh").tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn request(&self, argv: &[&str]) -> SpawnRequest {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()));
        env.insert("HOME".to_string(), self.path().display().to_string());
        SpawnRequest {
            token: new_token(),
            dir: self.path().to_path_buf(),
            argv: argv.iter().map(ToString::to_string).collect(),
            cwd: self.path().to_path_buf(),
            env,
            cols: 80,
            rows: 24,
            seq_start: 0,
            scrollback_rows: 1000,
            linger_secs: 60,
            owner: OWNER.into(),
            max_queue_bytes: None,
        }
    }

    fn spawn(req: &SpawnRequest) -> (String, Spawned) {
        let id = new_id();
        let s = spawn_host(&host_cmd(), &id, req, READY_TIMEOUT * 4).unwrap();
        (id, s)
    }

    /// Spawn `sh -c script` and connect.
    fn start(&self, script: &str) -> Session {
        self.start_with(self.request(&["sh", "-c", script]))
    }

    fn start_with(&self, req: SpawnRequest) -> Session {
        // In this rig's dir, so its drop finds (and kills) the host.
        assert_eq!(req.dir, self.path());
        let (id, spawned) = Self::spawn(&req);
        let events = Events::default();
        let client = HostClient::connect(&spawned.socket, &req.token, OFFER, events.sink()).unwrap();
        Session {
            id,
            req,
            spawned,
            client,
            events,
        }
    }

    fn record(&self, id: &str) -> HostRecord {
        read_record(&record_path(self.path(), id)).unwrap()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Ok(entries) = std::fs::read_dir(self.path()) {
            for e in entries.flatten() {
                if let Ok(rec) = read_record(&e.path()) {
                    kill_host(self.path(), &rec, Duration::from_millis(500));
                }
            }
        }
    }
}

struct Session {
    id: String,
    req: SpawnRequest,
    spawned: Spawned,
    client: HostClient,
    events: Events,
}

impl Session {
    fn wait_screen(&self, needle: &str) -> String {
        let mut last = String::new();
        eventually(&format!("{needle:?} on screen"), || {
            last = self.client.screen().unwrap().text;
            last.contains(needle)
        });
        last
    }
}

/// Connect and handshake by hand, then hand back the socket.
fn raw_hello(socket: &Path, token: &str) -> UnixStream {
    let s = UnixStream::connect(socket).unwrap();
    s.set_read_timeout(Some(WAIT)).unwrap();
    write_frame(
        &mut &s,
        &ClientMsg::Hello {
            protocol: (1, 1),
            token: token.into(),
            client: "raw".into(),
        },
        &[],
    )
    .unwrap();
    let f = read_frame(&mut &s).unwrap().unwrap();
    assert_eq!(f.kind(), Some("hello"), "{:?}", f.header);
    s
}

/// Read frames until EOF (or an error), returning them.
fn drain(s: &UnixStream) -> Vec<Frame> {
    let mut out = Vec::new();
    while let Ok(Some(f)) = read_frame(&mut &*s) {
        out.push(f);
    }
    out
}

fn host_dead(rec: &HostRecord) -> bool {
    !rec.host_alive()
}

#[test]
fn output_seq_and_the_exact_exit_code() {
    let rig = Rig::new();
    let s = rig.start(r#"read x; printf "hi-%s" "$x"; exit 7"#);
    let hello = s.client.hello().clone();
    assert!(hello.running);
    assert_eq!(hello.id, s.id);
    assert_eq!(hello.pid, s.spawned.pid);
    assert_eq!((hello.cols, hello.rows), (80, 24));
    s.client.input(b"there\n").unwrap();
    let (code, signal, exit_seq) = s.events.exit();
    assert_eq!((code, signal), (Some(7), None));
    assert!(s.events.text().contains("hi-there"), "{:?}", s.events.text());
    let seqs = s.events.output_seqs();
    assert!(!seqs.is_empty());
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "seq rises by one per output: {seqs:?}");
    assert!(seqs[0] > hello.seq, "outputs after hello are newer than it");
    assert!(exit_seq >= *seqs.last().unwrap(), "exit carries the final seq");
    // The record learned the status before the daemon did.
    let rec = rig.record(&s.id);
    let exit = rec.exit.unwrap();
    assert_eq!((exit.code, exit.signal), (Some(7), None));
    assert_eq!(rec.argv, s.req.argv);
    assert_eq!(rec.owner, OWNER);
}

#[test]
fn a_signal_death_reports_the_signal() {
    let rig = Rig::new();
    let s = rig.start("read x; kill -KILL $$");
    s.client.input(b"\n").unwrap();
    let (code, signal, _) = s.events.exit();
    assert_eq!((code, signal), (None, Some(9)));
}

#[test]
fn seq_continues_from_seq_start() {
    let rig = Rig::new();
    let mut req = rig.request(&["cat"]);
    req.seq_start = 1000;
    let s = rig.start_with(req);
    assert_eq!(s.client.hello().seq, 1000, "nothing printed yet");
    s.client.input(b"x\n").unwrap();
    s.events.wait_text("x");
    assert_eq!(s.events.output_seqs()[0], 1001);
}

#[test]
fn input_reaches_the_child() {
    let rig = Rig::new();
    let s = rig.start_with(rig.request(&["cat"]));
    s.client.input(b"hello-cat\n").unwrap();
    s.events.wait("echo and cat's copy", |e| output_text(e).matches("hello-cat").count() >= 2);
}

#[test]
fn snapshot_replays_to_the_same_screen() {
    let rig = Rig::new();
    let s = rig.start(r"printf 'line one\r\n\033[1;31mred\033[0m two\r\n'; read x");
    let screen = s.wait_screen("red two");
    let snap = s.client.snapshot(1 << 20).unwrap();
    assert_eq!(snap.fidelity, "full");
    assert!(!snap.alternate);
    assert_eq!((snap.cols, snap.rows), (80, 24));
    let snap_text = String::from_utf8_lossy(&snap.data);
    assert!(snap_text.contains("line one") && snap_text.contains("red"), "{snap_text:?}");
    let mut fresh = Vt::new(snap.cols, snap.rows, 1000).unwrap();
    fresh.feed(&snap.data);
    assert_eq!(fresh.plain_screen(), screen);
    // A snapshot's seq is the screen's: nothing printed in between.
    assert_eq!(snap.seq, s.client.screen().unwrap().seq);
    // A tiny budget degrades instead of failing.
    let small = s.client.snapshot(16).unwrap();
    assert!(small.data.len() <= 16);
    assert!(["plain", "empty"].contains(&small.fidelity.as_str()), "{}", small.fidelity);
}

#[test]
fn resize_reaches_the_child() {
    let rig = Rig::new();
    let s = rig.start("stty size; read x; stty size; read y");
    s.wait_screen("24 80");
    let r = s.client.resize(100, 30).unwrap();
    assert!(r.changed);
    assert!(!s.client.resize(100, 30).unwrap().changed, "same size is not a change");
    s.client.input(b"\n").unwrap();
    s.wait_screen("30 100");
    let sc = s.client.screen().unwrap();
    assert_eq!((sc.cols, sc.rows), (100, 30));
    match s.client.resize(0, 5) {
        Err(ClientError::Host { code: c, .. }) => assert_eq!(c, code::BAD_REQUEST),
        other => panic!("{other:?}"),
    }
}

/// The host is the session's terminal: a DA query from the child is
/// answered from the host's VT (`take_replies`), with no client involved.
#[test]
fn terminal_queries_are_answered_by_the_host() {
    let rig = Rig::new();
    let got = rig.path().join("da.bin");
    let script = format!(
        "stty raw -echo; printf '\\033[c'; dd bs=1 count=3 of='{}' 2>/dev/null; printf done; read x",
        got.display()
    );
    let s = rig.start(&script);
    s.wait_screen("done");
    let reply = std::fs::read(&got).unwrap();
    assert_eq!(reply, b"\x1b[?", "a DA1 reply reached the child");
}

#[test]
fn paste_keys_and_modes() {
    let rig = Rig::new();
    let s = rig.start(r"printf '\033[?2004h\033[?1002h\033[?1006h\033]2;my-title\007ready'; cat");
    s.wait_screen("ready");
    let sc = s.client.screen().unwrap();
    assert!(sc.modes.bracketed_paste);
    assert_eq!(sc.modes.mouse_tracking.as_deref(), Some("button"));
    assert_eq!(sc.modes.mouse_format, "sgr");
    assert!(!sc.modes.cursor_keys_app);
    assert_eq!(sc.modes.kitty_keyboard, 0);
    assert_eq!(sc.title.as_deref(), Some("my-title"));
    // Bracketed paste: the markers wrap it, and an embedded end marker is defused.
    s.client.paste("pasted\x1b[201~text").unwrap();
    s.client.key("Enter", 1).unwrap();
    s.events.wait("the bracketed paste", |e| output_text(e).contains("[200~pasted [201~text"));
    match s.client.key("NoSuchKey", 1) {
        Err(ClientError::Host { code: c, .. }) => assert_eq!(c, code::UNKNOWN_KEY),
        other => panic!("{other:?}"),
    }
    s.client.ping().unwrap();
}

/// THE property: the daemon dies, the session doesn't. A new daemon adopts
/// the host from its record and finds the same terminal with `seq` rising.
#[test]
fn a_restarted_daemon_adopts_the_host_with_the_session_intact() {
    let rig = Rig::new();
    let s = rig.start_with(rig.request(&["cat"]));
    s.client.input(b"before-crash\n").unwrap();
    s.events.wait("first output", |e| output_text(e).matches("before-crash").count() >= 2);
    let seq_before = *s.events.output_seqs().last().unwrap();
    let Session { id, client, spawned, .. } = s;
    // Daemon one goes away.
    drop(client);
    // Daemon two connected and then died without closing: a half-open
    // socket nobody reads (what a SIGKILLed daemon leaves).
    let token = rig.record(&id).token;
    let zombie = raw_hello(&spawned.socket, &token);

    // Daemon three boots and adopts.
    let events = Events::default();
    let sink = events.sink();
    let found = adopt(rig.path(), OWNER, OFFER, &move |_| sink.clone());
    let client = match found.into_iter().next() {
        Some(Found::Live { record, client }) => {
            assert_eq!(record.id, id);
            client
        }
        other => panic!("{other:?}"),
    };
    let hello = client.hello().clone();
    assert!(hello.running);
    assert!(hello.seq >= seq_before, "seq continued: {} < {seq_before}", hello.seq);
    // The half-open connection was superseded and closed.
    let frames = drain(&zombie);
    assert!(
        frames.iter().any(|f| f.kind() == Some("error") && f.header["code"] == code::SUPERSEDED),
        "{:?}",
        frames.iter().map(|f| &f.header).collect::<Vec<_>>()
    );
    // Same terminal: the old output is still on screen and in the snapshot.
    assert!(client.screen().unwrap().text.contains("before-crash"));
    let snap = client.snapshot(1 << 20).unwrap();
    assert!(String::from_utf8_lossy(&snap.data).contains("before-crash"));
    // Same process: input still reaches cat, with newer seqs.
    client.input(b"after-adopt\n").unwrap();
    events.wait("output after adoption", |e| output_text(e).contains("after-adopt"));
    assert!(events.output_seqs().iter().all(|&q| q > hello.seq));
    assert!(rig.record(&id).host_alive());
}

/// The child exits while no daemon is connected: the record keeps the
/// status, the next daemon settles from it and releases the host.
#[test]
fn an_exit_with_no_daemon_is_settled_by_the_next_one() {
    let rig = Rig::new();
    let s = rig.start("read x; exit 3");
    let Session { id, client, .. } = s;
    client.input(b"go\n").unwrap();
    drop(client);
    eventually("the record's exit", || rig.record(&id).exit.is_some());
    let found = adopt(rig.path(), OWNER, OFFER, &|_| Arc::new(|_| {}));
    let (record, client) = match found.into_iter().next() {
        Some(Found::Live { record, client }) => (record, client),
        other => panic!("{other:?}"),
    };
    let hello = client.hello();
    assert!(!hello.running);
    assert_eq!(hello.exit.as_ref().unwrap().code, Some(3));
    // Lingering: the final screen is still readable.
    client.screen().unwrap();
    client.release().unwrap();
    eventually("the released host to exit", || host_dead(&record));
    assert!(!record_path(rig.path(), &id).exists(), "record removed on release");
    assert!(!record.socket.exists(), "socket removed on release");
}

#[test]
fn release_before_exit_is_refused() {
    let rig = Rig::new();
    let s = rig.start_with(rig.request(&["cat"]));
    s.client.release().unwrap();
    s.events.wait("not_exited", |e| {
        e.iter().any(|x| matches!(x, HostEvent::Error { code, .. } if code == code::NOT_EXITED))
    });
    s.client.ping().unwrap();
}

#[test]
fn a_lingering_host_exits_after_linger_with_no_daemon() {
    let rig = Rig::new();
    let mut req = rig.request(&["sh", "-c", "exit 0"]);
    req.linger_secs = 1;
    let (id, _) = Rig::spawn(&req);
    let rec = rig.record(&id);
    eventually("the lingering host to clean up and exit", || {
        host_dead(&rec) && !record_path(rig.path(), &id).exists()
    });
    assert!(!rec.socket.exists());
}

#[test]
fn a_lingering_host_stays_while_a_daemon_is_connected() {
    let rig = Rig::new();
    let mut req = rig.request(&["sh", "-c", "exit 5"]);
    req.linger_secs = 1;
    let s = rig.start_with(req);
    eventually("exit known", || rig.record(&s.id).exit.is_some());
    let rec = rig.record(&s.id);
    // Proving a negative needs a wait: twice the linger, still serving.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(rec.host_alive(), "a connected daemon holds a lingering host");
    assert_eq!(s.client.screen().map(|_| ()).ok(), Some(()));
    drop(s);
    eventually("exit once the daemon leaves", || host_dead(&rec));
    assert!(!record_path(rig.path(), &rec.id).exists());
}

/// A daemon that stops reading gets `overrun` instead of an unbounded queue;
/// answers are never dropped, and output after it is newer than what was.
#[test]
fn a_daemon_that_falls_behind_gets_overrun() {
    let rig = Rig::new();
    let done = rig.path().join("burst.done");
    let script = format!(
        "read x; head -c 3000000 /dev/zero | tr '\\0' x; echo; touch '{}'; read y; echo AFTER-$y",
        done.display()
    );
    let mut req = rig.request(&["sh", "-c", &script]);
    req.max_queue_bytes = Some(64 * 1024);
    let (_, spawned) = Rig::spawn(&req);
    let s = raw_hello(&spawned.socket, &req.token);
    write_frame(&mut &s, &ClientMsg::Input, b"\n").unwrap();
    // Not reading while the child prints 3 MB.
    eventually("the burst to finish", || done.exists());
    // A ping queues behind whatever output survived: its pong marks the end
    // of the backlog.
    write_frame(&mut &s, &ClientMsg::Ping { req: 99 }, &[]).unwrap();
    let mut through = None;
    let mut backlog = 0usize;
    loop {
        let f = read_frame(&mut &s).unwrap().unwrap();
        match f.kind() {
            Some("overrun") => through = Some(f.header["through_seq"].as_u64().unwrap()),
            Some("output") => backlog += f.body.len(),
            Some("pong") if f.req() == Some(99) => break,
            _ => {}
        }
    }
    let through = through.expect("an overrun was sent");
    assert!(backlog < 3_000_000, "the queue was bounded: {backlog} bytes delivered");
    write_frame(&mut &s, &ClientMsg::Input, b"go\n").unwrap();
    let mut seqs = Vec::new();
    let mut text = String::new();
    while !text.contains("AFTER-go") {
        let f = read_frame(&mut &s).unwrap().unwrap();
        if f.kind() == Some("output") {
            seqs.push(f.header["seq"].as_u64().unwrap());
            text.push_str(&String::from_utf8_lossy(&f.body));
        }
    }
    assert!(seqs.iter().all(|&q| q > through), "{seqs:?} vs through {through}");
}

#[test]
fn kill_tears_down_the_process_tree() {
    let rig = Rig::new();
    let pidfile = rig.path().join("bg.pid");
    let s = rig.start(&format!("sleep 300 & echo $! > '{}'; sleep 300 & wait", pidfile.display()));
    eventually("the background pid", || {
        std::fs::read_to_string(&pidfile).is_ok_and(|p| p.trim().parse::<u32>().is_ok())
    });
    let bg: u32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    assert!(smooth_flow::proc::start_time(bg).is_some());
    s.client.kill(KillSignal::Term, Duration::from_secs(3)).unwrap();
    let (code, signal, _) = s.events.exit();
    assert_eq!((code, signal), (None, Some(15)));
    eventually("the background sleep to die too", || smooth_flow::proc::start_time(bg).is_none());
}

#[test]
fn kill_escalates_when_term_is_ignored() {
    let rig = Rig::new();
    let s = rig.start("trap '' TERM; echo armed; while :; do sleep 1; done");
    s.wait_screen("armed");
    s.client.kill(KillSignal::Term, Duration::from_millis(300)).unwrap();
    let (_, signal, _) = s.events.exit();
    assert_eq!(signal, Some(9));
}

#[test]
fn a_dead_host_leaves_a_stale_record_that_adoption_removes() {
    let rig = Rig::new();
    let s = rig.start_with(rig.request(&["cat"]));
    let rec = rig.record(&s.id);
    let child = rec.child_pid;
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(i32::try_from(rec.pid).unwrap()), nix::sys::signal::Signal::SIGKILL).unwrap();
    eventually("the host to die", || host_dead(&rec));
    // Its master closed, so the child was hung up.
    eventually("the child to be hung up", || smooth_flow::proc::start_time(child).is_none());
    let found = adopt(rig.path(), OWNER, OFFER, &|_| Arc::new(|_| {}));
    assert!(
        matches!(&found[..], [Found::Stale { record }] if record.id == s.id && record.exit.is_none()),
        "{found:?}"
    );
    assert!(!record_path(rig.path(), &s.id).exists());
    assert!(!rec.socket.exists());
}

#[test]
fn a_wrong_token_is_refused_and_the_host_keeps_serving() {
    let rig = Rig::new();
    let s = rig.start_with(rig.request(&["cat"]));
    let wrong = new_token();
    match HostClient::connect(&s.spawned.socket, &wrong, OFFER, Arc::new(|_| {})) {
        Err(ClientError::Auth(_)) => {}
        other => panic!("{other:?}"),
    }
    // An empty or truncated token, too.
    assert!(matches!(
        HostClient::connect(&s.spawned.socket, "", OFFER, Arc::new(|_| {})),
        Err(ClientError::Auth(_))
    ));
    assert!(matches!(
        HostClient::connect(&s.spawned.socket, &s.req.token[..63], OFFER, Arc::new(|_| {})),
        Err(ClientError::Auth(_))
    ));
    // The refused attempts did not supersede the real daemon.
    s.client.ping().unwrap();
}

#[test]
fn version_negotiation_against_a_live_host() {
    let rig = Rig::new();
    let s = rig.start_with(rig.request(&["cat"]));
    match HostClient::connect(&s.spawned.socket, &s.req.token, (2, 3), Arc::new(|_| {})) {
        Err(ClientError::Version(m)) => assert_eq!(m, "host speaks 1..1"),
        other => panic!("{other:?}"),
    }
    // A daemon from the future that still speaks 1 negotiates down to it.
    let c = HostClient::connect(&s.spawned.socket, &s.req.token, (1, 5), Arc::new(|_| {})).unwrap();
    assert_eq!(c.hello().protocol, 1);
    // Adoption by a daemon that speaks only newer versions leaves it running.
    let found = adopt(rig.path(), OWNER, (2, 3), &|_| Arc::new(|_| {}));
    assert!(
        matches!(&found[..], [Found::Held { reason, .. }] if reason == "host speaks protocol 1; this daemon speaks 2..3"),
        "{found:?}"
    );
    assert!(rig.record(&s.id).host_alive());
    // …and can still end it without the protocol.
    let rec = rig.record(&s.id);
    assert!(kill_host(rig.path(), &rec, Duration::from_secs(2)));
    assert!(!record_path(rig.path(), &s.id).exists());
}

#[test]
#[allow(clippy::many_single_char_names, reason = "a, b: two answers in order")]
fn malformed_frames_are_answered_and_closed() {
    let rig = Rig::new();
    let s = rig.start_with(rig.request(&["cat"]));
    let sock = &s.spawned.socket;

    // An oversized length before the handshake.
    let raw = UnixStream::connect(sock).unwrap();
    raw.set_read_timeout(Some(WAIT)).unwrap();
    (&raw).write_all(&u32::MAX.to_be_bytes()).unwrap();
    let frames = drain(&raw);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].header["code"], code::FRAME_TOO_LARGE);

    // A header that isn't JSON.
    let raw = UnixStream::connect(sock).unwrap();
    raw.set_read_timeout(Some(WAIT)).unwrap();
    let h = b"{nope";
    let mut b = u32::try_from(4 + h.len()).unwrap().to_be_bytes().to_vec();
    b.extend_from_slice(&u32::try_from(h.len()).unwrap().to_be_bytes());
    b.extend_from_slice(h);
    (&raw).write_all(&b).unwrap();
    let frames = drain(&raw);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].header["code"], code::BAD_REQUEST);

    // Anything but hello first.
    let raw = UnixStream::connect(sock).unwrap();
    raw.set_read_timeout(Some(WAIT)).unwrap();
    write_frame(&mut &raw, &ClientMsg::Ping { req: 1 }, &[]).unwrap();
    let frames = drain(&raw);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].header["code"], code::BAD_REQUEST);

    // A truncated frame then EOF: closed without an answer, host unharmed.
    let raw = UnixStream::connect(sock).unwrap();
    (&raw).write_all(&100u32.to_be_bytes()).unwrap();
    raw.shutdown(std::net::Shutdown::Write).unwrap();
    raw.set_read_timeout(Some(WAIT)).unwrap();
    assert!(drain(&raw).is_empty());

    // After the handshake: an unknown type is answered and the connection
    // stays; an oversized frame closes it.
    let raw = raw_hello(sock, &s.req.token);
    write_frame(&mut &raw, &serde_json::json!({"type": "teleport", "req": 4}), &[]).unwrap();
    write_frame(&mut &raw, &ClientMsg::Ping { req: 5 }, &[]).unwrap();
    let a = read_frame(&mut &raw).unwrap().unwrap();
    assert_eq!(a.header["code"], code::UNKNOWN_TYPE);
    assert_eq!(a.req(), Some(4));
    let b = read_frame(&mut &raw).unwrap().unwrap();
    assert_eq!(b.kind(), Some("pong"));
    (&raw).write_all(&u32::MAX.to_be_bytes()).unwrap();
    let frames = drain(&raw);
    assert!(
        frames.iter().any(|f| f.header["code"] == code::FRAME_TOO_LARGE),
        "{:?}",
        frames.iter().map(|f| &f.header).collect::<Vec<_>>()
    );

    // The host survived all of it.
    let c = HostClient::connect(sock, &s.req.token, OFFER, Arc::new(|_| {})).unwrap();
    c.ping().unwrap();
}

#[test]
fn files_are_private() {
    let rig = Rig::new();
    let s = rig.start_with(rig.request(&["cat"]));
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&s.spawned.socket), 0o600, "socket");
    assert_eq!(mode(&record_path(rig.path(), &s.id)), 0o600, "record");
    assert_eq!(mode(rig.path()), 0o700, "dir");
    // The token is in the record, not in the host's argv.
    let rec = rig.record(&s.id);
    assert_eq!(rec.token, s.req.token);
    let ps = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &rec.pid.to_string()])
        .output()
        .unwrap();
    let cmdline = String::from_utf8_lossy(&ps.stdout);
    assert!(!cmdline.contains(&rec.token), "token in argv: {cmdline}");
    assert!(cmdline.contains(&s.id), "{cmdline}");
}

#[test]
fn spawn_failures_report_why() {
    let rig = Rig::new();
    let err = |req: &SpawnRequest, id: &str| spawn_host(&host_cmd(), id, req, READY_TIMEOUT * 4).unwrap_err().to_string();

    let e = err(&rig.request(&["cat"]), "../escape");
    assert!(e.contains("invalid session id"), "{e}");

    let mut bad = rig.request(&["cat"]);
    bad.token = "short".into();
    assert!(err(&bad, &new_id()).contains("token"));

    let mut bad = rig.request(&[]);
    bad.argv.clear();
    assert!(err(&bad, &new_id()).contains("argv"));

    let e = err(&rig.request(&["/nonexistent/agent-binary"]), &new_id());
    assert!(e.contains("nonexistent"), "{e}");

    // A host dir others can read is refused, not fixed.
    let open = rig.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut req = rig.request(&["cat"]);
    req.dir = open;
    assert!(err(&req, &new_id()).contains("world-accessible"));

    // A second host for a live id is refused.
    let req = rig.request(&["cat"]);
    let (id, _) = Rig::spawn(&req);
    let e = err(&rig.request(&["cat"]), &id);
    assert!(e.contains("already running"), "{e}");

    // Nothing failed leaves files behind (only the live host's two).
    let names: Vec<String> = std::fs::read_dir(rig.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("fs-"))
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
}

/// A hosts dir too deep for a Unix socket path moves the socket under
/// `$TMPDIR`; the record says where.
#[test]
fn a_long_host_dir_moves_the_socket_to_tmpdir() {
    let rig = Rig::new();
    let deep: PathBuf = rig.path().join("d".repeat(90));
    let mut req = rig.request(&["cat"]);
    req.dir = deep.clone();
    let id = new_id();
    let spawned = spawn_host(&host_cmd(), &id, &req, READY_TIMEOUT * 4).unwrap();
    assert!(spawned.socket.starts_with(std::env::temp_dir()), "{}", spawned.socket.display());
    let rec = read_record(&record_path(&deep, &id)).unwrap();
    assert_eq!(rec.socket, spawned.socket);
    let c = HostClient::connect(&rec.socket, &req.token, OFFER, Arc::new(|_| {})).unwrap();
    c.ping().unwrap();
    assert!(kill_host(&deep, &rec, Duration::from_secs(2)));
    assert!(!rec.socket.exists());
}
