//! `th flow` — a thin client over the SmoothFlow engine in the daemon
//! (epic th-6ac036, lane A th-7f0af3).
//!
//! Everything here is HTTP/WS against `/api/flow/*`; the engine is the only
//! state holder. Discovery is `~/.smooth/daemon.addr` (same as every other
//! `th` command); auth is the daemon's local token (`SMOOTH_LOCAL_TOKEN` →
//! `~/.smooth/operator-token`). `th flow` connects; it never launches the
//! daemon (spec rule 6 — the app owns the process tree for TCC).

use std::io::{Read, Write};
use std::time::Duration;

use anstream::{eprintln, println};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use clap::Subcommand;
use futures_util::{SinkExt, StreamExt};
use owo_colors::OwoColorize;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use crate::gradient::paint;

/// Ctrl-\ — detaches `th flow attach`.
const DETACH_BYTE: u8 = 0x1c;

#[derive(Debug, Subcommand)]
pub enum FlowCommands {
    /// List sessions (state, attention, worktree).
    #[command(visible_alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Start a session. `--kind claude` pre-assigns a `--session-id`; the
    /// engine launches it under tmux in `--worktree` (or creates
    /// `../<repo>-<pearl>-<slug>` when `--pearl` is given).
    New {
        /// shell, or any harness manifest name — `th harness list`
        /// (claude | codex | opencode | th-code | …)
        #[arg(long, default_value = "claude")]
        kind: String,
        /// Directory the PTY runs in (default: the daemon's workspace).
        #[arg(long)]
        worktree: Option<String>,
        /// Main checkout (default: derived from the worktree).
        #[arg(long)]
        project: Option<String>,
        /// Pearl to work (creates a worktree when --worktree is absent).
        #[arg(long)]
        pearl: Option<String>,
        /// Initial prompt for an agent kind.
        #[arg(long)]
        prompt: Option<String>,
        /// Model for an agent kind.
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// tmux socket (`tmux -L <name>`) to create the session on, instead of
        /// the daemon's default. Use the shell app's socket (`smoothflow`) when
        /// the agent needs the app's TCC grants.
        #[arg(long)]
        tmux_socket: Option<String>,
        /// Attach immediately after creating.
        #[arg(long)]
        attach: bool,
        #[arg(long)]
        json: bool,
        /// Explicit argv (after `--`), instead of the kind's default.
        #[arg(last = true)]
        argv: Vec<String>,
    },
    /// Stream a session to this terminal (raw mode) until Ctrl-\.
    Attach { id: String },
    /// Steer: paste TEXT + Enter into the agent's prompt.
    Send { id: String, text: Vec<String> },
    /// Answer a permission attention.
    Approve {
        id: String,
        /// Request id from the attention (default: the session's current one).
        #[arg(long)]
        request: Option<String>,
        /// allow | deny | allow_session
        #[arg(long, default_value = "allow")]
        decision: String,
        #[arg(long)]
        json: bool,
    },
    /// Kill the process tree; `--resume` relaunches with `claude --resume`.
    Kill {
        id: String,
        #[arg(long)]
        resume: bool,
        #[arg(long)]
        json: bool,
    },
    /// Plain-text snapshot of the visible pane.
    Snapshot {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Fan a prompt out to N candidate sessions, then pick the winner.
    Fanout {
        #[command(subcommand)]
        cmd: FanoutCommands,
    },
    /// Sessions that need you or finished unread.
    Inbox {
        #[arg(long)]
        json: bool,
    },
    /// The pearl-rail handoff block for a session.
    Handoff { id: String },
}

#[derive(Debug, Subcommand)]
pub enum FanoutCommands {
    /// Create N worktrees + sessions + child pearls for PROMPT.
    New {
        prompt: String,
        #[arg(long)]
        pearl: String,
        /// `label[:kind[:model]]`, repeatable (default kind claude).
        #[arg(long = "candidate", required = true)]
        candidates: Vec<String>,
        /// Main checkout to fan out from (default: the daemon's workspace).
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Merge WINNER, GC the losers, close their child pearls.
    Pick {
        fan_out_id: String,
        winner: String,
        #[arg(long)]
        json: bool,
    },
}

// ── daemon discovery ──────────────────────────────────────────────────────────

/// `host:port` of the running daemon from `~/.smooth/daemon.addr`.
fn daemon_addr() -> Result<String> {
    let path = dirs_next::home_dir().context("no home dir")?.join(".smooth").join("daemon.addr");
    let addr = std::fs::read_to_string(&path)
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("no daemon advertised in {} — start Big Smooth (th up) first", path.display()))?;
    Ok(addr)
}

/// `SMOOTH_LOCAL_TOKEN` → `~/.smooth/operator-token`.
fn local_token() -> Option<String> {
    if let Ok(t) = std::env::var("SMOOTH_LOCAL_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let p = dirs_next::home_dir()?.join(".smooth").join("operator-token");
    let t = std::fs::read_to_string(p).ok()?.trim().to_string();
    (!t.is_empty()).then_some(t)
}

fn http_base() -> Result<String> {
    Ok(format!("http://{}", daemon_addr()?))
}

fn ws_url() -> Result<String> {
    let token = local_token().map(|t| format!("?token={}", urlencoding::encode(&t))).unwrap_or_default();
    Ok(format!("ws://{}/api/flow/ws{token}", daemon_addr()?))
}

/// Parse one `label[:kind[:model]]` candidate spec.
pub fn parse_candidate(spec: &str) -> Result<Value> {
    let mut parts = spec.splitn(3, ':');
    let label = parts.next().unwrap_or("").trim();
    if label.is_empty() {
        bail!("candidate needs a label: `label[:kind[:model]]`");
    }
    let kind = parts.next().map(str::trim).filter(|k| !k.is_empty()).unwrap_or("claude");
    // Any harness manifest name is a kind (th-0f6126); the engine refuses one
    // it has no manifest for, with the list to run.
    let kind = kind.parse::<smooth_flow::SessionKind>().map_err(|e| anyhow!("candidate `{label}`: {e}"))?;
    let model = parts.next().map(str::trim).filter(|m| !m.is_empty());
    Ok(json!({ "label": label, "kind": kind.as_str(), "model": model }))
}

/// The two-line error contract: what failed, then what to do.
fn api_error(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string());
    let hint = match status.as_u16() {
        401 => "set SMOOTH_LOCAL_TOKEN or check ~/.smooth/operator-token",
        404 => "check the id with `th flow ls`",
        _ => "see ~/.smooth/smooth.log",
    };
    anyhow!("{msg}\n  → {hint}")
}

pub(crate) async fn call(method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
    let url = format!("{}{path}", http_base()?);
    let client = reqwest::Client::builder().timeout(Duration::from_secs(600)).build()?;
    let mut req = client.request(method, &url);
    if let Some(t) = local_token() {
        req = req.header("x-smooth-token", t);
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("daemon unreachable at {url}\n  → is Big Smooth running? (th up)"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(status, &text));
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// One request/response over the flow WS: send `frame`, return the first
/// reply whose type is in `want` (or the error).
async fn ws_call(frame: Value, want: &[&str], timeout: Duration) -> Result<Value> {
    let (ws, _) = tokio_tungstenite::connect_async(ws_url()?)
        .await
        .context("flow WS connect failed\n  → is Big Smooth running? (th up)")?;
    let (mut sink, mut source) = ws.split();
    // hello
    let _ = tokio::time::timeout(Duration::from_secs(10), source.next()).await;
    sink.send(Message::Text(frame.to_string().into())).await?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let msg = tokio::time::timeout_at(deadline, source.next())
            .await
            .context("timed out waiting for the engine")?;
        let Some(Ok(Message::Text(t))) = msg else {
            bail!("flow WS closed before replying")
        };
        let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        if ty == "flow.error" {
            bail!(
                "{}\n  → see ~/.smooth/smooth.log",
                v.get("message").and_then(Value::as_str).unwrap_or("engine error")
            );
        }
        if want.contains(&ty) {
            return Ok(v);
        }
    }
}

// ── rendering ─────────────────────────────────────────────────────────────────

/// State cell per the Presence CLI rules (`smooth-glow-up`): the glyph
/// carries the meaning (`●` live / `○` quiet / `◐` in between), styling is
/// pipe-safe via `paint`, and **amber is spent only on "needs you"**.
/// Returns `(cell, visible_width)` so tables pad without counting escapes.
fn state_cell(state: &str) -> (String, usize) {
    let (glyph, label) = match state {
        "working" => (paint("●", |g| g.bold().to_string()), "working"),
        "idle" => (paint("○", |g| g.dimmed().to_string()), "idle"),
        "needs_you" => (paint("●", |g| g.yellow().bold().to_string()), "needs you"),
        "limited" => (paint("◐", |g| g.dimmed().to_string()), "limited"),
        "starting" => (paint("◐", |g| g.dimmed().to_string()), "starting"),
        "done" => (paint("○", |g| g.green().to_string()), "done"),
        "dead" => (paint("●", |g| g.red().to_string()), "dead"),
        other => return (other.to_string(), other.chars().count()),
    };
    (format!("{glyph} {label}"), label.chars().count() + 2)
}

/// `[reason]` tag: amber only when the reason means Big Smooth needs you;
/// a crash is an error; a scheduled usage-limit resume is quiet.
fn attention_tag(reason: &str) -> String {
    let tag = format!("[{reason}]");
    match reason {
        "permission" | "question" | "held" => paint(&tag, |t| t.yellow().to_string()),
        "crashed" => paint(&tag, |t| t.red().to_string()),
        _ => paint(&tag, |t| t.dimmed().to_string()),
    }
}

/// Pad `cell` (whose visible width is `width`) to `to` columns.
fn pad(cell: &str, width: usize, to: usize) -> String {
    format!("{cell}{}", " ".repeat(to.saturating_sub(width)))
}

fn short(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= n {
        s.to_string()
    } else {
        format!("{}…", c[..n.saturating_sub(1)].iter().collect::<String>())
    }
}

fn print_sessions(sessions: &[Value]) {
    if sessions.is_empty() {
        println!("No flow sessions. This is a confirmed read of the engine, not a read failure.");
        println!("  Start one: th flow new --kind claude --prompt \"…\"");
        return;
    }
    // Boldness is spent once per screen: the header. Pad BEFORE styling —
    // escape codes would otherwise count toward width.
    let header = format!("{:<12} {:<9} {:<16} {:<9} {:<34} {}", "ID", "KIND", "STATE", "VIA", "TITLE", "WORKTREE");
    println!("{}", paint(&header, |h| h.bold().to_string()));
    for s in sessions {
        let g = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let (mut state, mut width) = state_cell(&g("state"));
        if s.get("unread").and_then(Value::as_bool).unwrap_or(false) {
            state.push_str(" ✦");
            width += 2;
        }
        let att = s
            .pointer("/attention/reason")
            .and_then(Value::as_str)
            .map(|r| format!(" {}", attention_tag(r)))
            .unwrap_or_default();
        // VIA: how the engine knows the state — `hooks` (the harness told it)
        // or `inferred` (pane scraping) — th-5c5457.
        let via = match g("state_source").as_str() {
            "" => "-".to_string(),
            v => v.to_string(),
        };
        println!(
            "{:<12} {:<9} {} {} {:<34} {}{att}",
            g("id"),
            g("kind"),
            pad(&state, width, 16),
            paint(&format!("{via:<9}"), |v| v.dimmed().to_string()),
            short(&g("title"), 33),
            paint(&short(&g("worktree"), 48), |w| w.dimmed().to_string())
        );
    }
}

/// The array under `key`, or empty.
fn list_of<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn emit(json: bool, v: &Value, human: impl FnOnce(&Value)) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(v)?);
    } else {
        human(v);
    }
    Ok(())
}

// ── commands ──────────────────────────────────────────────────────────────────

/// Entry point for `th flow`.
pub async fn cmd_flow(cmd: FlowCommands) -> Result<()> {
    match cmd {
        FlowCommands::Ls { json } => {
            let v = call(reqwest::Method::GET, "/api/flow/sessions", None).await?;
            emit(json, &v, |v| {
                print_sessions(list_of(v, "sessions"));
            })
        }
        FlowCommands::Inbox { json } => {
            let v = call(reqwest::Method::GET, "/api/flow/sessions", None).await?;
            let inbox: Vec<Value> = v
                .get("sessions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|s| s.get("state").and_then(Value::as_str) == Some("needs_you") || s.get("unread").and_then(Value::as_bool) == Some(true))
                .collect();
            emit(json, &json!({ "sessions": inbox }), |v| {
                let items = list_of(v, "sessions");
                if items.is_empty() {
                    println!("Inbox empty — nothing needs you. This is a confirmed read of the engine.");
                } else {
                    print_sessions(items);
                }
            })
        }
        FlowCommands::New {
            kind,
            worktree,
            project,
            pearl,
            prompt,
            model,
            title,
            tmux_socket,
            attach,
            json,
            argv,
        } => {
            cmd_new(NewArgs {
                kind,
                worktree,
                project,
                pearl,
                prompt,
                model,
                title,
                tmux_socket,
                attach,
                json,
                argv,
            })
            .await
        }
        FlowCommands::Attach { id } => attach_session(&id).await,
        FlowCommands::Send { id, text } => {
            let text = text.join(" ");
            if text.trim().is_empty() {
                bail!("nothing to send\n  → th flow send <id> \"text\"");
            }
            call(reqwest::Method::POST, &format!("/api/flow/sessions/{id}/send"), Some(json!({ "text": text }))).await?;
            println!("{} sent to {id}", paint("●", |g| g.green().to_string()));
            Ok(())
        }
        FlowCommands::Approve { id, request, decision, json } => cmd_approve(&id, request, &decision, json).await,
        FlowCommands::Kill { id, resume, json } => {
            let v = call(
                reqwest::Method::POST,
                &format!("/api/flow/sessions/{id}/kill"),
                Some(json!({ "resume": resume })),
            )
            .await?;
            emit(json, &v, |v| {
                let state = v.pointer("/session/state").and_then(Value::as_str).unwrap_or("?");
                println!("{} {id} → {}", paint("●", |g| g.green().to_string()), state_cell(state).0);
            })
        }
        FlowCommands::Snapshot { id, json } => {
            let v = call(reqwest::Method::GET, &format!("/api/flow/sessions/{id}/snapshot"), None).await?;
            emit(json, &v, |v| println!("{}", v.get("text").and_then(Value::as_str).unwrap_or("")))
        }
        FlowCommands::Handoff { id } => {
            let v = call(reqwest::Method::GET, &format!("/api/flow/sessions/{id}/handoff"), None).await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        FlowCommands::Fanout { cmd } => cmd_fanout(cmd).await,
    }
}

struct NewArgs {
    kind: String,
    worktree: Option<String>,
    project: Option<String>,
    pearl: Option<String>,
    prompt: Option<String>,
    model: Option<String>,
    title: Option<String>,
    tmux_socket: Option<String>,
    attach: bool,
    json: bool,
    argv: Vec<String>,
}

async fn cmd_new(a: NewArgs) -> Result<()> {
    let NewArgs {
        kind,
        worktree,
        project,
        pearl,
        prompt,
        model,
        title,
        tmux_socket,
        attach,
        json,
        argv,
    } = a;
    let body = json!({
        "kind": kind,
        "worktree": worktree,
        "project": project,
        "pearl_id": pearl,
        "prompt": prompt,
        "model": model,
        "title": title,
        "tmux_socket": tmux_socket,
        "argv": if argv.is_empty() { Value::Null } else { json!(argv) },
    });
    let v = call(reqwest::Method::POST, "/api/flow/sessions", Some(body)).await?;
    let id = v.pointer("/session/id").and_then(Value::as_str).unwrap_or("").to_string();
    emit(json, &v, |v| {
        let s = v.get("session").cloned().unwrap_or(Value::Null);
        println!(
            "{} {id} {}",
            paint("●", |g| g.green().to_string()),
            s.get("title").and_then(Value::as_str).unwrap_or("")
        );
        println!(
            "  {}  {}",
            paint("worktree", |l| l.dimmed().to_string()),
            s.get("worktree").and_then(Value::as_str).unwrap_or("")
        );
        if !attach {
            println!("  {}  th flow attach {id}", paint("attach  ", |l| l.dimmed().to_string()));
        }
    })?;
    if attach {
        attach_session(&id).await?;
    }
    Ok(())
}

async fn cmd_approve(id: &str, request: Option<String>, decision: &str, json: bool) -> Result<()> {
    if !matches!(decision, "allow" | "deny" | "allow_session") {
        bail!("decision must be allow | deny | allow_session");
    }
    let request_id = if let Some(r) = request {
        r
    } else {
        let v = call(reqwest::Method::GET, "/api/flow/sessions", None).await?;
        list_of(&v, "sessions")
            .iter()
            .find(|s| s.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|s| s.pointer("/attention/request_id").and_then(Value::as_str))
            .map(str::to_string)
            .ok_or_else(|| anyhow!("{id} has no pending permission request\n  → th flow ls"))?
    };
    let v = call(
        reqwest::Method::POST,
        &format!("/api/flow/sessions/{id}/approve"),
        Some(json!({ "request_id": request_id, "decision": decision })),
    )
    .await?;
    emit(json, &v, |_| println!("{} {decision} → {id}", paint("●", |g| g.green().to_string())))
}

async fn cmd_fanout(cmd: FanoutCommands) -> Result<()> {
    match cmd {
        FanoutCommands::New {
            prompt,
            pearl,
            candidates,
            project,
            json,
        } => {
            let cands = candidates.iter().map(|c| parse_candidate(c)).collect::<Result<Vec<_>>>()?;
            let frame = json!({
                "channel": "flow", "type": "flow.fanout.new",
                "prompt": prompt, "pearl_id": pearl, "candidates": cands, "project": project,
            });
            let v = ws_call(frame, &["flow.fanout"], Duration::from_secs(300)).await?;
            emit(json, &v, |v| {
                println!(
                    "{} fan-out {}",
                    paint("●", |g| g.green().to_string()),
                    v.pointer("/fan_out/id").and_then(Value::as_str).unwrap_or("?")
                );
                print_sessions(list_of(v, "candidates"));
            })
        }
        FanoutCommands::Pick { fan_out_id, winner, json } => {
            let frame = json!({ "channel": "flow", "type": "flow.fanout.pick", "fan_out_id": fan_out_id, "winner_session_id": winner });
            let v = ws_call(frame, &["flow.fanout"], Duration::from_secs(600)).await?;
            emit(json, &v, |v| {
                println!("{} merged {winner} (fan-out {fan_out_id})", paint("●", |g| g.green().to_string()));
                print_sessions(list_of(v, "candidates"));
            })
        }
    }
}

/// Restores the terminal when `attach_session` returns by any path.
struct RawGuard;

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Raw-mode attach: stdin → `flow.input`, `flow.output` → stdout, size
/// changes → `flow.resize`, Ctrl-\ detaches.
async fn attach_session(id: &str) -> Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(ws_url()?)
        .await
        .context("flow WS connect failed\n  → is Big Smooth running? (th up)")?;
    let (mut sink, mut source) = ws.split();
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    sink.send(Message::Text(
        json!({"channel":"flow","type":"flow.attach","id":id,"cols":cols,"rows":rows})
            .to_string()
            .into(),
    ))
    .await?;
    eprintln!("{} attached to {id} — Ctrl-\\ to detach", paint("●", |g| g.bold().to_string()));
    crossterm::terminal::enable_raw_mode().context("enable raw mode")?;
    let raw_guard = RawGuard;

    // stdin reader thread → channel (blocking read; a thread is the only
    // portable way to get raw bytes without a stdin future).
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if in_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut size_tick = tokio::time::interval(Duration::from_millis(500));
    let (mut last_cols, mut last_rows) = (cols, rows);
    loop {
        tokio::select! {
            bytes = in_rx.recv() => {
                let Some(bytes) = bytes else { break };
                if bytes.contains(&DETACH_BYTE) {
                    let _ = sink.send(Message::Text(json!({"channel":"flow","type":"flow.detach","id":id}).to_string().into())).await;
                    break;
                }
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                if sink.send(Message::Text(json!({"channel":"flow","type":"flow.input","id":id,"data_b64":b64}).to_string().into())).await.is_err() {
                    break;
                }
            }
            _ = size_tick.tick() => {
                if let Ok((c, r)) = crossterm::terminal::size() {
                    if (c, r) != (last_cols, last_rows) {
                        (last_cols, last_rows) = (c, r);
                        let _ = sink.send(Message::Text(json!({"channel":"flow","type":"flow.resize","id":id,"cols":c,"rows":r}).to_string().into())).await;
                    }
                }
            }
            msg = source.next() => {
                let Some(Ok(Message::Text(t))) = msg else { break };
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                match v.get("type").and_then(Value::as_str) {
                    Some("flow.output") if v.get("id").and_then(Value::as_str) == Some(id) => {
                        if let Some(b) = v.get("data_b64").and_then(Value::as_str) {
                            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b) {
                                let mut stdout = std::io::stdout();
                                let _ = stdout.write_all(&bytes);
                                let _ = stdout.flush();
                            }
                        }
                    }
                    Some("flow.error") => {
                        drop(raw_guard);
                        bail!("{}", v.get("message").and_then(Value::as_str).unwrap_or("engine error"));
                    }
                    _ => {}
                }
            }
        }
    }
    drop(raw_guard);
    eprintln!(
        "\r\n{} detached from {id} (still running — `th flow attach {id}` to return)",
        paint("○", |g| g.dimmed().to_string())
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn candidate_spec_parsing() {
        assert_eq!(parse_candidate("a").unwrap(), json!({"label":"a","kind":"claude","model":null}));
        assert_eq!(parse_candidate("b:codex").unwrap(), json!({"label":"b","kind":"codex","model":null}));
        assert_eq!(parse_candidate("c:claude:opus").unwrap(), json!({"label":"c","kind":"claude","model":"opus"}));
        assert_eq!(parse_candidate("d::sonnet").unwrap()["kind"], "claude");
        assert!(parse_candidate("").is_err());
        // th-0f6126: any manifest name is a kind (the engine validates it);
        // junk that can't be a name is refused here.
        assert_eq!(parse_candidate("e:th-code").unwrap()["kind"], "th-code");
        assert_eq!(parse_candidate("f:aider").unwrap()["kind"], "aider");
        assert!(parse_candidate("x:Bad Kind").is_err());
    }

    #[test]
    fn state_cells_carry_a_glyph() {
        for s in ["working", "idle", "needs_you", "limited", "starting", "done", "dead"] {
            let (cell, width) = state_cell(s);
            assert!(cell.contains('●') || cell.contains('○') || cell.contains('◐'), "{cell}");
            assert!((6..=11).contains(&width), "{s}: {width}");
        }
        assert_eq!(state_cell("weird"), ("weird".to_string(), 5));
        // Amber is reserved for "needs you"; with color off (tests aren't a
        // TTY) every tag is the plain bracketed reason.
        for r in ["permission", "question", "held", "crashed", "usage_limit"] {
            assert_eq!(attention_tag(r), format!("[{r}]"));
        }
        // Padding counts visible width, not escape codes.
        assert_eq!(pad("ab", 2, 5), "ab   ");
        assert_eq!(pad("abcdef", 6, 5), "abcdef");
        assert_eq!(short("abcdef", 4), "abc…");
        assert_eq!(short("ab", 4), "ab");
    }

    #[test]
    fn api_error_is_two_lines_with_a_hint() {
        let e = api_error(reqwest::StatusCode::UNAUTHORIZED, r#"{"error":"missing token"}"#).to_string();
        assert!(e.starts_with("missing token\n"));
        assert!(e.contains("SMOOTH_LOCAL_TOKEN"));
        let e = api_error(reqwest::StatusCode::NOT_FOUND, "plain text").to_string();
        assert!(e.starts_with("plain text\n") && e.contains("th flow ls"));
    }

    #[test]
    fn ws_url_carries_the_token_when_present() {
        std::env::set_var("SMOOTH_LOCAL_TOKEN", "a b");
        assert_eq!(local_token().as_deref(), Some("a b"));
        std::env::remove_var("SMOOTH_LOCAL_TOKEN");
    }
}
