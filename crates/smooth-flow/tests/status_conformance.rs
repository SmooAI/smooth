//! What a row says against what the harness on screen is doing (SMOODEV-4268),
//! for Claude Code, Codex and OpenCode, with and without their hooks, on both
//! hosts.
//!
//! The screens under `tests/status_conformance/` were captured from the real
//! harnesses by `tests/status_live/rig.py`, which is also how to re-verify them
//! against a new release. Here a small script stands in for the binary the
//! manifest launches and paints whichever screen a test chooses, and the hook
//! events go straight into the engine, including the ones that real harnesses
//! never send: pressing Esc fires no `Stop`, and denying a Codex prompt fires
//! nothing. Those silences are most of what this file exists to test.
//!
//! Every wait polls with a deadline: this machine runs at load 20+.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "unwrap/expect are the idiom for test assertions; a scenario is one long test on purpose"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smooth_flow::harness::Registry;
use smooth_flow::status::{Confidence, StatusWindows};
use smooth_flow::store::Session;
use smooth_flow::{tmux, Engine, EngineConfig, HookCaller, HookEvent, HookReply, HostKind, NewRequest, ServerFrame, SessionHost, SessionState};

const WAIT: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(200);
/// Longer than `status::HOOK_GRACE` + `status::QUIET` by a margin, so a row
/// that was going to move has had its chance.
const HOLD: Duration = Duration::from_secs(7);

/// The binary a manifest launches: paints `$CTRL/screen`, plus a line that
/// keeps changing with `$CTRL/moving`, or a turning spinner with
/// `$CTRL/spinner`. `$CTRL/exit` holds an exit code to leave with.
const FAKE: &str = r#"#!/bin/sh
echo "$$" >"$CTRL/pid"
echo "$SMOOTH_FLOW_HOOK_TOKEN_FILE" >"$CTRL/token-file"
stty -echo 2>/dev/null
last=""
n=0
while :; do
    if [ -f "$CTRL/exit" ]; then exit "$(cat "$CTRL/exit")"; fi
    n=$((n + 1))
    extra=""
    if [ -f "$CTRL/moving" ]; then extra="
  $n"; fi
    if [ -f "$CTRL/spinner" ]; then
        case $((n % 2)) in
            0) extra=" ⠦" ;;
            *) extra=" ⠇" ;;
        esac
    fi
    now="$(cat "$CTRL/screen" 2>/dev/null)$extra"
    if [ "$now" != "$last" ]; then
        printf '\033[2J\033[H%s\n' "$now"
        last="$now"
    fi
    sleep 0.2
done
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    Tmux,
    #[cfg(feature = "pty-host")]
    Pty,
}

/// How the harness tells the engine what it is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wiring {
    Hooks,
    /// Before `th harness enable`, or hooks Codex has not been told to trust.
    ScrapeOnly,
}

/// One harness's real screens, and how its hooks report an ask.
struct Harness {
    name: &'static str,
    /// First-run dialogs that must be answered in the pane.
    dialogs: &'static [&'static str],
    working: &'static str,
    idle: &'static str,
    approval: &'static str,
    question: Option<&'static str>,
    /// A hook held open until `flow.approve`, or answered at once.
    holds_permission: bool,
    /// The event and payload its hooks report a question with.
    question_hook: Option<(&'static str, &'static str)>,
    /// `working` when a stopped turn leaves a spinner turning.
    idle_spins: bool,
    /// Its session id comes only from a hook: without hooks a crash cannot be
    /// resumed, since starting it again would send its prompt again.
    id_from_hooks: bool,
}

const CLAUDE: Harness = Harness {
    name: "claude",
    dialogs: &[include_str!("status_conformance/claude/trust.txt")],
    working: include_str!("status_conformance/claude/working.txt"),
    idle: include_str!("status_conformance/claude/interrupted.txt"),
    approval: include_str!("status_conformance/claude/approval.txt"),
    question: Some(include_str!("status_conformance/claude/question.txt")),
    holds_permission: true,
    question_hook: Some((
        "PermissionRequest",
        r#"{"tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which color?"}]}}"#,
    )),
    idle_spins: false,
    id_from_hooks: false,
};

const CODEX: Harness = Harness {
    name: "codex",
    dialogs: &[
        include_str!("status_conformance/codex/trust.txt"),
        include_str!("status_conformance/codex/hooks_review.txt"),
    ],
    working: include_str!("status_conformance/codex/working.txt"),
    idle: include_str!("status_conformance/codex/interrupted.txt"),
    approval: include_str!("status_conformance/codex/approval.txt"),
    // `request_user_input` is only offered in plan mode; not captured.
    question: None,
    holds_permission: false,
    question_hook: None,
    idle_spins: true,
    id_from_hooks: true,
};

const OPENCODE: Harness = Harness {
    name: "opencode",
    dialogs: &[],
    working: include_str!("status_conformance/opencode/working.txt"),
    idle: include_str!("status_conformance/opencode/interrupted.txt"),
    approval: include_str!("status_conformance/opencode/permission.txt"),
    question: Some(include_str!("status_conformance/opencode/question.txt")),
    holds_permission: false,
    question_hook: Some(("Notification", r#"{"notification_type":"question","message":"Which color?"}"#)),
    idle_spins: false,
    id_from_hooks: true,
};

struct Rig {
    engine: Engine,
    frames: tokio::sync::broadcast::Receiver<ServerFrame>,
    socket: String,
    ctrl: PathBuf,
    ws: PathBuf,
    id: String,
    harness: &'static str,
    #[cfg(feature = "pty-host")]
    hosts: Option<PathBuf>,
    _scratch: tempfile::TempDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::write(self.ctrl.join("exit"), "0");
        tmux::kill_server(&self.socket);
        #[cfg(feature = "pty-host")]
        if let Some(hosts) = &self.hosts {
            for p in std::fs::read_dir(hosts).into_iter().flatten().flatten().map(|e| e.path()) {
                if p.extension().is_some_and(|x| x == "json") {
                    if let Ok(rec) = smooth_flow::session_host::record::read_record(&p) {
                        smooth_flow::session_host::adopt::kill_host(hosts, &rec, Duration::from_secs(1));
                    }
                }
            }
        }
    }
}

fn install_fake(path: &Path, ctrl: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, FAKE.replacen("#!/bin/sh\n", &format!("#!/bin/sh\nCTRL='{}'\n", ctrl.display()), 1)).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Rig {
    /// `name` launched on `host`, painting nothing yet.
    fn launch(name: &'static str, host: Host, windows: StatusWindows) -> Self {
        // Short and under /tmp: a session host's Unix socket lives in here.
        let scratch = tempfile::Builder::new().prefix("sc").tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = std::fs::canonicalize(scratch.path()).unwrap();
        let (home, ws, ctrl, bin) = (root.join("home"), root.join("ws"), root.join("ctrl"), root.join("bin"));
        for d in [&home, &ws, &ctrl, &bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        let _ = std::process::Command::new("git").args(["init", "-q"]).current_dir(&ws).output();
        let registry = Registry::builtin();
        let manifest = registry.get(name).unwrap();
        let target = manifest
            .binary
            .prefer_paths
            .first()
            .map_or_else(|| bin.join(&manifest.binary.names[0]), |rel| home.join(rel));
        install_fake(&target, &ctrl);

        let socket = format!("smooth-status-{}-{}", std::process::id(), &uuid::Uuid::new_v4().simple().to_string()[..8]);
        #[cfg_attr(not(feature = "pty-host"), allow(unused_mut))]
        let mut cfg = EngineConfig {
            db_path: root.join("flow.db"),
            version: "status".into(),
            machine_label: "status".into(),
            home: home.clone(),
            daemon_url: None,
            new_host: HostKind::Tmux,
            status_windows: windows,
            ..EngineConfig::new(ws.clone())
        };
        #[cfg(feature = "pty-host")]
        let hosts = (host == Host::Pty).then(|| {
            let dir = root.join("h");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let cmd = smooth_flow::session_host::client::HostCommand {
                program: PathBuf::from(env!("CARGO_BIN_EXE_smooth-flow-host")),
                args: Vec::new(),
            };
            let pty: Arc<dyn SessionHost> = Arc::new(smooth_flow::PtyHost::new("status-test", &dir, cmd));
            cfg.pty_host = Some(pty);
            cfg.new_host = HostKind::Pty;
            dir
        });
        #[cfg(not(feature = "pty-host"))]
        {
            let _: (Host, Option<Arc<dyn SessionHost>>) = (host, None);
        }
        let engine = Engine::open(cfg).unwrap();
        engine.set_resolve_env(home, std::env::join_paths([&bin, &PathBuf::from("/usr/bin"), &PathBuf::from("/bin")]).unwrap());
        let frames = engine.subscribe();
        let session = engine
            .new_session(NewRequest {
                kind: name.parse().unwrap(),
                worktree: Some(ws.to_string_lossy().into_owned()),
                tmux_socket: Some(socket.clone()),
                title: Some("status".into()),
                ..NewRequest::default()
            })
            .unwrap();
        Self {
            engine,
            frames,
            socket,
            ctrl,
            ws,
            id: session.id,
            harness: name,
            #[cfg(feature = "pty-host")]
            hosts,
            _scratch: scratch,
        }
    }

    fn row(&self) -> Session {
        self.engine.get(&self.id).unwrap().unwrap()
    }

    /// Paint `screen`, holding still.
    fn paint(&self, screen: &str) {
        self.flag("moving", false);
        self.flag("spinner", false);
        self.write_screen(screen);
    }

    /// Paint `screen` with a line that keeps changing, as a busy harness does.
    fn paint_moving(&self, screen: &str) {
        self.flag("spinner", false);
        self.write_screen(screen);
        self.flag("moving", true);
    }

    /// Paint `screen` with only a spinner turning.
    fn paint_spinning(&self, screen: &str) {
        self.flag("moving", false);
        self.write_screen(screen);
        self.flag("spinner", true);
    }

    fn write_screen(&self, screen: &str) {
        let tmp = self.ctrl.join("screen.tmp");
        std::fs::write(&tmp, screen).unwrap();
        std::fs::rename(&tmp, self.ctrl.join("screen")).unwrap();
    }

    fn flag(&self, name: &str, on: bool) {
        let p = self.ctrl.join(name);
        if on {
            std::fs::write(p, "").unwrap();
        } else {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Post one hook event the way `th flow hook` does.
    fn hook(&self, event: &str, payload: &Value) -> HookReply {
        let start = Instant::now();
        let token = loop {
            let file = std::fs::read_to_string(self.ctrl.join("token-file")).unwrap_or_default();
            let token = std::fs::read_to_string(file.trim()).unwrap_or_default();
            if !token.trim().is_empty() {
                break token.trim().to_string();
            }
            assert!(start.elapsed() < WAIT, "{}: the launch never got a hook token", self.harness);
            std::thread::sleep(POLL);
        };
        let row = self.row();
        let ev: HookEvent = serde_json::from_value(json!({
            "harness": self.harness,
            "event": event,
            "session_id": row.agent_session_id.unwrap_or_else(|| format!("learned-{}", self.id)),
            "cwd": self.ws,
            "payload": payload,
        }))
        .unwrap();
        self.engine
            .hook(
                ev,
                &HookCaller {
                    token: Some(token),
                    direct: true,
                },
            )
            .unwrap()
    }

    fn tick(&self) {
        self.engine.supervise_tick().unwrap();
    }

    fn pane(&self) -> String {
        match self.engine.snapshot(&self.id) {
            Ok(ServerFrame::Screen { text, .. }) => text,
            _ => "<no pane>".into(),
        }
    }

    /// Supervise until the row is in `want`, or fail saying what it read.
    fn wait(&self, want: SessionState, what: &str) -> Session {
        let start = Instant::now();
        loop {
            self.tick();
            let s = self.row();
            if s.state == want {
                return s;
            }
            assert!(
                start.elapsed() < WAIT,
                "{}: {what}: wanted `{want}`, row stayed `{}` ({:?}, source {}); pane:\n{}",
                self.harness,
                s.state,
                s.attention,
                s.state_source,
                self.pane()
            );
            std::thread::sleep(POLL);
        }
    }

    /// The row must not move off `want` however long the pane reads this way.
    fn holds(&self, want: SessionState, what: &str) -> Session {
        let start = Instant::now();
        loop {
            self.tick();
            let s = self.row();
            assert_eq!(s.state, want, "{}: {what}: {:?}; pane:\n{}", self.harness, s.attention, self.pane());
            if start.elapsed() > HOLD {
                return s;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Wait until `screen`'s text has actually reached the pane.
    fn wait_painted(&self, screen: &str) {
        let needle = screen.lines().rev().find(|l| l.trim().len() > 8).unwrap_or("").trim().to_string();
        let needle: String = needle.chars().take(30).collect();
        let start = Instant::now();
        while !self.pane().contains(&needle) {
            assert!(start.elapsed() < WAIT, "{}: `{needle}` never reached the pane:\n{}", self.harness, self.pane());
            std::thread::sleep(POLL);
        }
    }

    fn fake_pid(&self) -> i32 {
        std::fs::read_to_string(self.ctrl.join("pid")).unwrap().trim().parse().unwrap()
    }
}

fn scenario(h: &Harness, wiring: Wiring, host: Host) {
    let hooks = wiring == Wiring::Hooks;
    let r = Rig::launch(h.name, host, StatusWindows::default());

    // starting: nothing on screen is no evidence of anything.
    let s = r.holds(SessionState::Starting, "a blank pane");
    assert_eq!(s.confidence, Confidence::Confirmed, "still within the start window");

    // First-run dialogs wait on a person, and never as a one-tap approval: the
    // first option of Claude Code's trust dialog is "No, exit".
    for dialog in h.dialogs {
        r.paint(dialog);
        let s = r.wait(SessionState::NeedsYou, "a first-run dialog");
        let a = s.attention.as_ref().unwrap();
        assert_eq!(a.reason, "question", "{dialog}");
        assert_eq!(a.request_id, None, "a dialog is not approvable");
        assert!(a.since.is_some(), "an ask knows when it began");
        r.holds(SessionState::NeedsYou, "an unanswered dialog");
    }

    // Answered, or none: the composer is up.
    if hooks {
        let _ = r.hook("SessionStart", &json!({}));
    }
    r.paint(h.idle);
    r.wait(SessionState::Idle, "the composer is up");

    let start_turn = |r: &Rig| {
        if hooks {
            let _ = r.hook("UserPromptSubmit", &json!({"prompt": "go"}));
        }
        r.paint_moving(h.working);
        r.wait(SessionState::Working, "a turn started");
    };
    let end_turn = |r: &Rig| {
        r.paint(h.idle);
        if hooks {
            let _ = r.hook("Stop", &json!({}));
        }
        r.wait(SessionState::Idle, "a turn ended");
    };
    let ask = |r: &Rig| {
        let mut held = None;
        if hooks {
            let _ = r.hook("PreToolUse", &json!({"tool_name": "Bash", "tool_input": {"command": "mkdir x"}}));
            let reply = r.hook("PermissionRequest", &json!({"tool_name": "Bash", "tool_input": {"command": "mkdir x"}}));
            match reply {
                HookReply::Pending { rx, .. } => {
                    assert!(h.holds_permission, "{}: this harness draws its prompt only after the hook returns", h.name);
                    held = Some(rx);
                }
                HookReply::Immediate(_) => assert!(!h.holds_permission, "{}: the hook should wait for an answer", h.name),
            }
        }
        r.paint(h.approval);
        let s = r.wait(SessionState::NeedsYou, "a permission prompt");
        let a = s.attention.as_ref().unwrap();
        assert_eq!(a.reason, "permission");
        assert!(a.request_id.is_some(), "a permission prompt can be approved from a client");
        held
    };

    // working -> idle.
    start_turn(&r);
    end_turn(&r);

    // A prompt waits as long as it takes, and is answered at the pane: allow.
    start_turn(&r);
    let held = ask(&r);
    r.wait_painted(h.approval);
    let s = r.holds(SessionState::NeedsYou, "an unanswered prompt");
    assert_eq!(s.confidence, Confidence::Confirmed);
    if hooks {
        let _ = r.hook("PostToolUse", &json!({"tool_name": "Bash"}));
    }
    r.paint_moving(h.working);
    r.wait(SessionState::Working, "allowed at the pane");
    drop(held);
    end_turn(&r);

    // Denied at the pane: no real harness sends a hook for this.
    start_turn(&r);
    let held = ask(&r);
    r.wait_painted(h.approval);
    r.tick();
    r.paint(h.idle);
    let s = r.wait(SessionState::Idle, "denied at the pane, with no hook");
    assert_eq!(s.attention, None);
    assert!(s.unread, "the turn ended");
    if let Some(mut rx) = held {
        assert!(rx.try_recv().is_err(), "a held hook is let go once the prompt is answered");
        assert!(!r.engine.has_pending(&r.id));
    }

    // A question names what is asked, and is not an approval.
    if let Some(question) = h.question {
        start_turn(&r);
        if hooks {
            if let Some((event, payload)) = h.question_hook {
                let reply = r.hook(event, &serde_json::from_str(payload).unwrap());
                assert!(matches!(reply, HookReply::Immediate(_)), "{}: a question is answered at the terminal", h.name);
            }
        }
        r.paint(question);
        let s = r.wait(SessionState::NeedsYou, "a question");
        let a = s.attention.as_ref().unwrap();
        assert_eq!(a.reason, "question");
        assert_eq!(a.request_id, None);
        if hooks {
            assert_eq!(a.detail.as_deref(), Some("Which color?"));
        }
        r.wait_painted(question);
        r.holds(SessionState::NeedsYou, "an unanswered question");
        r.paint_moving(h.working);
        if hooks {
            let _ = r.hook("PostToolUse", &json!({}));
        }
        r.wait(SessionState::Working, "answered");
        end_turn(&r);
    }

    // Esc mid-turn: no real harness sends a `Stop` for this. The row must not
    // stay `working` on an idle pane.
    start_turn(&r);
    if h.idle_spins {
        r.paint_spinning(h.idle);
    } else {
        r.paint(h.idle);
    }
    r.wait(SessionState::Idle, "interrupted, with no hook");

    if hooks {
        // A stale working marker frozen on screen is not work: the Stop stands.
        start_turn(&r);
        r.paint(h.working);
        r.wait_painted(h.working);
        let _ = r.hook("Stop", &json!({}));
        r.holds(SessionState::Idle, "a frozen working marker after Stop");

        // A busy harness whose footer happens to read idle (Codex while it
        // streams) is still busy while the text keeps moving.
        let _ = r.hook("UserPromptSubmit", &json!({"prompt": "go"}));
        r.paint_moving(h.idle);
        r.holds(SessionState::Working, "text moving above an idle-looking footer");
        r.paint(h.idle);
        r.wait(SessionState::Idle, "and then it stopped");
    }

    // A crash says so, resumes, and the resumed row does not sit in `starting`
    // once its composer is up, even if a hook spoke and then none followed.
    r.paint("");
    let pid = r.fake_pid();
    let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
    if h.id_from_hooks && !hooks {
        // Nothing to resume with: it says so rather than restart the prompt.
        let s = r.wait(SessionState::NeedsYou, "a crash with no session id");
        assert_eq!(s.attention.as_ref().map(|a| a.reason.as_str()), Some("restorable"));
        let r = Rig::launch(h.name, host, StatusWindows::default());
        r.paint(h.idle);
        r.wait(SessionState::Idle, "a new session, composer up");
        std::fs::write(r.ctrl.join("exit"), "0").unwrap();
        let s = r.wait(SessionState::Done, "a clean exit");
        assert_eq!(s.exit_code, Some(0));
        return;
    }
    let s = r.wait(SessionState::Starting, "a crash");
    assert_eq!(s.attention.as_ref().map(|a| a.reason.as_str()), Some("crashed"));
    let start = Instant::now();
    while r.fake_pid() == pid {
        assert!(start.elapsed() < WAIT, "{}: never resumed", h.name);
        r.tick();
        std::thread::sleep(POLL);
    }
    r.wait(SessionState::Starting, "the relaunch");
    if hooks {
        // Mapped to nothing, but it does mark the row as hook-reported.
        let _ = r.hook("PreCompact", &json!({}));
        assert_ne!(r.row().state_source, "inferred");
    }
    r.paint(h.idle);
    r.wait(SessionState::Idle, "resumed, composer up, hooks silent");

    // A clean exit.
    std::fs::write(r.ctrl.join("exit"), "0").unwrap();
    let s = r.wait(SessionState::Done, "a clean exit");
    assert_eq!(s.exit_code, Some(0));
}

fn staleness(host: Host) {
    let windows = StatusWindows {
        stale_after: Duration::from_secs(4),
        starting_after: Duration::from_secs(4),
    };
    let mut r = Rig::launch("claude", host, windows);
    let confidence_within = |r: &Rig, want: Confidence, what: &str| {
        let start = Instant::now();
        loop {
            r.tick();
            let s = r.row();
            if s.confidence == want {
                return s;
            }
            assert!(start.elapsed() < WAIT, "{what}: wanted {want:?}, row reads {:?} ({})", s.confidence, s.state);
            std::thread::sleep(POLL);
        }
    };

    // No sign of life: `starting` says it does not know.
    let s = confidence_within(&r, Confidence::Unknown, "starting with a blank pane");
    assert_eq!(s.state, SessionState::Starting);
    let saw_frame = std::iter::from_fn(|| r.frames.try_recv().ok()).any(|f| match f {
        ServerFrame::Session { session } => session.id == r.id && session.confidence == Confidence::Unknown,
        _ => false,
    });
    assert!(saw_frame, "clients are told when a row stops being believable");

    // Evidence restores it.
    let _ = r.hook("UserPromptSubmit", &json!({"prompt": "go"}));
    assert_eq!(r.row().confidence, Confidence::Confirmed);
    assert!(r.row().confirmed_at.is_some());

    // `working` with a frozen pane fades, keeping its state: last known, not live.
    r.paint(CLAUDE.working);
    let s = confidence_within(&r, Confidence::Stale, "working on a frozen pane");
    assert_eq!(s.state, SessionState::Working);

    // A moving pane holds it up.
    r.paint_moving(CLAUDE.working);
    confidence_within(&r, Confidence::Confirmed, "the pane moved");
    std::thread::sleep(windows.stale_after + Duration::from_secs(1));
    let s = confidence_within(&r, Confidence::Confirmed, "the pane is still moving");
    assert_eq!(s.state, SessionState::Working);

    // `idle` with a quiet composer is exactly what idle looks like.
    r.paint(CLAUDE.idle);
    let _ = r.hook("Stop", &json!({}));
    std::thread::sleep(windows.stale_after + Duration::from_secs(1));
    let s = confidence_within(&r, Confidence::Confirmed, "idle at a quiet composer");
    assert_eq!(s.state, SessionState::Idle);

    // Waiting on a person never fades, and outlives its long-poll.
    let reply = r.hook("PermissionRequest", &json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}));
    let HookReply::Pending { request_id, payload, .. } = reply else {
        panic!("Claude Code's permission hook waits for an answer")
    };
    r.paint(CLAUDE.approval);
    let since = r.wait(SessionState::NeedsYou, "the prompt").attention.unwrap().since;
    assert!(since.is_some());
    std::thread::sleep(windows.stale_after + Duration::from_secs(1));
    let s = r.holds(SessionState::NeedsYou, "well past the stale window");
    assert_eq!(s.confidence, Confidence::Confirmed, "a person taking their time is not doubt");
    assert_eq!(r.engine.finish_pending(&request_id, None, &payload), json!({}), "the long-poll gave up");
    let s = r.holds(SessionState::NeedsYou, "after the long-poll timed out");
    assert_eq!(s.confidence, Confidence::Confirmed);
    assert_eq!(s.attention.unwrap().since, since, "still the same ask");
    assert!(!r.engine.has_pending(&r.id));
}

/// Without tmux the tmux rows skip, unless `SMOOTH_E2E_STRICT=1` (CI).
fn tmux_or_skip() -> bool {
    let have = tmux::tmux_available();
    let strict = std::env::var("SMOOTH_E2E_STRICT").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(have || !strict, "tmux is required (SMOOTH_E2E_STRICT is set)");
    have
}

macro_rules! conformance {
    ($($name:ident: $harness:ident, $wiring:ident, $host:ident;)*) => {$(
        #[test]
        fn $name() {
            if Host::$host == Host::Tmux && !tmux_or_skip() {
                return;
            }
            scenario(&$harness, Wiring::$wiring, Host::$host);
        }
    )*};
}

conformance! {
    claude_hooks_on_tmux: CLAUDE, Hooks, Tmux;
    claude_scrape_on_tmux: CLAUDE, ScrapeOnly, Tmux;
    codex_hooks_on_tmux: CODEX, Hooks, Tmux;
    codex_scrape_on_tmux: CODEX, ScrapeOnly, Tmux;
    opencode_hooks_on_tmux: OPENCODE, Hooks, Tmux;
    opencode_scrape_on_tmux: OPENCODE, ScrapeOnly, Tmux;
}

#[cfg(feature = "pty-host")]
conformance! {
    claude_hooks_on_pty: CLAUDE, Hooks, Pty;
    claude_scrape_on_pty: CLAUDE, ScrapeOnly, Pty;
    codex_hooks_on_pty: CODEX, Hooks, Pty;
    codex_scrape_on_pty: CODEX, ScrapeOnly, Pty;
    opencode_hooks_on_pty: OPENCODE, Hooks, Pty;
    opencode_scrape_on_pty: OPENCODE, ScrapeOnly, Pty;
}

#[test]
fn staleness_on_tmux() {
    if tmux_or_skip() {
        staleness(Host::Tmux);
    }
}

#[cfg(feature = "pty-host")]
#[test]
fn staleness_on_pty() {
    staleness(Host::Pty);
}
