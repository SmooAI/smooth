//! The e2e rig: a REAL `smooth-daemon` per test on an ephemeral port, fully
//! isolated from the developer's Big Smooth — its own `$HOME` (so
//! `~/.smooth/{daemon.addr,operator-token,flow.db}` and `~/.smooth/harnesses/`
//! are throwaway), its own tmux server (`tmux -L flow-e2e-<pid>-<n>`), no
//! single-instance lock, no `tailscale serve`, no relay, no gateway
//! credentials. `fake-agent` and its four manifests are installed into that
//! HOME so the engine launches it the way it launches any harness.
//!
//! Everything here is driven the way real clients drive it: HTTP + the flow
//! WS for the apps, the `th` binary for the CLI, `POST /api/flow/hooks` for
//! what a harness's hook script posts.

#![allow(dead_code, reason = "each test file uses a different slice of the rig")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime};

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// How long a daemon may take to advertise its address.
const BOOT_TIMEOUT: Duration = Duration::from_secs(60);
/// The default wait for a state / snapshot / frame. The machine running this
/// suite is shared with other agents (load 10–20 is normal here), so waits
/// are generous and every one of them polls.
pub const WAIT: Duration = Duration::from_secs(30);
/// Supervision cadence in the daemon — a state driven by the tick lands
/// within one of these after its cause.
pub const TICK: Duration = Duration::from_secs(2);

static SEQ: AtomicU32 = AtomicU32::new(0);

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

/// The `smooth-daemon` this test crate was built against.
pub fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_smooth-daemon"))
}

/// `$SMOOTH_TH_BIN`, else the `th` cargo put next to the daemon (a workspace
/// `cargo test`/`nextest run` builds both), else none.
pub fn th_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SMOOTH_TH_BIN").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let sibling = daemon_bin().with_file_name("th");
    sibling.is_file().then_some(sibling)
}

/// The `flow_e2e_server` example next to the daemon (the macOS lane's host),
/// when it was built.
pub fn e2e_server_bin() -> Option<PathBuf> {
    let p = daemon_bin().parent()?.join("examples").join("flow_e2e_server");
    p.is_file().then_some(p)
}

/// `SMOOTH_E2E_STRICT=1` (CI) turns every skip into a failure, so a runner
/// missing tmux/bash/curl/`th` can never report green having run nothing.
pub fn strict() -> bool {
    std::env::var("SMOOTH_E2E_STRICT").is_ok_and(|v| !v.is_empty() && v != "0")
}

fn have(bin: &str, arg: &str) -> bool {
    Command::new(bin)
        .arg(arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Skip (or, strict, fail) with `why`. Returns `false` when the test should
/// return early.
pub fn skip(why: &str) -> bool {
    assert!(!strict(), "SMOOTH_E2E_STRICT is set and a prerequisite is missing: {why}");
    eprintln!("[skip] {why}");
    false
}

/// tmux + bash + curl — what the rig and `fake-agent` need.
pub fn prereqs() -> bool {
    if !have("tmux", "-V") {
        return skip("tmux is not installed");
    }
    if !have("bash", "--version") {
        return skip("bash is not installed");
    }
    if !have("curl", "--version") {
        return skip("curl is not installed");
    }
    true
}

/// Prereqs plus a `th` binary.
pub fn prereqs_with_th() -> bool {
    prereqs() && (th_bin().is_some() || skip("no `th` binary: build smooai-smooth-cli or set SMOOTH_TH_BIN"))
}

/// The real `~/.smooth/daemon.addr` of the user running this suite —
/// bytes + mtime, or `None` — for the never-touch-the-real-daemon proof.
pub fn real_daemon_addr() -> Option<(Vec<u8>, SystemTime)> {
    let p = dirs_next::home_dir()?.join(".smooth").join("daemon.addr");
    let bytes = std::fs::read(&p).ok()?;
    let mtime = std::fs::metadata(&p).ok()?.modified().ok()?;
    Some((bytes, mtime))
}

/// Base64 of `s` (the `flow.input` payload).
pub fn b64(s: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

/// Decode a `flow.output` payload.
pub fn unb64(s: &str) -> String {
    use base64::Engine as _;
    String::from_utf8_lossy(&base64::engine::general_purpose::STANDARD.decode(s).unwrap_or_default()).into_owned()
}

/// Sessions are `starting` until the first supervision tick or hook.
pub fn state(v: &Value) -> &str {
    v.get("state").and_then(Value::as_str).unwrap_or("")
}

pub fn sid(v: &Value) -> String {
    v.get("id").and_then(Value::as_str).unwrap_or("").to_string()
}

/// One booted daemon and the throwaway world it lives in.
pub struct Daemon {
    root: tempfile::TempDir,
    pub home: PathBuf,
    /// A git repo (branch `main`, one commit) sessions run in.
    pub ws: PathBuf,
    pub addr: String,
    pub token: String,
    pub socket: String,
    child: Child,
    log_path: PathBuf,
    http: reqwest::Client,
}

impl Daemon {
    /// Boot. Panics (with the daemon log) when it doesn't come up.
    pub async fn boot() -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = tempfile::Builder::new().prefix("flow-e2e-").tempdir().expect("tempdir");
        let home = root.path().join("home");
        let ws = root.path().join("ws");
        let socket = format!("flow-e2e-{}-{n}", std::process::id());
        install_fixtures(&home);
        git_init(&ws);

        let log_path = root.path().join("daemon.log");
        let log = std::fs::File::create(&log_path).expect("daemon log");
        let err = log.try_clone().expect("daemon log");
        let mut path = home.join(".local").join("bin").into_os_string();
        if let Some(p) = std::env::var_os("PATH") {
            path.push(":");
            path.push(p);
        }
        let mut cmd = Command::new(daemon_bin());
        cmd.args(["operator", "--addr", "127.0.0.1:0", "--tmux-socket", &socket])
            .env_clear()
            .env("PATH", &path)
            .env("HOME", &home)
            .env("TMPDIR", root.path())
            .env("SMOOTH_ALLOW_SECOND_DAEMON", "1")
            .env("SMOOTH_RELAY", "0")
            .env("SMOOTH_TAILSCALE_SERVE", "0")
            .env("SMOOTH_WORKSPACE", &ws)
            .env("RUST_LOG", "info,smooth_flow=debug,smooth_daemon::flow_route=debug")
            .env("TERM", "xterm-256color")
            .current_dir(&ws)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err));
        if let Some(th) = th_bin() {
            cmd.env("SMOOTH_TH_BIN", th);
        }
        let mut child = cmd.spawn().expect("spawn smooth-daemon");

        // A second instance (SMOOTH_ALLOW_SECOND_DAEMON) deliberately does NOT
        // advertise itself in ~/.smooth/daemon.addr (#546: SmoothFlow's
        // daemon must never repoint `th` at itself). The bound port is on the
        // daemon's own "listening" log line; the rig then writes daemon.addr
        // in ITS home so `th flow` / `th harness` find this daemon.
        let addr_file = home.join(".smooth").join("daemon.addr");
        let token_file = home.join(".smooth").join("operator-token");
        let start = Instant::now();
        let addr = loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                panic!(
                    "smooth-daemon exited during boot ({status}):\n{}",
                    std::fs::read_to_string(&log_path).unwrap_or_default()
                );
            }
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            if let Some(addr) = listening_addr(&log) {
                break addr;
            }
            assert!(
                start.elapsed() < BOOT_TIMEOUT,
                "smooth-daemon did not report a listening address within {BOOT_TIMEOUT:?}:\n{log}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        std::fs::write(&addr_file, format!("{addr}\n")).expect("write the rig's daemon.addr");
        let token = std::fs::read_to_string(&token_file).expect("operator-token").trim().to_string();
        assert!(!token.is_empty(), "empty operator-token");
        let http = reqwest::Client::builder().timeout(Duration::from_secs(150)).build().expect("reqwest");
        let d = Self {
            root,
            home,
            ws,
            addr,
            token,
            socket,
            child,
            log_path,
            http,
        };
        // The router is merged into the operator server; once the address is
        // advertised it is listening, but poll the flow route anyway.
        loop {
            if let Ok(r) = d.http.get(d.url("/api/flow/sessions")).header("x-smooth-token", &d.token).send().await {
                if r.status().is_success() {
                    break;
                }
            }
            assert!(start.elapsed() < BOOT_TIMEOUT, "flow routes never answered:\n{}", d.log());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        d
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn ws_url(&self) -> String {
        format!("ws://{}/api/flow/ws?token={}", self.addr, self.token)
    }

    /// The daemon's log so far.
    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    /// `fake-agent`'s own log in the workspace (argv, every hook + reply).
    pub fn agent_log(&self) -> String {
        std::fs::read_to_string(self.ws.join(".fake-agent.log")).unwrap_or_default()
    }

    /// `./.fake-agent-script` — commands fake-agent runs on EVERY start.
    pub fn write_script(&self, dir: &Path, lines: &[&str]) {
        std::fs::write(dir.join(".fake-agent-script"), format!("{}\n", lines.join("\n"))).expect("script");
    }

    /// A second git repo beside `ws` (another worktree for a session).
    pub fn extra_repo(&self, name: &str) -> PathBuf {
        let p = self.root.path().join(name);
        git_init(&p);
        p
    }

    // ── HTTP ──────────────────────────────────────────────────────────────

    pub async fn get(&self, path: &str) -> (u16, Value) {
        let r = self.http.get(self.url(path)).header("x-smooth-token", &self.token).send().await.expect("GET");
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(self.url(path))
            .header("x-smooth-token", &self.token)
            .json(&body)
            .send()
            .await
            .expect("POST");
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn put(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .put(self.url(path))
            .header("x-smooth-token", &self.token)
            .json(&body)
            .send()
            .await
            .expect("PUT");
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    /// A raw request with no token (auth tests).
    pub async fn get_unauthed(&self, path: &str) -> u16 {
        self.http.get(self.url(path)).send().await.expect("GET").status().as_u16()
    }

    /// `POST /api/flow/hooks` — what a hook script posts. Never sends the
    /// token (hooks are unauthenticated by contract). Waits up to 150 s so a
    /// `PermissionRequest` long-poll can be awaited.
    pub async fn hook(&self, harness: &str, event: &str, session_id: &str, cwd: Option<&str>, payload: Value) -> (u16, Value) {
        let mut body = json!({ "harness": harness, "event": event, "session_id": session_id, "payload": payload });
        if let Some(c) = cwd {
            body["cwd"] = json!(c);
        }
        let r = self.http.post(self.url("/api/flow/hooks")).json(&body).send().await.expect("POST hooks");
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn sessions(&self) -> Vec<Value> {
        let (status, v) = self.get("/api/flow/sessions").await;
        assert_eq!(status, 200, "{v}");
        v["sessions"].as_array().cloned().unwrap_or_default()
    }

    pub async fn session(&self, id: &str) -> Value {
        self.sessions()
            .await
            .into_iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("no session {id}"))
    }

    /// `POST /api/flow/sessions` for `kind` in the workspace.
    pub async fn new_session(&self, kind: &str, prompt: Option<&str>) -> Value {
        self.new_session_in(kind, prompt, &self.ws.clone()).await
    }

    pub async fn new_session_in(&self, kind: &str, prompt: Option<&str>, worktree: &Path) -> Value {
        let (status, v) = self
            .post("/api/flow/sessions", json!({ "kind": kind, "worktree": worktree, "prompt": prompt }))
            .await;
        assert_eq!(status, 200, "new {kind}: {v}\n{}", self.log());
        v["session"].clone()
    }

    pub async fn send(&self, id: &str, text: &str) {
        let (status, v) = self.post(&format!("/api/flow/sessions/{id}/send"), json!({ "text": text })).await;
        assert_eq!(status, 200, "send: {v}");
    }

    pub async fn approve(&self, id: &str, request_id: &str, decision: &str) -> Value {
        let (status, v) = self
            .post(
                &format!("/api/flow/sessions/{id}/approve"),
                json!({ "request_id": request_id, "decision": decision }),
            )
            .await;
        assert_eq!(status, 200, "approve: {v}");
        v["session"].clone()
    }

    pub async fn kill(&self, id: &str, resume: bool) -> Value {
        let (status, v) = self.post(&format!("/api/flow/sessions/{id}/kill"), json!({ "resume": resume })).await;
        assert_eq!(status, 200, "kill: {v}");
        v["session"].clone()
    }

    pub async fn snapshot(&self, id: &str) -> Value {
        let (status, v) = self.get(&format!("/api/flow/sessions/{id}/snapshot")).await;
        assert_eq!(status, 200, "snapshot: {v}");
        v
    }

    pub async fn screen(&self, id: &str) -> String {
        self.snapshot(id).await["text"].as_str().unwrap_or("").to_string()
    }

    /// The pane, or the engine's error for a dead one — for failure messages.
    pub async fn screen_lossy(&self, id: &str) -> String {
        let (_, v) = self.get(&format!("/api/flow/sessions/{id}/snapshot")).await;
        v["text"].as_str().map_or_else(|| format!("<no pane: {v}>"), str::to_string)
    }

    /// Poll the pane until it shows anything at all (a real CLI painting
    /// its first screen).
    pub async fn wait_screen_nonblank(&self, id: &str, timeout: Duration) -> String {
        let start = Instant::now();
        while start.elapsed() < timeout {
            let s = self.screen(id).await;
            if !s.trim().is_empty() {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("pane of {id} stayed blank for {timeout:?}\nagent log:\n{}", self.agent_log());
    }

    // ── waits (always polled, never slept-and-hoped) ─────────────────────

    /// Poll the session until `pred` holds; panics with the session, the
    /// pane and the daemon log otherwise.
    pub async fn wait_until(&self, id: &str, what: &str, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Value {
        let start = Instant::now();
        let mut last = Value::Null;
        while start.elapsed() < timeout {
            last = self.session(id).await;
            if pred(&last) {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let screen = self.screen_lossy(id).await;
        panic!(
            "session {id} never reached `{what}` within {timeout:?}\nlast: {last}\npane:\n{screen}\nagent log:\n{}\ndaemon log tail:\n{}",
            self.agent_log(),
            tail(&self.log(), 40)
        );
    }

    pub async fn wait_state(&self, id: &str, want: &str, timeout: Duration) -> Value {
        self.wait_until(id, want, timeout, |s| state(s) == want).await
    }

    /// Poll the pane until it contains `needle`.
    pub async fn wait_screen(&self, id: &str, needle: &str, timeout: Duration) -> String {
        let start = Instant::now();
        let mut last = String::new();
        while start.elapsed() < timeout {
            last = self.screen(id).await;
            if last.contains(needle) {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!(
            "pane of {id} never showed `{needle}` within {timeout:?}\npane:\n{last}\nagent log:\n{}",
            self.agent_log()
        );
    }

    // ── th ────────────────────────────────────────────────────────────────

    /// Run `th <args>` against THIS daemon (its HOME carries daemon.addr +
    /// operator-token). Returns (exit code, stdout, stderr).
    pub fn th(&self, args: &[&str]) -> (i32, String, String) {
        let th = th_bin().expect("th binary (checked by prereqs_with_th)");
        let mut path = self.home.join(".local").join("bin").into_os_string();
        if let Some(p) = std::env::var_os("PATH") {
            path.push(":");
            path.push(p);
        }
        let out = Command::new(&th)
            .args(args)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("TMPDIR", self.root.path())
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .current_dir(&self.ws)
            .output()
            .unwrap_or_else(|e| panic!("run {}: {e}", th.display()));
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// `th <args>` that must succeed and print JSON.
    pub fn th_json(&self, args: &[&str]) -> Value {
        let (code, out, err) = self.th(args);
        assert_eq!(code, 0, "th {} failed:\nstdout: {out}\nstderr: {err}", args.join(" "));
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("th {}: not JSON ({e}):\n{out}", args.join(" ")))
    }

    // ── flow WS ───────────────────────────────────────────────────────────

    pub async fn ws(&self) -> Ws {
        Ws::connect(&self.ws_url()).await
    }

    /// Open the flow.db this daemon writes (WAL — a second writer is fine).
    pub fn store(&self) -> smooth_flow::FlowStore {
        smooth_flow::FlowStore::open(&self.home.join(".smooth").join("flow.db")).expect("open flow.db")
    }

    /// Is `pid` alive?
    pub fn pid_alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
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
            eprintln!("--- daemon log ({}) ---\n{}", self.log_path.display(), tail(&self.log(), 60));
        }
    }
}

/// A flow WS client: `hello` consumed, frames read with a deadline.
pub struct Ws {
    sink: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    source: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    pub hello: Value,
}

impl Ws {
    pub async fn connect(url: &str) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(url).await.expect("flow ws connect");
        let (sink, mut source) = ws.split();
        let hello = next_text(&mut source, Duration::from_secs(10)).await.expect("hello");
        assert_eq!(hello["type"], "flow.hello", "{hello}");
        Self { sink, source, hello }
    }

    pub async fn send(&mut self, mut frame: Value) {
        if frame.get("channel").is_none() {
            frame["channel"] = json!("flow");
        }
        self.sink.send(Message::Text(frame.to_string().into())).await.expect("ws send");
    }

    /// The next frame, or `None` at the deadline.
    pub async fn next(&mut self, timeout: Duration) -> Option<Value> {
        next_text(&mut self.source, timeout).await
    }

    /// Drain frames until `pred` matches one; panics at the deadline listing
    /// what was seen.
    pub async fn wait_for(&mut self, what: &str, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + timeout;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.next(left).await {
                Some(v) if pred(&v) => return v,
                Some(v) => seen.push(brief(&v)),
                None => break,
            }
        }
        panic!("no `{what}` frame within {timeout:?}; saw:\n  {}", seen.join("\n  "));
    }

    /// Collect frames for `dur` (everything that arrives).
    pub async fn collect(&mut self, dur: Duration) -> Vec<Value> {
        let deadline = Instant::now() + dur;
        let mut out = Vec::new();
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.next(left).await {
                Some(v) => out.push(v),
                None => break,
            }
        }
        out
    }

    pub async fn attach(&mut self, id: &str, cols: u16, rows: u16) {
        self.send(json!({"type":"flow.attach","id":id,"cols":cols,"rows":rows})).await;
    }

    pub async fn input(&mut self, id: &str, text: &str) {
        self.send(json!({"type":"flow.input","id":id,"data_b64":b64(text)})).await;
    }

    /// Wait until the concatenated `flow.output` for `id` contains `needle`.
    pub async fn wait_output(&mut self, id: &str, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let mut acc = String::new();
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(v) = self.next(left).await else { break };
            if v["type"] == "flow.output" && v["id"] == id {
                acc.push_str(&unb64(v["data_b64"].as_str().unwrap_or("")));
                if acc.contains(needle) {
                    return acc;
                }
            }
        }
        panic!("output of {id} never contained `{needle}` within {timeout:?}; got:\n{acc}");
    }
}

async fn next_text(source: &mut SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>, timeout: Duration) -> Option<Value> {
    loop {
        let msg = tokio::time::timeout(timeout, source.next()).await.ok()??;
        match msg {
            Ok(Message::Text(t)) => return serde_json::from_str(&t).ok(),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

/// A one-line rendering of a frame for failure messages.
pub fn brief(v: &Value) -> String {
    let ty = v["type"].as_str().unwrap_or("?");
    match ty {
        "flow.session" => format!(
            "flow.session {} {} src={}",
            v["session"]["id"], v["session"]["state"], v["session"]["state_source"]
        ),
        "flow.output" => format!("flow.output {} seq={}", v["id"], v["seq"]),
        "flow.event" => format!("flow.event {} {} {:?}", v["id"], v["kind"], v["text"]),
        "flow.attention" => format!("flow.attention {} {}", v["id"], v["attention"]),
        _ => {
            let s = v.to_string();
            s.chars().take(160).collect()
        }
    }
}

/// The `host:port` from the daemon's `… operator listening … addr=<addr> …`
/// log line, once it is there.
pub fn listening_addr(log: &str) -> Option<String> {
    // tracing colours the log even into a file; strip `ESC[...m` first.
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

pub fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// The local clock time `secs` from now as `h:mm(am|pm)` — what a usage-limit
/// banner says, at the minute resolution the parser reads.
pub fn local_clock_in(secs: i64) -> String {
    let t = chrono::Local::now() + chrono::Duration::seconds(secs);
    let (h, m) = (t.format("%I").to_string(), t.format("%M").to_string());
    let h = h.trim_start_matches('0');
    let h = if h.is_empty() { "12" } else { h };
    format!("{h}:{m}{}", t.format("%p").to_string().to_lowercase())
}

fn install_fixtures(home: &Path) {
    let bin = home.join(".local").join("bin");
    let manifests = home.join(".smooth").join("harnesses");
    std::fs::create_dir_all(&bin).expect("mkdir bin");
    std::fs::create_dir_all(&manifests).expect("mkdir harnesses");
    let agent = fixtures().join("fake-agent");
    let dest = bin.join("fake-agent");
    std::fs::copy(&agent, &dest).expect("copy fake-agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).expect("chmod fake-agent");
    }
    for entry in std::fs::read_dir(fixtures().join("harnesses")).expect("fixtures/harnesses") {
        let p = entry.expect("entry").path();
        if p.extension().is_some_and(|e| e == "toml") {
            std::fs::copy(&p, manifests.join(p.file_name().expect("name"))).expect("copy manifest");
        }
    }
}

fn git_init(dir: &Path) {
    std::fs::create_dir_all(dir).expect("mkdir ws");
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "e2e")
            .env("GIT_AUTHOR_EMAIL", "e2e@test")
            .env("GIT_COMMITTER_NAME", "e2e")
            .env("GIT_COMMITTER_EMAIL", "e2e@test")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr));
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["commit", "-q", "--allow-empty", "-m", "init"]);
}
