//! The harness conformance suite (pearl th-3cabf6).
//!
//! Every built-in manifest (`harness::BUILTIN`) — plus the reference
//! manifests in `tests/conformance/manifests/`, which exercise the mechanisms
//! no built-in uses yet — runs the contract in
//! `smooth_flow::harness_conformance` against `smooth-flow-fake-agent`,
//! through a private engine: scratch HOME / flow.db / worktree, its own tmux
//! socket, an in-process `POST /api/flow/hooks` listener, and binary
//! resolution pinned to the scratch HOME + a PATH whose first entry is a cmux
//! shim decoy. No real CLI, no network, no credentials.
//!
//! Adding a harness = adding its manifest to `BUILTIN` (and, for a scraped
//! one, `tests/conformance/<name>.toml` with captured screens). No test code.
//!
//! `SMOOTH_CONFORMANCE_ONLY=a,b` runs a subset. Without tmux the suite skips,
//! unless `SMOOTH_E2E_STRICT=1` (CI), which turns the skip into a failure.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smooth_flow::harness::{render_argv, Manifest, PromptAs, ResumeMode, SessionIdMode, StateSource, Vars, BUILTIN};
use smooth_flow::harness_conformance::{claims_permission, keys_bytes, screen_problems, FakeSpec, Fixture, Mechanism, Step, PERMISSION_MARKER, SPEC_ENV};
use smooth_flow::{proc, tmux, Decision, Engine, EngineConfig, HookCaller, HookEvent, HookReply, NewRequest, ServerFrame, Session, SessionState};

/// Every wait polls; the machine running this is shared with other agents.
const WAIT: Duration = Duration::from_secs(45);
const POLL: Duration = Duration::from_millis(150);
/// Manifests run this many at a time (each has its own tmux server).
const PARALLEL: usize = 4;
const LAUNCH_PROMPT: &str = "conformance launch prompt";
const STEER: &str = "conformance steer";

fn strict() -> bool {
    std::env::var("SMOOTH_E2E_STRICT").is_ok_and(|v| !v.is_empty() && v != "0")
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("conformance")
}

#[derive(Debug, Clone)]
enum Outcome {
    Pass(String),
    Fail(String),
    NotApplicable(String),
    Skipped,
}

struct Report {
    name: String,
    source: String,
    rows: Vec<(Step, Outcome)>,
}

impl Report {
    fn failed(&self) -> bool {
        self.rows.iter().any(|(_, o)| matches!(o, Outcome::Fail(_)))
    }

    fn render(&self) -> String {
        let mut s = format!("{} (state.source = {})\n", self.name, self.source);
        for (step, o) in &self.rows {
            let (glyph, detail) = match o {
                Outcome::Pass(d) => ("●", d.clone()),
                Outcome::Fail(d) => ("○ FAILED", d.clone()),
                Outcome::NotApplicable(d) => ("·", format!("n/a — {d}")),
                Outcome::Skipped => ("○", "skipped (an earlier step failed)".to_string()),
            };
            let _ = writeln!(s, "  {glyph} {:<10} {detail}", step.label());
        }
        s
    }
}

// ── the in-process hook listener ─────────────────────────────────────────────

/// `POST /api/flow/hooks` → `engine.hook`, long-polling a permission request
/// the way `smooth-daemon`'s route does. Returns the URL.
fn serve_hooks(engine: Engine) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/api/flow/hooks", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let engine = engine.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut len = 0usize;
                let mut token = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let l = line.trim();
                    if l.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = l.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap_or(0);
                        } else if k.eq_ignore_ascii_case(smooth_flow::hook_auth::TOKEN_HEADER) {
                            token = Some(v.trim().to_string());
                        }
                    }
                }
                let mut body = vec![0u8; len];
                if reader.read_exact(&mut body).is_err() {
                    return;
                }
                // th-91d032: the fake presents its launch token like a real hook.
                let caller = HookCaller { token, direct: true };
                let reply = match serde_json::from_slice::<HookEvent>(&body).map(|ev| engine.hook(ev, &caller)) {
                    Ok(Ok(HookReply::Immediate(v))) => v,
                    Ok(Ok(HookReply::Pending { request_id, rx, payload })) => {
                        let decision = rx.blocking_recv().ok();
                        engine.finish_pending(&request_id, decision, &payload)
                    }
                    _ => json!({}),
                };
                let text = reply.to_string();
                let mut conn = conn;
                let _ = write!(
                    conn,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            });
        }
    });
    url
}

// ── the rig ──────────────────────────────────────────────────────────────────

struct Rig {
    engine: Engine,
    frames: tokio::sync::broadcast::Receiver<ServerFrame>,
    socket: String,
    home: PathBuf,
    ws: PathBuf,
    path: OsString,
    log: PathBuf,
    spec_path: PathBuf,
    hook_url: String,
    scratch: tempfile::TempDir,
}

fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Rig {
    fn new(m: &Manifest, manifest_text: Option<&str>) -> Self {
        let scratch = tempfile::Builder::new().prefix("smooth-conformance-").tempdir().unwrap();
        // Canonical: a fake reports `cwd` as the OS resolves it (/private/var
        // on macOS), and the engine binds a learned session id by that path.
        let root = std::fs::canonicalize(scratch.path()).unwrap();
        let home = root.join("home");
        let ws = root.join("ws");
        std::fs::create_dir_all(home.join(".smooth").join("harnesses")).unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        if let Some(text) = manifest_text {
            std::fs::write(home.join(".smooth").join("harnesses").join(format!("{}.toml", m.name)), text).unwrap();
        }
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.email=c@smoo.ai",
                "-c",
                "user.name=c",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "scratch",
            ],
        ] {
            let _ = std::process::Command::new("git").args(args).current_dir(&ws).output();
        }
        let socket = format!("smooth-conformance-{}-{}", std::process::id(), &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let engine = Engine::open(EngineConfig {
            db_path: root.join("flow.db"),
            version: "conformance".into(),
            machine_label: "conformance".into(),
            home: home.clone(),
            daemon_url: None,
            ..EngineConfig::new(ws.clone())
        })
        .unwrap();
        let shims = root.join("cmux-cli-shims");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = std::env::join_paths([&shims, &bin]).unwrap();
        engine.set_resolve_env(home.clone(), path.clone());
        let frames = engine.subscribe();
        let hook_url = serve_hooks(engine.clone());
        Self {
            engine,
            frames,
            socket,
            log: root.join("fake.log"),
            spec_path: root.join("spec.json"),
            home,
            ws,
            path,
            hook_url,
            scratch,
        }
    }

    /// The fake behind a wrapper where the manifest says the real binary
    /// lives: `prefer_paths[0]` under HOME, else `names[0]` on PATH — and a
    /// decoy `names[0]` in a cmux shim dir first on PATH.
    fn install(&self, m: &Manifest) -> PathBuf {
        let root = self.home.parent().unwrap();
        let target = m
            .binary
            .prefer_paths
            .first()
            .map_or_else(|| root.join("bin").join(&m.binary.names[0]), |rel| self.home.join(rel));
        let fake = env!("CARGO_BIN_EXE_smooth-flow-fake-agent");
        script(&target, &format!("{SPEC_ENV}='{}' exec '{fake}' \"$@\"", self.spec_path.display()));
        script(
            &root.join("cmux-cli-shims").join(&m.binary.names[0]),
            &format!("echo shim >>'{}'\nexit 70", root.join("shim.log").display()),
        );
        target
    }

    fn get(&self, id: &str) -> Session {
        self.engine.get(id).unwrap().unwrap()
    }

    fn pane(&self, id: &str) -> String {
        match self.engine.snapshot(id) {
            Ok(ServerFrame::Screen { text, .. }) => text.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" ⏎ "),
            _ => "<no pane>".into(),
        }
    }

    fn drain(&mut self) {
        while self.frames.try_recv().is_ok() {}
    }

    /// Tick the supervisor until the session is in `want` — and, with
    /// `after_working`, only once `working` has been seen on the way (a turn,
    /// not the idle it started from). Reports whether `working` was seen.
    fn wait(&mut self, id: &str, want: SessionState, after_working: bool) -> Result<(Session, bool), String> {
        let start = Instant::now();
        let mut saw_working = false;
        loop {
            if let Err(e) = self.engine.supervise_tick() {
                return Err(format!("supervise tick: {e:#}"));
            }
            while let Ok(f) = self.frames.try_recv() {
                if let ServerFrame::Session { session } = f {
                    saw_working |= session.id == id && session.state == SessionState::Working;
                }
            }
            let s = self.get(id);
            saw_working |= s.state == SessionState::Working;
            if s.state == want && (saw_working || !after_working) {
                return Ok((s, saw_working));
            }
            if start.elapsed() > WAIT {
                return Err(format!(
                    "waited {}s for `{want}`{}, state stayed `{}` (source {}); pane: {}",
                    WAIT.as_secs(),
                    if after_working && !saw_working {
                        " after `working` (never observed)"
                    } else {
                        ""
                    },
                    s.state,
                    s.state_source,
                    self.pane(id)
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    fn log_entries(&self, what: &str) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["what"] == what)
            .map(|v| v["detail"].clone())
            .collect()
    }

    fn wait_log(&self, what: &str, count: usize) -> Option<Value> {
        let start = Instant::now();
        loop {
            let e = self.log_entries(what);
            if e.len() >= count {
                return e.into_iter().nth(count - 1);
            }
            if start.elapsed() > WAIT {
                return None;
            }
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        tmux::kill_server(&self.socket);
        // `SMOOTH_CONFORMANCE_KEEP=1` leaves the scratch dir (fake.log,
        // spec.json, flow.db) behind for a post-mortem.
        if std::env::var_os("SMOOTH_CONFORMANCE_KEEP").is_some() {
            let dir = std::mem::replace(&mut self.scratch, tempfile::tempdir().unwrap());
            eprintln!("harness conformance: kept {}", dir.keep().display());
        }
    }
}

// ── the contract ─────────────────────────────────────────────────────────────

fn expected_source(m: &Manifest) -> &'static str {
    match m.state.source {
        StateSource::Hooks => "hooks",
        StateSource::Native => "native",
        StateSource::Scrape => "inferred",
    }
}

#[allow(clippy::too_many_lines, reason = "the contract reads best as one linear machine")]
fn run_contract(m: &Manifest, manifest_text: Option<&str>) -> Report {
    let mut rig = Rig::new(m, manifest_text);
    let mut rows: Vec<(Step, Outcome)> = Vec::new();
    let report = |rows: Vec<(Step, Outcome)>| {
        let mut rows = rows;
        for s in Step::ALL {
            if !rows.iter().any(|(r, _)| r == s) {
                rows.push((*s, Outcome::Skipped));
            }
        }
        Report {
            name: m.name.clone(),
            source: format!("{:?}", m.state.source).to_lowercase(),
            rows,
        }
    };

    // 1. resolve
    let wrapper = rig.install(m);
    match m.resolve_binary_in(&rig.home, &rig.path) {
        Some(p) if p == wrapper => rows.push((Step::Resolve, Outcome::Pass(format!("{} (cmux shim on PATH skipped)", p.display())))),
        Some(p) => {
            rows.push((
                Step::Resolve,
                Outcome::Fail(format!("resolved {} instead of {}", p.display(), wrapper.display())),
            ));
            return report(rows);
        }
        None => {
            rows.push((Step::Resolve, Outcome::Fail(format!("nothing resolved (expected {})", wrapper.display()))));
            return report(rows);
        }
    }

    // 2. launch
    let fixture = match Fixture::load(&fixtures_dir(), &m.name) {
        Ok(f) => f,
        Err(e) => {
            rows.push((Step::Launch, Outcome::Fail(format!("{e:#}"))));
            return report(rows);
        }
    };
    let spec = match FakeSpec::for_manifest(m, fixture.as_ref(), Some(rig.hook_url.clone()), rig.log.clone()) {
        Ok(s) => s,
        Err(e) => {
            rows.push((Step::Launch, Outcome::Fail(e)));
            return report(rows);
        }
    };
    let problems = screen_problems(m, &spec);
    if !problems.is_empty() {
        rows.push((
            Step::Launch,
            Outcome::Fail(format!("fixture screens vs [state.scrape]: {}", problems.join("; "))),
        ));
        return report(rows);
    }
    std::fs::write(&rig.spec_path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
    rig.drain();
    let session = match rig.engine.new_session(NewRequest {
        kind: m.name.parse().unwrap(),
        worktree: Some(rig.ws.to_string_lossy().into_owned()),
        prompt: Some(LAUNCH_PROMPT.into()),
        tmux_socket: Some(rig.socket.clone()),
        title: Some("conformance".into()),
        ..NewRequest::default()
    }) {
        Ok(s) => s,
        Err(e) => {
            rows.push((Step::Launch, Outcome::Fail(format!("engine refused the launch: {e:#}"))));
            return report(rows);
        }
    };
    let id = session.id.clone();
    let paste = m.launch.prompt_as == PromptAs::Paste;
    let ws = rig.ws.to_string_lossy().into_owned();
    let mut want_argv = vec![wrapper.to_string_lossy().into_owned()];
    want_argv.extend(render_argv(
        &m.launch.argv,
        &Vars {
            prompt: (!paste).then_some(LAUNCH_PROMPT),
            session_id: session.agent_session_id.as_deref(),
            cwd: Some(&ws),
            ..Vars::default()
        },
    ));
    if session.argv != want_argv {
        rows.push((
            Step::Launch,
            Outcome::Fail(format!("engine argv {:?}, manifest renders {want_argv:?}", session.argv)),
        ));
        return report(rows);
    }
    let Some(parsed) = rig.wait_log("argv", 1) else {
        let why = if rig.log_entries("argv_mismatch").is_empty() {
            format!("the fake never started; pane: {}", rig.pane(&id))
        } else {
            format!("the argv does not round-trip through launch.argv {:?}", m.launch.argv)
        };
        rows.push((Step::Launch, Outcome::Fail(why)));
        return report(rows);
    };
    let got_prompt = parsed["prompt"].as_str().map(str::to_string);
    let want_prompt = (!paste).then(|| LAUNCH_PROMPT.to_string());
    if got_prompt != want_prompt {
        rows.push((
            Step::Launch,
            Outcome::Fail(format!("the fake read prompt {got_prompt:?} from its argv, expected {want_prompt:?}")),
        ));
        return report(rows);
    }
    if m.launch.session_id == SessionIdMode::Preassigned
        && m.resume.mode == ResumeMode::ResumeSession
        && spec.session_id_env.is_empty()
        && !m.launch.argv.iter().any(|a| a.contains("{session_id}"))
    {
        rows.push((
            Step::Launch,
            Outcome::Fail("resume_session with session_id = \"preassigned\", but neither launch.argv nor launch.env hands the harness {session_id} — it would resume an id it never had".into()),
        ));
        return report(rows);
    }
    let hands_over_id = !spec.session_id_env.is_empty() || m.launch.argv.iter().any(|a| a.contains("{session_id}"));
    if let (SessionIdMode::Preassigned, Some(sid), true) = (m.launch.session_id, &session.agent_session_id, hands_over_id) {
        if parsed["session_id"] != sid.as_str() {
            rows.push((
                Step::Launch,
                Outcome::Fail(format!("the harness saw session id {}, the engine assigned {sid}", parsed["session_id"])),
            ));
            return report(rows);
        }
    }
    rows.push((
        Step::Launch,
        Outcome::Pass(format!(
            "{} {}",
            if paste { "prompt pasted;" } else { "prompt in argv;" },
            want_argv[1..].join(" ")
        )),
    ));

    // 3 + 4. working → idle
    match rig.wait(&id, SessionState::Idle, true) {
        Ok((s, saw)) => {
            rows.push((
                Step::Working,
                if saw {
                    Outcome::Pass("observed".into())
                } else {
                    Outcome::Fail(format!("never observed `working` before idle; pane: {}", rig.pane(&id)))
                },
            ));
            if s.state_source == expected_source(m) {
                rows.push((Step::Idle, Outcome::Pass(format!("via {}", s.state_source))));
            } else {
                rows.push((
                    Step::Idle,
                    Outcome::Fail(format!(
                        "reached idle via `{}`, the manifest's state.source means `{}`",
                        s.state_source,
                        expected_source(m)
                    )),
                ));
            }
        }
        Err(e) => {
            rows.push((Step::Working, Outcome::Fail(e.clone())));
            rows.push((Step::Idle, Outcome::Fail(e)));
        }
    }
    if rows.iter().any(|(_, o)| matches!(o, Outcome::Fail(_))) {
        let _ = rig.engine.kill(&id, false);
        return report(rows);
    }

    // 5. steer
    let steer = |rig: &mut Rig, text: &str| -> Result<String, String> {
        rig.drain();
        rig.engine.send(&id, text).map_err(|e| format!("send: {e:#}"))?;
        rig.wait(&id, SessionState::Idle, true)?;
        Ok("working → idle".into())
    };
    rows.push((Step::Steer, steer(&mut rig, STEER).map_or_else(Outcome::Fail, Outcome::Pass)));

    // 6. permission
    if claims_permission(m) {
        let outcome = (|| -> Result<String, String> {
            rig.engine
                .send(&id, &format!("{PERMISSION_MARKER} please"))
                .map_err(|e| format!("send: {e:#}"))?;
            let (s, _) = rig.wait(&id, SessionState::NeedsYou, false)?;
            let att = s.attention.ok_or("needs_you without an attention")?;
            let rid = att
                .request_id
                .clone()
                .ok_or_else(|| format!("attention `{}` carries no request_id to approve", att.reason))?;
            rig.engine.approve(&id, &rid, Decision::Allow).map_err(|e| format!("approve: {e:#}"))?;
            rig.wait(&id, SessionState::Idle, true)?;
            if spec.mechanism == Mechanism::ClaudeHooks {
                let d = rig.wait_log("decision", 1).ok_or("the PermissionRequest hook never returned")?;
                let reply = d["reply"].as_str().unwrap_or_default();
                if !reply.contains("\"allow\"") {
                    return Err(format!("the harness got {reply:?}, not an allow decision"));
                }
                return Ok(format!("{} → approve → hook reply allow", att.reason));
            }
            // th-5a2314: the manifest's own approve_keys, byte for byte — the
            // Claude `1` typed into aider's `(Y)es/(N)o` prompt does nothing.
            let want = keys_bytes(&m.steer.approve_keys).map_err(|k| format!("[steer] approve_keys names `{k}`, which the rig cannot check"))?;
            let k = rig.wait_log("key", 1).ok_or("the approval keys never reached the harness")?;
            if k["key"].as_str() != Some(want.as_str()) {
                return Err(format!(
                    "the harness read {:?}, but [steer] approve_keys {:?} press {want:?}",
                    k["key"].as_str().unwrap_or_default(),
                    m.steer.approve_keys
                ));
            }
            Ok(format!("{} → approve → keys {}", att.reason, m.steer.approve_keys.join(" ")))
        })();
        rows.push((Step::Permission, outcome.map_or_else(Outcome::Fail, Outcome::Pass)));
    } else {
        rows.push((Step::Permission, Outcome::NotApplicable("the manifest reports no permission asks".into())));
    }

    // 7. resume
    let before = rig.get(&id);
    let outcome = (|| -> Result<String, String> {
        if m.launch.session_id == SessionIdMode::Learned && m.state.source != StateSource::Scrape && before.agent_session_id.is_none() {
            return Err("session_id = \"learned\" but no id was bound from the harness's first hook".into());
        }
        let argv_seen = rig.log_entries("argv").len();
        rig.drain();
        let relaunched = rig.engine.kill(&id, true).map_err(|e| format!("kill+resume: {e:#}"))?;
        let want = match (m.resume.mode, before.agent_session_id.as_deref()) {
            (ResumeMode::ResumeSession, Some(sid)) => {
                let mut v = vec![before.argv[0].clone()];
                v.extend(render_argv(
                    &m.resume.argv,
                    &Vars {
                        session_id: Some(sid),
                        cwd: Some(&before.worktree),
                        ..Vars::default()
                    },
                ));
                v
            }
            // th-e77603: the CLI's own "most recent conversation here" — the
            // flag follows the original command when the prompt was pasted
            // (it never reached the argv), the bare binary when it was an
            // argument (it must not be sent twice).
            (ResumeMode::ContinueLatest, _) => {
                let mut v = if paste { before.argv.clone() } else { vec![before.argv[0].clone()] };
                v.extend(render_argv(
                    &m.resume.argv,
                    &Vars {
                        cwd: Some(&before.worktree),
                        ..Vars::default()
                    },
                ));
                v
            }
            _ => before.argv.clone(),
        };
        if relaunched.argv != want {
            return Err(format!("relaunched with {:?}, expected {want:?}", relaunched.argv));
        }
        let parsed = rig
            .wait_log("argv", argv_seen + 1)
            .ok_or_else(|| format!("the resumed harness never started; pane: {}", rig.pane(&id)))?;
        if m.resume.mode == ResumeMode::ContinueLatest && parsed["resume"] != true {
            return Err("the relaunch argv did not parse as launch + the manifest's continue_latest resume.argv".into());
        }
        if m.resume.mode == ResumeMode::ResumeSession && before.agent_session_id.is_some() {
            if parsed["resume"] != true {
                return Err("the relaunch argv did not parse as the manifest's resume template".into());
            }
            if parsed["session_id"] != before.agent_session_id.as_deref().unwrap_or_default() {
                return Err(format!("resumed as {}, not {:?}", parsed["session_id"], before.agent_session_id));
            }
        }
        // Relaunching the original command re-runs an argv prompt; let that
        // turn finish before steering.
        if want == before.argv && !paste {
            rig.wait(&id, SessionState::Idle, true)?;
        }
        steer(&mut rig, "conformance steer after resume")?;
        let s = rig.get(&id);
        if s.state_source != expected_source(m) {
            return Err(format!(
                "after resume the row reads `{}`, not `{}` — the resumed harness's reports did not reach it",
                s.state_source,
                expected_source(m)
            ));
        }
        Ok(match (m.resume.mode, before.agent_session_id.is_some()) {
            (ResumeMode::ResumeSession, true) => format!("{} → steered turn idle", want[1..].join(" ")),
            (ResumeMode::ContinueLatest, _) => format!("continue_latest {} → steered turn idle", m.resume.argv.join(" ")),
            _ => "relaunched the original command → steered turn idle".into(),
        })
    })();
    rows.push((Step::Resume, outcome.map_or_else(Outcome::Fail, Outcome::Pass)));

    // 8. kill
    let s = rig.get(&id);
    let outcome = (|| -> Result<String, String> {
        let done = rig.engine.kill(&id, false).map_err(|e| format!("kill: {e:#}"))?;
        if done.state != SessionState::Done {
            return Err(format!("kill left the session `{}`", done.state));
        }
        let start = Instant::now();
        while s.pid.is_some_and(|p| proc::is_alive(p, s.pid_start)) {
            if start.elapsed() > Duration::from_secs(10) {
                // What is left says whose bug it is: a `Z` (exited, unreaped —
                // the parent's job) is not a `S`/`T` (never got the signal).
                let ps = std::process::Command::new("ps")
                    .args(["-o", "pid=,ppid=,stat=,args=", "-p", &s.pid.unwrap_or_default().to_string()])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                return Err(format!("pid {:?} still alive 10s after kill; ps: {ps}", s.pid));
            }
            std::thread::sleep(POLL);
        }
        if s.tmux_session.as_deref().is_some_and(|t| tmux::session_alive(&rig.socket, t)) {
            return Err("the tmux session survived the kill".into());
        }
        Ok("done; process and tmux session gone".into())
    })();
    rows.push((Step::Kill, outcome.map_or_else(Outcome::Fail, Outcome::Pass)));
    let shim_log = rig.home.parent().unwrap().join("shim.log");
    if shim_log.exists() {
        rows.push((Step::Resolve, Outcome::Fail("the cmux shim decoy was executed".into())));
    }
    report(rows)
}

/// `(name, manifest, text when it is not a built-in)` for every manifest under test.
fn manifests() -> Vec<(Manifest, Option<String>)> {
    let mut out: Vec<(Manifest, Option<String>)> = BUILTIN
        .iter()
        .map(|(name, text)| (Manifest::parse(text).unwrap_or_else(|e| panic!("built-in {name}: {e:#}")), None))
        .collect();
    let dir = fixtures_dir().join("manifests");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect())
        .unwrap_or_default();
    files.sort();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let m = Manifest::parse(&text).unwrap_or_else(|e| panic!("{}: {e:#}", f.display()));
        out.push((m, Some(text)));
    }
    if let Some(only) = std::env::var("SMOOTH_CONFORMANCE_ONLY").ok().filter(|s| !s.trim().is_empty()) {
        let names: Vec<&str> = only.split(',').map(str::trim).collect();
        out.retain(|(m, _)| names.contains(&m.name.as_str()));
        assert!(!out.is_empty(), "SMOOTH_CONFORMANCE_ONLY={only} matched no manifest");
    }
    out
}

#[test]
fn every_harness_manifest_passes_the_conformance_contract() {
    if !tmux::tmux_available() {
        assert!(!strict(), "tmux is required for the harness conformance suite (SMOOTH_E2E_STRICT is set)");
        eprintln!("harness conformance: SKIPPED — tmux is not installed");
        return;
    }
    let all = manifests();
    let mut reports = Vec::new();
    for chunk in all.chunks(PARALLEL) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk.iter().map(|(m, text)| scope.spawn(move || run_contract(m, text.as_deref()))).collect();
            for h in handles {
                reports.push(h.join().expect("a conformance run panicked"));
            }
        });
    }
    let text: String = reports.iter().map(Report::render).collect::<Vec<_>>().join("\n");
    eprintln!("harness conformance:\n{text}");
    let failed: Vec<&str> = reports.iter().filter(|r| r.failed()).map(|r| r.name.as_str()).collect();
    assert!(
        failed.is_empty(),
        "harness manifests failing the conformance contract: {failed:?}\n\n{text}\nSee docs/Engineering/Harness-Manifests.md § Supporting a harness."
    );
}
