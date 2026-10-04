//! `PtyHost` (th-dc9822, ADR-011) against REAL session hosts: the
//! test-support `smooth-flow-host` binary, which runs the same
//! `session_host::server::run` as the shipped `smooth-daemon flow-host`.
//!
//! Two layers:
//! - the `SessionHost` methods one by one (launch, capture, keys under
//!   DECCKM, bracketed paste only under mode 2004, meta, attach stream,
//!   exit status, tree kill, adoption by a second `PtyHost`);
//! - the engine on top of it (launch, send, scrape → needs you → approve
//!   keys, exact exit codes, crash resume with `seq` continuing, kill, and a
//!   daemon restart that adopts the running session).
//!
//! Every host lives in a temp dir and is killed at the end, so nothing here
//! touches `~/.smooth`. Waits poll with deadlines (this box runs at load 20+).

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "unwrap/expect are the idiom for test assertions; a scenario is one long test on purpose"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smooth_flow::host::{Adopted, Launch, PaneDeath, SessionHost, SessionRef};
use smooth_flow::session_host::client::HostCommand;
use smooth_flow::session_host::record;
use smooth_flow::store::SessionState;
use smooth_flow::{Engine, EngineConfig, HostKind, NewRequest, PtyHost, ReplayReason, ServerFrame};

const WAIT: Duration = Duration::from_secs(30);

/// One `(seq, bytes)` an attach stream delivered.
type Chunk = (u64, Vec<u8>);
const OWNER: &str = "pty-host-test";

fn cmd() -> HostCommand {
    HostCommand {
        program: PathBuf::from(env!("CARGO_BIN_EXE_smooth-flow-host")),
        args: Vec::new(),
    }
}

/// A host dir short enough for a Unix socket path on macOS.
/// Private (0700), as the host requires of its dir.
fn host_dir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let d = tempfile::Builder::new().prefix("ph").tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}

fn host(dir: &Path) -> PtyHost {
    PtyHost::new(OWNER, dir, cmd())
}

/// Kills every host whose record is in the dir when dropped.
struct Reaper(PathBuf);

impl Drop for Reaper {
    fn drop(&mut self) {
        let Ok(entries) = std::fs::read_dir(&self.0) else { return };
        for p in entries.flatten().map(|e| e.path()) {
            if p.extension().is_some_and(|x| x == "json") {
                if let Ok(rec) = record::read_record(&p) {
                    smooth_flow::session_host::adopt::kill_host(&self.0, &rec, Duration::from_secs(1));
                }
            }
        }
    }
}

fn sref(id: &str) -> SessionRef {
    SessionRef::new(OWNER, id)
}

fn launch(h: &PtyHost, id: &str, script: &str) -> u32 {
    let cwd = std::env::temp_dir();
    h.launch(
        &sref(id),
        &Launch {
            cwd: &cwd,
            argv: &["sh".to_string(), "-c".to_string(), script.to_string()],
            env: &[("PATH".to_string(), "/usr/bin:/bin:/usr/sbin:/sbin".to_string())],
            exit_prefix: None,
        },
    )
    .unwrap()
}

fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn screen_has(h: &PtyHost, id: &str, needle: &str) -> String {
    until(&format!("`{needle}` on {id}'s screen"), || {
        h.capture_visible(&sref(id)).ok().filter(|t| t.contains(needle))
    })
}

fn exit_of(h: &PtyHost, id: &str) -> PaneDeath {
    until(&format!("{id}'s exit"), || h.exit_status(&sref(id)).ok().flatten())
}

/// A program that reads `n` raw bytes and prints them with `od -c` — what a
/// key or paste really put on the wire.
fn raw_reader(prelude: &str, n: usize) -> String {
    format!("stty raw -echo; {prelude} printf 'READY\\n'; dd bs=1 count={n} 2>/dev/null | od -An -c | tr -s ' '; printf 'DONE\\n'; sleep 30")
}

#[test]
fn launch_capture_send_and_an_exact_exit_code() {
    let dir = host_dir();
    let _r = Reaper(dir.path().to_path_buf());
    let h = host(dir.path());
    let id = "fs-00000a01";
    let pid = launch(&h, id, "printf 'ready> '; read x; echo got=$x; exit 3");
    assert!(h.alive(&sref(id)));
    assert_eq!(h.pid(&sref(id)).unwrap(), pid, "the pid is the agent's own, not a wrapper's");
    assert_eq!(h.kind(), HostKind::Pty);
    screen_has(&h, id, "ready>");
    assert_eq!(h.size(&sref(id)).unwrap(), (120, 40));
    h.send_text(&sref(id), "hello").unwrap();
    screen_has(&h, id, "got=hello");
    assert_eq!(exit_of(&h, id), PaneDeath::Code(3), "exact, from the host's wait");
    // A dead session stays readable (tmux's remain-on-exit) until killed.
    assert!(h.alive(&sref(id)));
    assert!(h.capture_visible(&sref(id)).unwrap().contains("got=hello"), "the final screen");
    assert!(h.capture_scrollback(&sref(id)).unwrap().contains("ready> hello"));
    h.kill_session(&sref(id));
    assert!(!h.alive(&sref(id)));
    assert!(!record::record_path(dir.path(), id).exists(), "released: the host removed its record");
}

#[test]
fn keys_follow_the_programs_cursor_key_mode_and_unknown_names_type_literally() {
    let dir = host_dir();
    let _r = Reaper(dir.path().to_path_buf());
    let h = host(dir.path());
    // Normal cursor keys: Up is CSI A.
    launch(&h, "fs-00000b01", &raw_reader("", 3));
    screen_has(&h, "fs-00000b01", "READY");
    h.send_key(&sref("fs-00000b01"), "Up").unwrap();
    let s = screen_has(&h, "fs-00000b01", "DONE");
    assert!(s.contains("033 [ A"), "CSI A:\n{s}");
    // DECCKM on (`ESC[?1h`): Up is SS3 A, as a real terminal sends it.
    launch(&h, "fs-00000b02", &raw_reader("printf '\\033[?1h';", 3));
    screen_has(&h, "fs-00000b02", "READY");
    h.send_key(&sref("fs-00000b02"), "Up").unwrap();
    let s = screen_has(&h, "fs-00000b02", "DONE");
    assert!(s.contains("033 O A"), "SS3 A under DECCKM:\n{s}");
    // C-c is the C0 byte; a name tmux wouldn't know is typed as text.
    launch(&h, "fs-00000b03", &raw_reader("", 4));
    screen_has(&h, "fs-00000b03", "READY");
    h.send_key(&sref("fs-00000b03"), "C-c").unwrap();
    h.send_key(&sref("fs-00000b03"), "abc").unwrap();
    let s = screen_has(&h, "fs-00000b03", "DONE");
    assert!(s.contains("003 a b c"), "^C then the literal text:\n{s}");
}

#[test]
fn a_paste_is_bracketed_only_when_the_program_asked() {
    let dir = host_dir();
    let _r = Reaper(dir.path().to_path_buf());
    let h = host(dir.path());
    launch(&h, "fs-00000c01", &raw_reader("", 2));
    screen_has(&h, "fs-00000c01", "READY");
    h.paste(&sref("fs-00000c01"), "hi").unwrap();
    let s = screen_has(&h, "fs-00000c01", "DONE");
    assert!(s.contains(" h i") && !s.contains("033"), "plain bytes:\n{s}");
    launch(&h, "fs-00000c02", &raw_reader("printf '\\033[?2004h';", 14));
    screen_has(&h, "fs-00000c02", "READY");
    h.paste(&sref("fs-00000c02"), "hi").unwrap();
    let s = screen_has(&h, "fs-00000c02", "DONE");
    assert!(s.contains("033 [ 2 0 0 ~ h i 033 [ 2 0 1 ~"), "bracketed under 2004:\n{s}");
}

#[test]
fn meta_reads_the_title_alternate_screen_and_cursor_from_the_vt() {
    let dir = host_dir();
    let _r = Reaper(dir.path().to_path_buf());
    let h = host(dir.path());
    let id = "fs-00000d01";
    launch(&h, id, "printf '\\033]2;flow-title\\007\\033[?1049h\\033[5;1HALT'; sleep 30");
    screen_has(&h, id, "ALT");
    let m = h.meta(&sref(id)).unwrap();
    assert_eq!(m.title, "flow-title");
    assert!(m.alternate_on);
    assert_eq!(m.cursor_y, Some(4), "row 5, zero-based");
}

#[test]
fn an_attach_streams_sequenced_output_resizes_and_snapshots() {
    let dir = host_dir();
    let _r = Reaper(dir.path().to_path_buf());
    let h = host(dir.path());
    let id = "fs-00000e01";
    launch(&h, id, "while read l; do echo \"echo:$l\"; done");
    let got: Arc<Mutex<Vec<Chunk>>> = Arc::default();
    let sink = got.clone();
    let stream = h
        .attach(&sref(id), 100, 30, Arc::new(move |seq, bytes| sink.lock().unwrap().push((seq, bytes))))
        .unwrap();
    assert_eq!(h.size(&sref(id)).unwrap(), (100, 30), "attach sizes the session");
    stream.write(b"one\r").unwrap();
    stream.write(b"two\r").unwrap();
    until("both echoes streamed", || {
        let all: Vec<u8> = got.lock().unwrap().iter().flat_map(|(_, b)| b.clone()).collect();
        String::from_utf8_lossy(&all).contains("echo:two").then_some(())
    });
    let seqs: Vec<u64> = got.lock().unwrap().iter().map(|(s, _)| *s).collect();
    assert!(seqs.windows(2).all(|w| w[1] > w[0]), "seq rises per output: {seqs:?}");
    stream.resize(90, 20).unwrap();
    assert_eq!(h.size(&sref(id)).unwrap(), (90, 20));
    let snap = h.snapshot(&sref(id), 1 << 20).unwrap().expect("the pty host has snapshots");
    assert_eq!((snap.cols, snap.rows), (90, 20));
    assert!(snap.seq >= *seqs.last().unwrap(), "current through the last output");
    let mut vt = smooth_flow::vt::Vt::new(snap.cols, snap.rows, 1000).unwrap();
    vt.feed(&snap.data);
    assert!(vt.plain_screen().contains("echo:one"), "the snapshot rebuilds the screen");
    stream.close();
    assert!(stream.is_closed());
    let before = got.lock().unwrap().len();
    h.send_text(&sref(id), "three").unwrap();
    screen_has(&h, id, "echo:three");
    assert_eq!(got.lock().unwrap().len(), before, "a closed stream gets nothing");
}

#[test]
fn a_tree_kill_ends_the_childs_group_with_the_signal() {
    let dir = host_dir();
    let _r = Reaper(dir.path().to_path_buf());
    let h = host(dir.path());
    let id = "fs-00000f01";
    let pid = launch(&h, id, "sleep 300 & sleep 300; wait");
    assert!(h.process_alive(pid, h.process_start(pid)));
    h.kill_process_tree(pid, Duration::from_secs(2));
    assert_eq!(h.exit_status(&sref(id)).unwrap(), Some(PaneDeath::Signal(15)), "SIGTERM, exact");
    assert!(!h.process_alive(pid, None));
    h.kill_session(&sref(id));
}

/// A daemon restart: the first `PtyHost` goes away (its connections close),
/// the hosts keep running, and a second one adopts them — screen, input,
/// and an exit that happened while nobody was connected.
#[test]
fn a_second_pty_host_adopts_what_the_first_left_running() {
    let dir = host_dir();
    let _r = Reaper(dir.path().to_path_buf());
    let (live, ends) = ("fs-00001a01", "fs-00001a02");
    let seq_before;
    {
        let h = host(dir.path());
        launch(&h, live, "while read l; do echo \"echo:$l\"; done");
        launch(&h, ends, "while [ ! -f go-fs-00001a02 ]; do sleep 0.1; done; exit 4");
        h.send_text(&sref(live), "before").unwrap();
        screen_has(&h, live, "echo:before");
        seq_before = h.snapshot(&sref(live), 1 << 20).unwrap().unwrap().seq;
    }
    // While no daemon is connected, one child exits.
    let go = std::env::temp_dir().join("go-fs-00001a02");
    std::fs::write(&go, b"").unwrap();
    until("the exit recorded", || {
        record::read_record(&record::record_path(dir.path(), ends)).ok()?.exit.map(|_| ())
    });
    let _ = std::fs::remove_file(&go);

    let h2 = host(dir.path());
    let mut found = h2.adopt_existing();
    found.sort_by(|a, b| a.name().cmp(b.name()));
    assert_eq!(
        found,
        vec![Adopted::Live { name: live.into() }, Adopted::Live { name: ends.into() }],
        "both hosts were still there"
    );
    assert!(h2.capture_visible(&sref(live)).unwrap().contains("echo:before"), "same screen");
    h2.send_text(&sref(live), "after").unwrap();
    screen_has(&h2, live, "echo:after");
    assert!(h2.snapshot(&sref(live), 1 << 20).unwrap().unwrap().seq > seq_before, "seq carried on");
    assert_eq!(h2.exit_status(&sref(ends)).unwrap(), Some(PaneDeath::Code(4)), "settled from the host, exactly");

    // Another daemon's hosts are not ours, and a dead host settles unknown.
    let other = PtyHost::new("someone-else", dir.path(), cmd());
    assert!(other.adopt_existing().is_empty());
    let rec = record::read_record(&record::record_path(dir.path(), live)).unwrap();
    smooth_flow::session_host::adopt::kill_host(dir.path(), &rec, Duration::from_secs(1));
    let h3 = host(dir.path());
    let found = h3.adopt_existing();
    assert!(found.iter().all(|f| f.name() != live), "a killed host's files are gone: {found:?}");
    for s in [live, ends] {
        h2.kill_session(&sref(s));
    }
}

/// A host whose process died hard (SIGKILL) leaves its record behind: the
/// next adoption settles the session as exit status unknown.
#[test]
fn a_host_that_died_without_an_exit_settles_unknown() {
    let dir = host_dir();
    let _r = Reaper(dir.path().to_path_buf());
    let id = "fs-00001b01";
    let host_pid = {
        let h = host(dir.path());
        launch(&h, id, "sleep 300");
        record::read_record(&record::record_path(dir.path(), id)).unwrap().pid
    };
    let _ = std::process::Command::new("kill").args(["-KILL", &host_pid.to_string()]).status();
    until("the host gone", || (!smooth_flow::proc::is_alive(host_pid, None)).then_some(()));
    let h = host(dir.path());
    let found = h.adopt_existing();
    assert_eq!(
        found,
        vec![Adopted::Settled {
            name: id.into(),
            death: PaneDeath::Unknown
        }]
    );
    assert_eq!(h.exit_status(&sref(id)).unwrap(), Some(PaneDeath::Unknown));
    h.kill_session(&sref(id));
}

// ── the engine on the pty host ─────────────────────────────────────────────

struct World {
    _root: tempfile::TempDir,
    _reaper: Reaper,
    hosts: PathBuf,
    home: PathBuf,
    ws: PathBuf,
}

impl World {
    fn new() -> Self {
        let root = tempfile::Builder::new().prefix("pe").tempdir_in("/tmp").unwrap();
        let hosts = root.path().join("h");
        let home = root.path().join("home");
        let ws = root.path().join("ws");
        std::fs::create_dir_all(home.join(".smooth/harnesses")).unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        Self {
            _reaper: Reaper(hosts.clone()),
            _root: root,
            hosts,
            home,
            ws,
        }
    }

    fn engine(&self) -> Engine {
        Engine::open(EngineConfig {
            db_path: self.home.join(".smooth/flow.db"),
            default_project: self.ws.clone(),
            version: "t".into(),
            machine_label: "m".into(),
            home: self.home.clone(),
            daemon_url: None,
            harness_doctor: false,
            repo_root: None,
            host: smooth_flow::host::default_host(),
            pty_host: Some(Arc::new(PtyHost::new(OWNER, &self.hosts, cmd()))),
            new_host: HostKind::Pty,
        })
        .unwrap()
    }

    /// A scraped harness `name` running `script`; a resume runs it again.
    fn manifest(&self, name: &str, script: &str) {
        let path = self.home.join(format!("{name}.sh"));
        std::fs::write(&path, script).unwrap();
        std::fs::write(
            self.home.join(".smooth/harnesses").join(format!("{name}.toml")),
            format!(
                r#"name = "{name}"
[binary]
names = ["sh"]
[launch]
argv = ["{script}"]
prompt_as = "paste"
[resume]
argv = []
mode = "relaunch_command"
[state]
source = "scrape"
[[state.scrape.rules]]
name = "question"
state = "needs_you"
match = ['Allow\? \(y/n\)\s*$']
where = "last_line"
[[state.scrape.rules]]
name = "composer"
state = "idle"
match = ['^>\s*$']
where = "last_line"
quiet_ms = 200
[steer]
method = "bracketed_paste"
submit_key = "Enter"
approve_keys = ["y", "Enter"]
deny_keys = ["n", "Enter"]
"#,
                script = path.display()
            ),
        )
        .unwrap();
    }
}

fn wait_state(e: &Engine, id: &str, want: SessionState) -> smooth_flow::Session {
    until(&format!("{id} {want:?}"), || {
        e.supervise_tick().unwrap();
        e.get(id).unwrap().filter(|s| s.state == want)
    })
}

fn screen(e: &Engine, id: &str) -> String {
    match e.snapshot(id).unwrap() {
        ServerFrame::Screen { text, .. } => text,
        other => panic!("{other:?}"),
    }
}

fn wait_screen(e: &Engine, id: &str, needle: &str) -> String {
    until(&format!("`{needle}` on {id}"), || {
        let t = screen(e, id);
        t.contains(needle).then_some(t)
    })
}

fn replay_seq(e: &Engine, id: &str) -> u64 {
    match e.replay(id, None, ReplayReason::Attach).unwrap() {
        Some(ServerFrame::Replay { seq, .. }) => seq,
        other => panic!("{other:?}"),
    }
}

#[test]
fn engine_runs_a_shell_on_the_pty_host_with_exact_exits() {
    let w = World::new();
    let e = w.engine();
    assert_eq!(e.new_session_host(), HostKind::Pty);
    let s = e
        .new_session(NewRequest {
            argv: Some(vec!["sh".into()]),
            worktree: Some(w.ws.to_string_lossy().into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(s.host, HostKind::Pty, "the row records its host");
    assert!(record::record_path(&w.hosts, &s.id).exists(), "a session host, not tmux");
    wait_state(&e, &s.id, SessionState::Idle);
    e.send(&s.id, "echo hi-$((40+2))").unwrap();
    wait_screen(&e, &s.id, "hi-42");
    e.send(&s.id, "exit 7").unwrap();
    let dead = wait_state(&e, &s.id, SessionState::Dead);
    assert_eq!(dead.exit_code, Some(7), "exact, no wrapper");
    assert!(dead.attention.unwrap().detail.unwrap().contains("exit 7"));
    until("the host released", || (!record::record_path(&w.hosts, &s.id).exists()).then_some(()));

    // Kill: SIGTERM through the host, done, host released.
    let k = e
        .new_session(NewRequest {
            argv: Some(vec!["sh".into(), "-c".into(), "sleep 300".into()]),
            worktree: Some(w.ws.to_string_lossy().into()),
            ..Default::default()
        })
        .unwrap();
    let pid = k.pid.unwrap();
    let done = e.kill(&k.id, false).unwrap();
    assert_eq!(done.state, SessionState::Done);
    assert!(!smooth_flow::proc::is_alive(pid, None));
    assert!(!record::record_path(&w.hosts, &k.id).exists());
}

#[test]
fn engine_scrapes_the_vt_and_approves_with_the_manifests_keys() {
    let w = World::new();
    w.manifest(
        "askpty",
        "printf 'Allow? (y/n) '; read a; echo \"ANSWER=[$a]\"; while :; do printf '> '; read p; echo \"PROMPT=[$p]\"; done\n",
    );
    let e = w.engine();
    let s = e
        .new_session(NewRequest {
            kind: "askpty".parse().unwrap(),
            worktree: Some(w.ws.to_string_lossy().into()),
            ..Default::default()
        })
        .unwrap();
    let asking = wait_state(&e, &s.id, SessionState::NeedsYou);
    let att = asking.attention.unwrap();
    assert_eq!(att.reason, "permission");
    e.approve(&s.id, att.request_id.as_deref().unwrap(), smooth_flow::Decision::Allow).unwrap();
    wait_screen(&e, &s.id, "ANSWER=[y]");
    wait_state(&e, &s.id, SessionState::Idle);
    e.send(&s.id, "go on").unwrap();
    wait_screen(&e, &s.id, "PROMPT=[go on]");
    let _ = e.kill(&s.id, false);
}

#[test]
fn engine_resumes_a_crashed_agent_on_a_new_host_and_seq_continues() {
    let w = World::new();
    w.manifest(
        "crashpty",
        "if [ -f started ]; then echo RESUMED; else touch started; echo FIRST; sleep 0.5; exit 3; fi; while :; do printf '> '; read p; done\n",
    );
    let e = w.engine();
    let s = e
        .new_session(NewRequest {
            kind: "crashpty".parse().unwrap(),
            worktree: Some(w.ws.to_string_lossy().into()),
            ..Default::default()
        })
        .unwrap();
    wait_screen(&e, &s.id, "FIRST");
    let first_seq = replay_seq(&e, &s.id);
    let first_pid = s.pid.unwrap();
    // exit 3 → crashed → resume after the backoff, on the pty host again.
    let resumed = until("the resume", || {
        e.supervise_tick().unwrap();
        e.get(&s.id).unwrap().filter(|r| r.pid.is_some_and(|p| p != first_pid))
    });
    assert_eq!(resumed.host, HostKind::Pty);
    wait_screen(&e, &s.id, "RESUMED");
    assert!(replay_seq(&e, &s.id) > first_seq, "the new host's seq continues the old one's");
    let _ = e.kill(&s.id, false);
}

#[test]
fn a_restarted_engine_adopts_its_running_pty_sessions() {
    let w = World::new();
    let id = {
        let e = w.engine();
        let s = e
            .new_session(NewRequest {
                argv: Some(vec!["sh".into()]),
                worktree: Some(w.ws.to_string_lossy().into()),
                ..Default::default()
            })
            .unwrap();
        wait_state(&e, &s.id, SessionState::Idle);
        e.send(&s.id, "echo before-restart").unwrap();
        wait_screen(&e, &s.id, "before-restart");
        s.id
    };
    // The engine (and its PtyHost) is gone; the host is not.
    let e2 = w.engine();
    let row = e2.get(&id).unwrap().unwrap();
    assert_eq!(row.state, SessionState::Idle);
    for _ in 0..3 {
        e2.supervise_tick().unwrap();
    }
    assert_eq!(e2.get(&id).unwrap().unwrap().state, SessionState::Idle, "adopted, not declared dead");
    assert!(screen(&e2, &id).contains("before-restart"));
    e2.send(&id, "echo after-restart").unwrap();
    wait_screen(&e2, &id, "after-restart");
    // A host with no row in this flow.db is left running, not killed.
    let other = World::new();
    let stranger = {
        let h = PtyHost::new(OWNER, &w.hosts, cmd());
        launch(&h, "fs-00002c01", "sleep 300");
        record::read_record(&record::record_path(&w.hosts, "fs-00002c01")).unwrap().pid
    };
    drop(other);
    let e3 = w.engine();
    assert!(smooth_flow::proc::is_alive(stranger, None), "an orphan host is left alone");
    assert!(e3.get("fs-00002c01").unwrap().is_none());
    let _ = e3.kill(&id, false);
}
