//! `smooth-flow-fake-agent` — the harness conformance suite's stand-in for a
//! coding agent CLI (pearl th-3cabf6). Test support only: it is launched by a
//! private engine through a harness manifest and never talks to anything but
//! that engine's in-process hook listener.
//!
//! It speaks whatever the manifest says (see
//! `smooth_flow::harness_conformance`): it reads its `FakeSpec` from
//! `$SMOOTH_CONFORMANCE_SPEC`, recovers its prompt / session id by matching
//! its own argv against the manifest's launch or resume template, then runs a
//! turn per prompt / stdin line — posting the manifest's hook events, or just
//! painting screens for a scraped harness. A line containing
//! `conformance-permission` is a turn that asks for approval first.
//!
//! Every observation lands in the spec's JSON-lines `log`, which the rig reads
//! to prove what reached the harness (the argv it parsed, the decision the
//! permission long-poll returned, the key an approval pressed).

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smooth_flow::harness_conformance::{match_argv, match_continue_argv, FakeSpec, Mechanism, PERMISSION_MARKER, SPEC_ENV};

struct Fake {
    spec: FakeSpec,
    session_id: String,
    cwd: String,
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

fn log(spec: &FakeSpec, what: &str, detail: &Value) {
    let line = json!({"at_ms": now_ms().to_string(), "what": what, "detail": detail});
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&spec.log) {
        let _ = writeln!(f, "{line}");
    }
}

/// Clear the screen and paint `text` — a scraped harness's whole signal.
fn paint(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "\x1b[2J\x1b[H{text}");
    let _ = out.flush();
}

/// `stty` on the pane's tty; best effort (the rig proves the outcome).
fn stty(args: &[&str]) {
    let _ = Command::new("stty").args(args).stdin(Stdio::inherit()).status();
}

/// This launch's hook token (th-91d032): the file the engine names in
/// `SMOOTH_FLOW_HOOK_TOKEN_FILE`, hex only. A real harness hook presents it.
fn hook_token() -> Option<String> {
    let path = std::env::var_os(smooth_flow::hook_auth::TOKEN_FILE_ENV)?;
    let t: String = std::fs::read_to_string(path).ok()?.chars().filter(char::is_ascii_hexdigit).collect();
    (!t.is_empty()).then_some(t)
}

/// POST `body` to the hook URL; the reply body (a long-poll blocks here).
fn post(url: &str, body: &Value) -> Option<String> {
    let rest = url.strip_prefix("http://")?;
    let (host, path) = rest.split_once('/').map_or_else(|| (rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let mut stream = TcpStream::connect(host).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(180)));
    let payload = body.to_string();
    let token = hook_token()
        .map(|t| format!("{}: {t}\r\n", smooth_flow::hook_auth::TOKEN_HEADER))
        .unwrap_or_default();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n{token}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    response.split_once("\r\n\r\n").map(|(_, b)| b.to_string())
}

impl Fake {
    fn hook(&self, event: &str, payload: &Value) -> Option<String> {
        let url = self.spec.hook_url.as_deref()?;
        let body = json!({
            "harness": self.spec.harness,
            "event": event,
            "session_id": self.session_id,
            "cwd": self.cwd,
            "payload": payload,
        });
        let reply = post(url, &body);
        log(&self.spec, "hook", &json!({"event": event, "reply": reply}));
        reply
    }

    fn turn_start(&self, text: &str) {
        match &self.spec.mechanism {
            Mechanism::ClaudeHooks => {
                self.hook("UserPromptSubmit", &json!({"prompt": text}));
                self.hook("PreToolUse", &json!({"tool_name": "Bash", "tool_input": {"command": "true"}}));
            }
            Mechanism::Mapped { working, .. } => {
                self.hook(working, &json!({"prompt": text}));
            }
            Mechanism::Scrape => {}
        }
        paint(&self.spec.working);
    }

    fn turn_end(&self, text: &str) {
        match &self.spec.mechanism {
            Mechanism::ClaudeHooks => {
                self.hook("PostToolUse", &json!({"tool_name": "Bash", "tool_response": {"stdout": ""}}));
                self.hook("Stop", &json!({"last_assistant_message": format!("done: {text}")}));
            }
            Mechanism::Mapped { idle, .. } => {
                self.hook(idle, &json!({"message": format!("done: {text}")}));
            }
            Mechanism::Scrape => {}
        }
        paint(&self.spec.idle);
    }

    /// One raw keypress from the pane (the engine's approval keystroke).
    fn read_key(&self) -> String {
        stty(&["-icanon", "min", "1"]);
        let mut buf = [0u8; 8];
        let n = std::io::stdin().lock().read(&mut buf).unwrap_or(0);
        stty(&["icanon"]);
        let key = String::from_utf8_lossy(&buf[..n]).into_owned();
        log(&self.spec, "key", &json!({"key": key}));
        key
    }

    fn ask_permission(&self) {
        match &self.spec.mechanism {
            Mechanism::ClaudeHooks => {
                paint(&self.spec.needs_you);
                let reply = self.hook("PermissionRequest", &json!({"tool_name": "Bash", "tool_input": {"command": "git push"}}));
                log(&self.spec, "decision", &json!({"reply": reply}));
            }
            Mechanism::Mapped { needs_you: Some(ev), .. } => {
                paint(&self.spec.needs_you);
                self.hook(ev, &json!({"reason": "permission", "message": "run git push?"}));
                self.read_key();
            }
            Mechanism::Scrape => {
                paint(&self.spec.needs_you);
                self.read_key();
            }
            Mechanism::Mapped { needs_you: None, .. } => {
                log(&self.spec, "no_permission_mechanism", &Value::Null);
            }
        }
    }

    fn turn(&self, text: &str) {
        log(&self.spec, "turn", &json!({"text": text}));
        self.turn_start(text);
        std::thread::sleep(Duration::from_millis(self.spec.work_ms / 2));
        if text.contains(PERMISSION_MARKER) {
            self.ask_permission();
            paint(&self.spec.working);
        }
        std::thread::sleep(Duration::from_millis(self.spec.work_ms / 2));
        self.turn_end(text);
    }
}

fn main() -> ExitCode {
    let Some(spec_path) = std::env::var_os(SPEC_ENV) else {
        eprintln!("smooth-flow-fake-agent: ${SPEC_ENV} is not set — this binary only runs under the harness conformance suite");
        return ExitCode::from(64);
    };
    let spec: FakeSpec = match std::fs::read_to_string(&spec_path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("smooth-flow-fake-agent: bad spec {}: {e}", spec_path.to_string_lossy());
            return ExitCode::from(64);
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Resume first: a resume argv is never a launch argv plus leftovers.
    let resumed = if spec.resume_session {
        match_argv(&spec.resume_argv, &args)
    } else if spec.resume_continue {
        match_continue_argv(&spec.launch_argv, &spec.resume_argv, &args)
    } else {
        None
    };
    let is_resume = resumed.is_some();
    let Some(matched) = resumed.or_else(|| match_argv(&spec.launch_argv, &args)) else {
        log(&spec, "argv_mismatch", &json!({"argv": args}));
        paint(&format!("argv {args:?} does not match the manifest's launch or resume template"));
        return ExitCode::from(65);
    };
    let env_sid = spec.session_id_env.iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
    let session_id = matched
        .get("session_id")
        .map(ToString::to_string)
        .or(env_sid)
        .unwrap_or_else(|| format!("conformance-{}-{}", std::process::id(), now_ms()));
    let prompt = matched.get("prompt").map(ToString::to_string);
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    log(
        &spec,
        "argv",
        &json!({"argv": args, "resume": is_resume, "session_id": session_id, "prompt": prompt, "vars": matched.vars}),
    );
    let fake = Fake { spec, session_id, cwd };
    // The rig paints what it sees, so input must not echo onto the screens.
    stty(&["-echo"]);
    if fake.spec.mechanism == Mechanism::ClaudeHooks {
        fake.hook("SessionStart", &json!({"source": if is_resume { "resume" } else { "startup" }}));
    }
    paint(&fake.spec.boot);
    if let Some(p) = prompt.filter(|p| !p.trim().is_empty()) {
        fake.turn(&p);
    } else {
        paint(&fake.spec.idle);
    }
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        // Bind the read first: a lock guard in the `match` scrutinee would
        // live through the arm, and a permission turn's key read needs it.
        let read = stdin.lock().read_line(&mut line);
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                // Bracketed-paste markers, if the pane sent them.
                let text = line.replace("\x1b[200~", "").replace("\x1b[201~", "");
                let text = text.trim();
                if !text.is_empty() {
                    fake.turn(text);
                }
            }
        }
    }
    if let Mechanism::Mapped { ended: Some(ev), .. } = &fake.spec.mechanism {
        fake.hook(ev, &json!({}));
    }
    log(&fake.spec, "exit", &Value::Null);
    ExitCode::SUCCESS
}
