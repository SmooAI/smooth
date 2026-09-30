//! `th flow hook <harness> <event>` — the native SmoothFlow state hook
//! (pearl th-f97a27, epic th-64d4ab). It replaces the bash + curl + jq
//! `flow-hook.sh`, which cannot run natively on Windows, and keeps its wire
//! behavior exactly:
//!
//! ```text
//! POST http://<addr>/api/flow/hooks
//!      Content-Type: application/json
//!      X-Smooth-Flow-Hook-Token: <this launch's token>     (only when one exists)
//!      {"harness":…,"event":…,"session_id":…,"cwd":…,"payload":{…}[,"flow_id":…]}
//! ```
//!
//! * **Discovery** — `$SMOOTH_FLOW_ADDR` → `~/.smooth/flow.addr` →
//!   `~/.smooth/daemon.addr` (the chain `th flow` uses; `SMOOTH_FLOW_ADDR_FILE`
//!   / `SMOOTH_DAEMON_ADDR_FILE` override the two files, as they did for the
//!   script). No address → exit 0, silent.
//! * **Auth** — the token is read from the 0600 file `$SMOOTH_FLOW_HOOK_TOKEN_FILE`
//!   names (hex only, at most 128 chars). No file → no header: the engine may
//!   still adopt the session (th-c103c1) but only for state.
//! * **Envelope** — `session_id` is the first non-empty string of
//!   `.session_id | .sessionId | .conversation_id`, `cwd` of
//!   `.cwd | .workspace_roots[0]`, else `$PWD`; `flow_id` is `$SMOOTH_FLOW_ID`
//!   when set. A payload that is not a JSON object is forwarded as
//!   `{"raw": "<input>"}`, an empty one as `{}`.
//! * **Preface** — harnesses that parse a hook's stdout get their no-opinion
//!   answer printed FIRST: gemini/copilot `{}`, cursor-agent
//!   `{"continue":true}` for `beforeSubmitPrompt` and `{}` otherwise.
//! * **`PermissionRequest`** (only for a harness whose stdout is still ours)
//!   long-polls up to `FLOW_HOOK_PERMISSION_TIMEOUT` (120 s, the daemon's
//!   `HOOK_LONG_POLL`) and prints the reply verbatim — it IS the harness's
//!   decision JSON — but only when it carries
//!   `.hookSpecificOutput.decision.behavior` as a string. `{}`, a 4xx/5xx, a
//!   timeout or garbage prints nothing and the harness asks the user.
//! * **Everything else** is fire-and-forget with `FLOW_HOOK_TIMEOUT` (2 s).
//! * **Exit code is always 0**, whatever happens (only exit 2 blocks a
//!   `PreToolUse`, and nothing here may block the harness).
//!
//! One deliberate difference: the script detached its curl so a
//! fire-and-forget hook returned before the daemon answered. Detaching a
//! process is not portable (and a second process costs more than the POST),
//! so the native hook waits for the reply — a loopback round trip, bounded
//! by the same 2 s timeout. Events now also arrive in order.
//!
//! It is dispatched from `main` before clap, auth-profile setup and the
//! tracing file logger (see [`intercept`]), so a hook pays for none of them
//! and a usage error can never turn into clap's exit 2.

use std::ffi::OsString;
use std::io::{IsTerminal, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

/// The route every harness hook posts to.
pub const HOOK_PATH: &str = "/api/flow/hooks";
/// The per-launch token header (th-91d032).
pub const TOKEN_HEADER: &str = "X-Smooth-Flow-Hook-Token";
/// Matches the daemon's `HOOK_LONG_POLL` (and the script's default).
pub const DEFAULT_PERMISSION_TIMEOUT: Duration = Duration::from_secs(120);
/// Fire-and-forget events (the script's `FLOW_HOOK_TIMEOUT` default).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
/// Upper bound on establishing the TCP connection: a loopback daemon either
/// accepts at once or refuses at once, so this only bites a black-holed host.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// A token longer than this is truncated (the script's `head -c 128`).
const MAX_TOKEN: usize = 128;

/// Is this argv (`th flow hook …`) the hook? `--help` / `-h` fall through to
/// clap so `th flow hook --help` still documents itself.
#[must_use]
pub fn intercept(argv: &[OsString]) -> bool {
    argv.get(1).is_some_and(|a| a == "flow") && argv.get(2).is_some_and(|a| a == "hook") && !argv.iter().skip(3).any(|a| a == "--help" || a == "-h")
}

/// Run the hook for `th flow hook <harness> <event>` (`args` are the words
/// after `hook`). Never panics out, never fails: every error path is a
/// silent no-op, and the caller exits 0.
pub fn main(args: &[OsString]) {
    // A panic message on stderr is harmless, but a hook has no business
    // printing one; the unwind is caught below either way.
    std::panic::set_hook(Box::new(|_| {}));
    let harness = args.first().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
    let event = args.get(1).map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
    let _ = std::panic::catch_unwind(|| {
        let mut stdout = std::io::stdout().lock();
        run(&harness, &event, &Env::from_process(), read_stdin, &mut stdout);
        let _ = stdout.flush();
    });
}

/// Everything the hook reads from its environment, gathered up front so the
/// logic is testable without touching the process env.
#[derive(Debug, Default, Clone)]
pub struct Env {
    pub flow_addr: Option<String>,
    pub flow_addr_file: Option<PathBuf>,
    pub daemon_addr_file: Option<PathBuf>,
    pub token_file: Option<PathBuf>,
    pub flow_id: Option<String>,
    pub pwd: String,
    pub permission_timeout: Duration,
    pub timeout: Duration,
}

impl Env {
    fn from_process() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let smooth = dirs_next::home_dir().map(|h| h.join(".smooth"));
        let file = |over: &str, name: &str| var(over).map(PathBuf::from).or_else(|| smooth.as_ref().map(|d| d.join(name)));
        Self {
            flow_addr: var("SMOOTH_FLOW_ADDR"),
            flow_addr_file: file("SMOOTH_FLOW_ADDR_FILE", "flow.addr"),
            daemon_addr_file: file("SMOOTH_DAEMON_ADDR_FILE", "daemon.addr"),
            token_file: var(smooth_flow::hook_auth::TOKEN_FILE_ENV).map(PathBuf::from),
            flow_id: var("SMOOTH_FLOW_ID"),
            pwd: logical_cwd(),
            permission_timeout: parse_timeout(var("FLOW_HOOK_PERMISSION_TIMEOUT").as_deref(), DEFAULT_PERMISSION_TIMEOUT),
            timeout: parse_timeout(var("FLOW_HOOK_TIMEOUT").as_deref(), DEFAULT_TIMEOUT),
        }
    }
}

/// `$PWD` when it still names the current directory (what bash's `$PWD`
/// gives the script — symlinks unresolved), else the real current directory.
fn logical_cwd() -> String {
    let real = std::env::current_dir().ok();
    if let Ok(pwd) = std::env::var("PWD") {
        let same = |a: &Path, b: &Path| std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b);
        if !pwd.is_empty() && real.as_deref().is_some_and(|r| same(Path::new(&pwd), r)) {
            return pwd;
        }
    }
    real.map(|p| p.display().to_string()).unwrap_or_default()
}

/// Seconds (fractional allowed, like curl's `-m`); anything unparseable or
/// non-positive keeps the default.
#[must_use]
pub fn parse_timeout(raw: Option<&str>, default: Duration) -> Duration {
    raw.and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .map_or(default, Duration::from_secs_f64)
}

/// The no-opinion answer a stdout-parsing harness needs, printed before
/// anything can exit early. `None` = stdout stays ours (and empty).
#[must_use]
pub fn preface(harness: &str, event: &str) -> Option<&'static str> {
    match harness {
        "gemini" | "copilot" => Some("{}\n"),
        "cursor-agent" if event == "beforeSubmitPrompt" => Some("{\"continue\":true}\n"),
        "cursor-agent" => Some("{}\n"),
        _ => None,
    }
}

/// The token from the pane's token file: hex digits only, at most 128.
/// `None` when there is no file, it is unreadable, or it holds no hex.
#[must_use]
pub fn read_token(path: Option<&Path>) -> Option<String> {
    let raw = std::fs::read(path?).ok()?;
    let token: String = raw.iter().filter(|b| b.is_ascii_hexdigit()).take(MAX_TOKEN).map(|&b| char::from(b)).collect();
    (!token.is_empty()).then_some(token)
}

/// The first non-empty string among `candidates`.
fn first_str<'a>(candidates: impl IntoIterator<Item = Option<&'a Value>>) -> Option<&'a str> {
    candidates.into_iter().flatten().filter_map(Value::as_str).find(|s| !s.is_empty())
}

/// The `{harness, event, session_id, cwd, payload[, flow_id]}` body, in the
/// script's key order. `input` is the hook's stdin.
#[must_use]
pub fn envelope(harness: &str, event: &str, input: &str, pwd: &str, flow_id: Option<&str>) -> Value {
    // `$(cat)` drops trailing newlines; an empty payload is `{}`.
    let input = input.trim_end_matches(['\n', '\r']);
    let payload = if input.is_empty() {
        Value::Object(Map::new())
    } else {
        match serde_json::from_str::<Value>(input) {
            Ok(v @ Value::Object(_)) => v,
            _ => json!({ "raw": input }),
        }
    };
    let session_id = first_str([payload.get("session_id"), payload.get("sessionId"), payload.get("conversation_id")]).unwrap_or("");
    let root = payload.get("workspace_roots").and_then(Value::as_array).and_then(|a| a.first());
    let cwd = first_str([payload.get("cwd"), root]).unwrap_or(pwd);
    let mut body = json!({
        "harness": harness,
        "event": event,
        "session_id": session_id,
        "cwd": cwd,
        "payload": payload,
    });
    if let Some(id) = flow_id.filter(|s| !s.is_empty()) {
        body["flow_id"] = json!(id);
    }
    body
}

/// Only a real decision object goes to the harness; `{}` or garbage means
/// "no opinion".
#[must_use]
pub fn is_decision(reply: &str) -> bool {
    serde_json::from_str::<Value>(reply.trim())
        .ok()
        .is_some_and(|v| v.pointer("/hookSpecificOutput/decision/behavior").is_some_and(Value::is_string))
}

/// The engine address: env override, then `flow.addr`, then `daemon.addr`.
fn engine_addr(env: &Env) -> Option<String> {
    let read = |p: &Option<PathBuf>| p.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
    crate::flow::pick_flow_addr(env.flow_addr.clone(), read(&env.flow_addr_file), read(&env.daemon_addr_file))
}

/// Where to POST: `(https, host:port)`. An `https://` address (the script
/// accepted one) keeps its scheme; everything else is plain HTTP.
fn target(addr: &str) -> (bool, String) {
    addr.strip_prefix("https://")
        .map_or_else(|| (false, addr.to_string()), |rest| (true, rest.trim_end_matches('/').to_string()))
}

/// The whole hook: `input` yields the hook's stdin (read only once an engine
/// is known), `out` is its stdout, flushed as each line is written so the
/// preface survives a harness killing the hook mid-poll. Every failure is
/// silent.
pub fn run(harness: &str, event: &str, env: &Env, input: impl FnOnce() -> String, out: &mut dyn Write) {
    let mut say = |s: &str| {
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    };
    if event.is_empty() {
        return;
    }
    let answered = preface(harness, event);
    if let Some(p) = answered {
        say(p);
    }
    let Some(addr) = engine_addr(env) else { return };
    let token = read_token(env.token_file.as_deref());
    let input = input();
    let body = envelope(harness, event, &input, &env.pwd, env.flow_id.as_deref()).to_string();
    let (https, host) = target(&addr);

    if event == "PermissionRequest" && answered.is_none() {
        if let Some((status, reply)) = post(https, &host, token.as_deref(), &body, env.permission_timeout) {
            // curl -f: a 4xx/5xx is a failure, not a reply.
            if status < 400 && is_decision(&reply) {
                say(&format!("{}\n", reply.trim_end_matches('\n')));
            }
        }
        return;
    }
    let _ = post(https, &host, token.as_deref(), &body, env.timeout);
}

/// The hook's stdin, or nothing when it is a terminal (someone ran the hook
/// by hand) — never block waiting on a TTY.
fn read_stdin() -> String {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = stdin.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// POST `body` to the hooks route within `timeout` (connect + send +
/// reply). `Some((status, body))` on any HTTP reply, `None` on any failure.
fn post(https: bool, host: &str, token: Option<&str>, body: &str, timeout: Duration) -> Option<(u16, String)> {
    if https {
        return post_https(host, token, body, timeout);
    }
    let deadline = Instant::now() + timeout;
    let mut stream = connect(host, deadline)?;
    stream.set_nodelay(true).ok()?;
    let request = request_bytes(host, token, body);
    stream.set_write_timeout(Some(remaining(deadline)?)).ok()?;
    stream.write_all(&request).ok()?;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        stream.set_read_timeout(Some(remaining(deadline)?)).ok()?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if let Some(done) = parse_response(&raw) {
                    return Some(done);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    parse_response_eof(&raw)
}

/// What is left of `deadline`, or `None` once it has passed.
fn remaining(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())
}

/// Connect to the first address `host` resolves to that accepts in time.
fn connect(host: &str, deadline: Instant) -> Option<TcpStream> {
    let addrs = host.to_socket_addrs().ok()?;
    for addr in addrs {
        let budget = remaining(deadline)?.min(CONNECT_TIMEOUT);
        if let Ok(s) = TcpStream::connect_timeout(&addr, budget) {
            return Some(s);
        }
    }
    None
}

/// The HTTP/1.1 request: the headers curl sent, plus `Connection: close`.
#[must_use]
pub fn request_bytes(host: &str, token: Option<&str>, body: &str) -> Vec<u8> {
    let mut head = format!(
        "POST {HOOK_PATH} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: th-flow-hook/{}\r\nAccept: */*\r\nContent-Type: application/json\r\n",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(t) = token {
        head.push_str(&format!("{TOKEN_HEADER}: {t}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()));
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

/// Split a raw response into status, lowercase headers and body start.
fn split_head(raw: &[u8]) -> Option<(u16, Vec<(String, String)>, &[u8])> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..end]).ok()?;
    let mut lines = head.split("\r\n");
    let status = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Some((status, headers, &raw[end + 4..]))
}

/// A complete response, if `raw` holds one yet (Content-Length or a
/// finished chunked body).
fn parse_response(raw: &[u8]) -> Option<(u16, String)> {
    let (status, headers, body) = split_head(raw)?;
    let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        return dechunk(body).map(|b| (status, String::from_utf8_lossy(&b).into_owned()));
    }
    let len: usize = header("content-length")?.parse().ok()?;
    (body.len() >= len).then(|| (status, String::from_utf8_lossy(&body[..len]).into_owned()))
}

/// The response once the server closed the connection: whatever body came.
fn parse_response_eof(raw: &[u8]) -> Option<(u16, String)> {
    parse_response(raw).or_else(|| split_head(raw).map(|(status, _, body)| (status, String::from_utf8_lossy(body).into_owned())))
}

/// Decode a chunked body; `None` until the terminating chunk has arrived.
fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = body.windows(2).position(|w| w == b"\r\n")?;
        let size_hex = std::str::from_utf8(&body[..line_end]).ok()?;
        let size = usize::from_str_radix(size_hex.split(';').next()?.trim(), 16).ok()?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        if body.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

/// HTTPS (an `https://` address): reqwest's blocking client on its own
/// thread, since this runs inside `main`'s tokio runtime.
fn post_https(host: &str, token: Option<&str>, body: &str, timeout: Duration) -> Option<(u16, String)> {
    let url = format!("https://{host}{HOOK_PATH}");
    let token = token.map(str::to_string);
    let body = body.to_string();
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(CONNECT_TIMEOUT))
            .build()
            .ok()?;
        let mut req = client.post(url).header("Content-Type", "application/json").body(body);
        if let Some(t) = token {
            req = req.header(TOKEN_HEADER, t);
        }
        let resp = req.send().ok()?;
        let status = resp.status().as_u16();
        Some((status, resp.text().ok()?))
    })
    .join()
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn env_at(addr: Option<&str>) -> Env {
        Env {
            flow_addr: addr.map(str::to_string),
            pwd: "/pwd".into(),
            permission_timeout: Duration::from_secs(5),
            timeout: Duration::from_secs(2),
            ..Env::default()
        }
    }

    /// A one-shot mock daemon: answers every request with `reply`, sends
    /// each raw request down the channel.
    fn mock(reply: &'static str, delay: Duration) -> (String, mpsc::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = s.read(&mut chunk).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                            let len: usize = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse().ok())
                                .unwrap_or(0);
                            if buf.len() >= end + 4 + len {
                                break;
                            }
                        }
                    }
                    let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
                    std::thread::sleep(delay);
                    let _ = s.write_all(reply.as_bytes());
                });
            }
        });
        (addr, rx)
    }

    /// Run the hook with a Claude-shaped payload on stdin; returns its stdout.
    fn run(harness: &str, event: &str, env: &Env) -> String {
        let mut out = Vec::new();
        super::run(harness, event, env, || r#"{"session_id":"sid-1","cwd":"/w"}"#.to_string(), &mut out);
        String::from_utf8(out).unwrap()
    }

    const DECISION: &str = r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#;

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn intercept_only_fires_for_flow_hook_without_help() {
        let v = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(intercept(&v(&["th", "flow", "hook", "claude-code", "Stop"])));
        assert!(
            intercept(&v(&["th", "flow", "hook"])),
            "missing args are the hook's silent no-op, not clap's exit 2"
        );
        assert!(!intercept(&v(&["th", "flow", "hook", "--help"])));
        assert!(!intercept(&v(&["th", "flow", "hook", "codex", "-h"])));
        assert!(!intercept(&v(&["th", "flow", "ls"])));
        assert!(!intercept(&v(&["th", "hook", "flow"])));
        assert!(!intercept(&v(&["th"])));
    }

    #[test]
    fn preface_matches_the_script() {
        assert_eq!(preface("gemini", "BeforeAgent"), Some("{}\n"));
        assert_eq!(preface("copilot", "PermissionRequest"), Some("{}\n"));
        assert_eq!(preface("cursor-agent", "beforeSubmitPrompt"), Some("{\"continue\":true}\n"));
        assert_eq!(preface("cursor-agent", "stop"), Some("{}\n"));
        assert_eq!(preface("claude-code", "PermissionRequest"), None);
        assert_eq!(preface("codex", "Stop"), None);
        assert_eq!(preface("qwen", "PermissionRequest"), None);
    }

    #[test]
    fn envelope_reads_session_and_cwd_from_every_shape() {
        let e = envelope("claude-code", "Stop", "{\"session_id\":\"s1\",\"cwd\":\"/w\",\"x\":1}\n", "/pwd", None);
        assert_eq!(
            e.to_string(),
            r#"{"harness":"claude-code","event":"Stop","session_id":"s1","cwd":"/w","payload":{"session_id":"s1","cwd":"/w","x":1}}"#,
            "script key order, payload order preserved, no flow_id when unset"
        );
        let e = envelope("copilot", "Stop", r#"{"sessionId":"s2"}"#, "/pwd", Some("f-1"));
        assert_eq!(
            (e["session_id"].as_str(), e["cwd"].as_str(), e["flow_id"].as_str()),
            (Some("s2"), Some("/pwd"), Some("f-1"))
        );
        let e = envelope(
            "cursor-agent",
            "stop",
            r#"{"conversation_id":"c3","workspace_roots":["/r","/q"]}"#,
            "/pwd",
            Some(""),
        );
        assert_eq!((e["session_id"].as_str(), e["cwd"].as_str()), (Some("c3"), Some("/r")));
        assert!(e.get("flow_id").is_none(), "an empty SMOOTH_FLOW_ID adds nothing");
        // Empty strings and non-strings are skipped, not taken.
        let e = envelope("h", "E", r#"{"session_id":"","sessionId":7,"conversation_id":"c","cwd":""}"#, "/pwd", None);
        assert_eq!((e["session_id"].as_str(), e["cwd"].as_str()), (Some("c"), Some("/pwd")));
        let e = envelope("h", "E", r#"{"workspace_roots":"not-an-array"}"#, "/pwd", None);
        assert_eq!((e["session_id"].as_str(), e["cwd"].as_str()), (Some(""), Some("/pwd")));
    }

    #[test]
    fn envelope_wraps_non_objects_and_defaults_empty_input() {
        assert_eq!(envelope("h", "E", "", "/p", None)["payload"], json!({}));
        assert_eq!(envelope("h", "E", "\n\n", "/p", None)["payload"], json!({}));
        assert_eq!(envelope("h", "E", "not json\n", "/p", None)["payload"], json!({"raw": "not json"}));
        assert_eq!(envelope("h", "E", "[1,2]", "/p", None)["payload"], json!({"raw": "[1,2]"}));
        assert_eq!(envelope("h", "E", "\"s\"", "/p", None)["payload"], json!({"raw": "\"s\""}));
    }

    #[test]
    fn token_is_hex_only_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t");
        std::fs::write(&p, "  abc-DEF 123\nzz\n").unwrap();
        assert_eq!(read_token(Some(&p)).as_deref(), Some("abcDEF123"));
        std::fs::write(&p, "a".repeat(300)).unwrap();
        assert_eq!(read_token(Some(&p)).map(|t| t.len()), Some(128));
        std::fs::write(&p, "xyz\n").unwrap();
        assert_eq!(read_token(Some(&p)), None, "no hex → no header");
        assert_eq!(read_token(Some(&dir.path().join("missing"))), None);
        assert_eq!(read_token(None), None);
    }

    #[test]
    fn decisions_are_recognised_and_nothing_else() {
        assert!(is_decision(DECISION));
        assert!(is_decision(&format!("{DECISION}\n")));
        assert!(!is_decision("{}"));
        assert!(!is_decision(r#"{"hookSpecificOutput":{"decision":{"behavior":1}}}"#));
        assert!(!is_decision(r#"[{"hookSpecificOutput":{}}]"#));
        assert!(!is_decision("garbage"));
        assert!(!is_decision(""));
    }

    #[test]
    fn timeouts_parse_like_curl_minus_the_footguns() {
        let d = Duration::from_secs(9);
        assert_eq!(parse_timeout(Some("1"), d), Duration::from_secs(1));
        assert_eq!(parse_timeout(Some("0.5"), d), Duration::from_millis(500));
        assert_eq!(parse_timeout(Some("0"), d), d, "0 would mean 'forever' to curl — keep the default");
        assert_eq!(parse_timeout(Some("-3"), d), d);
        assert_eq!(parse_timeout(Some("soon"), d), d);
        assert_eq!(parse_timeout(None, d), d);
    }

    #[test]
    fn addresses_normalise_and_keep_https() {
        assert_eq!(target("127.0.0.1:5"), (false, "127.0.0.1:5".to_string()));
        assert_eq!(target("https://h:1/"), (true, "h:1".to_string()));
        let dir = tempfile::tempdir().unwrap();
        let flow = dir.path().join("flow.addr");
        let daemon = dir.path().join("daemon.addr");
        std::fs::write(&daemon, "127.0.0.1:9\n").unwrap();
        let mut env = env_at(None);
        env.flow_addr_file = Some(flow.clone());
        env.daemon_addr_file = Some(daemon);
        assert_eq!(engine_addr(&env).as_deref(), Some("127.0.0.1:9"), "daemon.addr when there is no flow.addr");
        std::fs::write(&flow, " http://127.0.0.1:5/ \n").unwrap();
        assert_eq!(engine_addr(&env).as_deref(), Some("127.0.0.1:5"), "flow.addr wins");
        env.flow_addr = Some("127.0.0.1:7".into());
        assert_eq!(engine_addr(&env).as_deref(), Some("127.0.0.1:7"), "$SMOOTH_FLOW_ADDR wins over both");
    }

    #[test]
    fn request_carries_route_headers_and_body() {
        let r = String::from_utf8(request_bytes("127.0.0.1:5", Some("ab12"), "{\"a\":1}")).unwrap();
        assert!(r.starts_with("POST /api/flow/hooks HTTP/1.1\r\nHost: 127.0.0.1:5\r\n"), "{r}");
        assert!(r.contains("\r\nContent-Type: application/json\r\n"), "{r}");
        assert!(r.contains("\r\nX-Smooth-Flow-Hook-Token: ab12\r\n"), "{r}");
        assert!(r.ends_with("Content-Length: 7\r\nConnection: close\r\n\r\n{\"a\":1}"), "{r}");
        let r = String::from_utf8(request_bytes("h:1", None, "{}")).unwrap();
        assert!(!r.to_lowercase().contains("x-smooth-flow-hook-token"), "no token → no header: {r}");
    }

    #[test]
    fn responses_parse_by_length_chunked_and_eof() {
        assert_eq!(parse_response(ok("{}").as_bytes()), Some((200, "{}".into())));
        assert_eq!(parse_response(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n{}"), None, "incomplete");
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n4\r\n\":1}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked), Some((200, "{\"a\":1}".into())));
        assert_eq!(parse_response_eof(b"HTTP/1.1 500 Oops\r\n\r\nboom"), Some((500, "boom".into())));
        assert_eq!(parse_response_eof(b"garbage"), None);
    }

    #[test]
    fn no_event_or_no_engine_is_a_silent_no_op() {
        assert_eq!(run("claude-code", "", &env_at(Some("127.0.0.1:1"))), "");
        let mut env = env_at(None);
        env.flow_addr_file = Some("/definitely/not/here".into());
        env.daemon_addr_file = Some("/definitely/not/here".into());
        assert_eq!(run("claude-code", "Stop", &env), "");
        assert_eq!(run("claude-code", "PermissionRequest", &env), "");
        // …but a stdout-parsing harness still gets its no-opinion answer.
        assert_eq!(run("gemini", "BeforeAgent", &env), "{}\n");
        assert_eq!(run("cursor-agent", "beforeSubmitPrompt", &env), "{\"continue\":true}\n");
    }

    #[test]
    fn a_dead_engine_fails_open_fast() {
        // Bind then drop: nothing listens on this port any more.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let env = env_at(Some(&format!("127.0.0.1:{port}")));
        let t = Instant::now();
        assert_eq!(run("claude-code", "PermissionRequest", &env), "");
        assert_eq!(run("claude-code", "Stop", &env), "");
        assert!(t.elapsed() < Duration::from_secs(2), "refused connects return at once: {:?}", t.elapsed());
    }

    #[test]
    fn permission_request_relays_only_a_decision() {
        let (addr, rx) = mock(Box::leak(ok(DECISION).into_boxed_str()), Duration::ZERO);
        let dir = tempfile::tempdir().unwrap();
        let tok = dir.path().join("tok");
        std::fs::write(&tok, "deadbeef\n").unwrap();
        let mut env = env_at(Some(&addr));
        env.token_file = Some(tok);
        env.flow_id = Some("flow-9".into());
        assert_eq!(run("claude-code", "PermissionRequest", &env), format!("{DECISION}\n"));
        let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(req.starts_with("POST /api/flow/hooks HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("X-Smooth-Flow-Hook-Token: deadbeef\r\n"), "{req}");
        let body: Value = serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["harness"], "claude-code");
        assert_eq!(body["event"], "PermissionRequest");
        assert_eq!(body["flow_id"], "flow-9");

        let (addr, _rx) = mock(Box::leak(ok("{}").into_boxed_str()), Duration::ZERO);
        assert_eq!(run("claude-code", "PermissionRequest", &env_at(Some(&addr))), "", "{{}} = no opinion");
        let (addr, _rx) = mock("HTTP/1.1 500 Internal\r\ncontent-length: 0\r\n\r\n", Duration::ZERO);
        assert_eq!(run("claude-code", "PermissionRequest", &env_at(Some(&addr))), "", "5xx = nothing");
        let err_with_decision: &'static str =
            Box::leak(format!("HTTP/1.1 403 Forbidden\r\ncontent-length: {}\r\n\r\n{DECISION}", DECISION.len()).into_boxed_str());
        let (addr, _rx) = mock(err_with_decision, Duration::ZERO);
        assert_eq!(
            run("claude-code", "PermissionRequest", &env_at(Some(&addr))),
            "",
            "curl -f: a 4xx body is never relayed"
        );
    }

    #[test]
    fn permission_request_honours_its_timeout_and_stays_silent() {
        let (addr, _rx) = mock(Box::leak(ok(DECISION).into_boxed_str()), Duration::from_secs(5));
        let mut env = env_at(Some(&addr));
        env.permission_timeout = Duration::from_millis(500);
        let t = Instant::now();
        assert_eq!(run("claude-code", "PermissionRequest", &env), "");
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    }

    #[test]
    fn a_harness_that_already_answered_never_long_polls() {
        // gemini's stdout carries `{}` already: its PermissionRequest is fire-and-forget.
        let (addr, rx) = mock(Box::leak(ok(DECISION).into_boxed_str()), Duration::ZERO);
        assert_eq!(run("gemini", "PermissionRequest", &env_at(Some(&addr))), "{}\n");
        assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap().contains("\"harness\":\"gemini\""));
    }

    #[test]
    fn fire_and_forget_posts_and_prints_nothing() {
        let (addr, rx) = mock(Box::leak(ok(DECISION).into_boxed_str()), Duration::ZERO);
        assert_eq!(run("codex", "Stop", &env_at(Some(&addr))), "", "even a decision-shaped reply is not printed");
        let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!req.to_lowercase().contains("x-smooth-flow-hook-token"), "no token file → no header: {req}");
        assert!(req.contains("\"harness\":\"codex\",\"event\":\"Stop\""), "{req}");
        // A slow engine costs at most the fire-and-forget timeout.
        let (addr, _rx) = mock(Box::leak(ok("{}").into_boxed_str()), Duration::from_secs(5));
        let mut env = env_at(Some(&addr));
        env.timeout = Duration::from_millis(300);
        let t = Instant::now();
        assert_eq!(run("claude-code", "Stop", &env), "");
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }
}
