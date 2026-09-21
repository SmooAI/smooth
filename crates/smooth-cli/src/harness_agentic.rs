//! `th harness add --agentic <name>` (pearl th-473294): ask Big Smooth to add
//! a coding-agent CLI to SmoothFlow by itself.
//!
//! Two halves:
//!
//! 1. **The provider gate.** `GET /api/llm/provider` on the running daemon.
//!    No model ⇒ prompt for one — the Smoo AI Gateway (sign in with Smoo,
//!    mint the org's `llm.smoo.ai` key, save it to `~/.smooth/providers.json`)
//!    or bring-your-own-key (`th model login`). The daemon reads its gateway
//!    at boot, so after saving one the honest answer is "restart Big Smooth
//!    and rerun", which is what this prints.
//! 2. **The turn.** One canonical-protocol turn over the daemon's `/ws`
//!    (`create_conversation_session` → `send_message` → stream →
//!    `eventual_response`), asking the agent to call its `add_harness` tool
//!    with the exact arguments. Progress (tool calls) goes to stderr, the
//!    agent's reply to stdout — pipe-safe, Presence-styled.

use std::time::Duration;

use anstream::{eprintln, println};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use owo_colors::OwoColorize;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use crate::gradient::paint;

/// A long tool call: each validation launches the CLI and waits for turns.
#[allow(clippy::duration_suboptimal_units, reason = "seconds match the rest of this crate")]
const FRAME_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The arguments `th harness add --agentic` forwards to the tool.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgenticArgs {
    pub name: String,
    pub binary: Option<String>,
    pub docs: Option<String>,
    pub iterations: u8,
    pub force: bool,
    pub install_unverified: bool,
    pub model: Option<String>,
}

impl AgenticArgs {
    /// The tool's argument object, exactly as the agent should pass it.
    #[must_use]
    pub fn tool_args(&self) -> Value {
        let mut v = json!({ "name": self.name, "max_iterations": self.iterations, "force": self.force, "install_unverified": self.install_unverified });
        if let Some(b) = &self.binary {
            v["binary_hint"] = json!(b);
        }
        if let Some(d) = &self.docs {
            v["docs_url"] = json!(d);
        }
        if let Some(m) = &self.model {
            v["model"] = json!(m);
        }
        v
    }
}

/// The message that drives the turn: explicit tool + explicit args, so the
/// agent's only judgement is relaying the report.
#[must_use]
pub fn turn_message(args: &AgenticArgs) -> String {
    format!(
        "Add the coding-agent CLI `{}` to SmoothFlow. Call your `add_harness` tool exactly once with these arguments and nothing else:\n{}\nIt can take several minutes — wait for it. Then report, in plain text: the manifest path (or that it was not installed), what was proven, what could not be proven and why, and the next steps from the report. Do not paraphrase the manifest; do not call any other tool.",
        args.name,
        serde_json::to_string_pretty(&args.tool_args()).unwrap_or_default()
    )
}

/// Which provider the user chose at the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderChoice {
    SmooGateway,
    BringYourOwn,
    Cancel,
}

/// Map a picker index to a choice (the picker lists Smoo first, BYO second,
/// cancel last).
#[must_use]
pub const fn provider_choice(index: usize) -> ProviderChoice {
    match index {
        0 => ProviderChoice::SmooGateway,
        1 => ProviderChoice::BringYourOwn,
        _ => ProviderChoice::Cancel,
    }
}

/// The picker rows, in the order [`provider_choice`] expects.
#[must_use]
pub fn provider_rows() -> Vec<String> {
    vec![
        format!(
            "{} Gateway (recommended) — sign in with Smoo, mint your org's llm.smoo.ai key",
            crate::gradient::smoo_ai()
        ),
        "Bring your own key — openai / anthropic / openrouter / ollama … (th model login)".to_string(),
        "Cancel".to_string(),
    ]
}

/// Run the gate: returns `Ok(true)` when the daemon can draft right now,
/// `Ok(false)` when the user must restart the daemon (or cancelled) — the
/// caller prints nothing more in that case.
///
/// # Errors
/// When the daemon is unreachable or a provider step fails.
pub async fn ensure_provider() -> Result<bool> {
    let status = crate::flow::call(reqwest::Method::GET, "/api/llm/provider", None).await?;
    let configured = status.get("configured").and_then(Value::as_bool).unwrap_or(false);
    let restart = status.get("restart_required").and_then(Value::as_bool).unwrap_or(false);
    if configured && !restart {
        let model = status.get("model").and_then(Value::as_str).unwrap_or("?");
        let host = status.get("gateway_host").and_then(Value::as_str).unwrap_or("?");
        eprintln!("{} provider: {model} via {host}", paint("●", |g| g.bold().to_string()));
        return Ok(true);
    }
    if configured && restart {
        print_restart();
        return Ok(false);
    }
    eprintln!(
        "{} Big Smooth has no LLM provider — drafting a manifest needs a model.",
        paint("○", |g| g.dimmed().to_string())
    );
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        bail!(
            "no LLM provider configured\n  → smoo auth login && smoo llm create-key && th model login smooai-gateway   (Smoo AI Gateway)\n  → th model login <provider> --api-key <key>                                  (your own key)"
        );
    }
    let rows = provider_rows();
    let idx = dialoguer::Select::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt("Choose a provider for Big Smooth")
        .items(&rows)
        .default(0)
        .interact()?;
    match provider_choice(idx) {
        ProviderChoice::SmooGateway => setup_smoo_gateway().await?,
        ProviderChoice::BringYourOwn => setup_byo()?,
        ProviderChoice::Cancel => return Ok(false),
    }
    // Saved. The daemon booted without it — say so plainly.
    let status = crate::flow::call(reqwest::Method::GET, "/api/llm/provider", None).await?;
    if status.get("configured").and_then(Value::as_bool).unwrap_or(false) {
        if status.get("restart_required").and_then(Value::as_bool).unwrap_or(false) {
            print_restart();
            return Ok(false);
        }
        return Ok(true);
    }
    bail!("the provider was saved but the daemon still reports none — check ~/.smooth/providers.json (th model status)")
}

fn print_restart() {
    eprintln!(
        "{} provider saved, but Big Smooth read its gateway at boot — restart it, then rerun:\n  th down && th up\n  th harness add --agentic <name>",
        paint("◐", |g| g.bold().to_string())
    );
}

/// `th` itself, for re-entrant subcommands (`smoo auth login`, `th model login`).
fn self_exe() -> Result<std::path::PathBuf> {
    std::env::current_exe().context("locate the th binary")
}

fn run_self(args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(self_exe()?)
        .args(args)
        .status()
        .with_context(|| format!("run th {}", args.join(" ")))?;
    if !status.success() {
        bail!("`th {}` exited with {status}", args.join(" "));
    }
    Ok(())
}

/// Smoo AI Gateway: user session → mint (or paste) the org key → save it as
/// the `smooai-gateway` provider, routed for coding.
async fn setup_smoo_gateway() -> Result<()> {
    if crate::smooai::user_client::UserClient::user_label().is_none() {
        eprintln!("{} signing in to Smoo AI…", paint("●", |g| g.bold().to_string()));
        run_self(&["smoo", "auth", "login"])?;
    }
    let client = crate::smooai::user_client::UserClient::from_user_session().await?;
    let org = crate::active_org::resolve(None)?;
    let key = match client.post(&format!("/organizations/{org}/llm-gateway/create-key"), &json!({})).await {
        Ok(resp) => resp
            .get("key")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("create-key returned no key"))?,
        Err(e) if e.to_string().contains("409") => {
            eprintln!(
                "{} this org already has a gateway key (its value is shown once, at mint time).",
                paint("◐", |g| g.bold().to_string())
            );
            let rotate = dialoguer::Confirm::with_theme(&dialoguer::theme::ColorfulTheme::default())
                .with_prompt("Rotate it now? (the old key stops working everywhere it is used)")
                .default(false)
                .interact()?;
            if rotate {
                let resp = client
                    .post(&format!("/organizations/{org}/llm-gateway/rotate-key"), &json!({}))
                    .await
                    .context("rotate-key")?;
                resp.get("key")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| anyhow!("rotate-key returned no key"))?
            } else {
                dialoguer::Password::with_theme(&dialoguer::theme::ColorfulTheme::default())
                    .with_prompt("Paste the existing llm.smoo.ai key")
                    .interact()?
            }
        }
        Err(e) => return Err(e.context("mint the org's LLM gateway key (smoo llm create-key)")),
    };
    let path = dirs_next::home_dir().context("no home directory")?.join(".smooth").join("providers.json");
    save_smoo_provider(&path, key.trim())?;
    eprintln!("{} saved smooai-gateway to {}", paint("●", |g| g.bold().to_string()), path.display());
    Ok(())
}

/// Register the Smoo gateway provider in `providers.json` without clobbering
/// other providers; it becomes the default when nothing else works (the same
/// rule `th model login` applies).
pub fn save_smoo_provider(path: &std::path::Path, key: &str) -> Result<()> {
    if key.is_empty() {
        bail!("empty gateway key");
    }
    let mut registry = if path.exists() {
        smooth_cast::provider_migration::load_providers_with_migration(path).unwrap_or_default()
    } else {
        smooth_operator::providers::ProviderRegistry::default()
    };
    let mut cfg = smooth_operator::providers::ProviderConfig::smooai_gateway(key);
    cfg.default_model = SMOO_DEFAULT_MODEL.to_string();
    registry.register_provider(cfg);
    if registry.default_llm_config().is_err() || registry.list_providers().len() == 1 {
        registry.set_default_provider("smooai-gateway");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    registry.save_to_file(path)
}

/// The gateway's coding default (concrete: the legacy `smooth-*` aliases are
/// gone at the gateway, SMOODEV-1793).
pub const SMOO_DEFAULT_MODEL: &str = "deepseek-v4-flash";

fn setup_byo() -> Result<()> {
    eprintln!("{} th model login (pick a provider, paste its key)…", paint("●", |g| g.bold().to_string()));
    run_self(&["model", "login"])
}

// ── the turn ─────────────────────────────────────────────────────────────────

/// What one inbound frame means for the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A streamed token of the reply (stdout).
    Token(String),
    /// A tool call started (stderr).
    ToolStart(String),
    /// A tool call finished (stderr); `is_error` paints it amber.
    ToolDone { tool: String, is_error: bool },
    /// The turn is over; the reply text.
    Done(String),
    /// The server reported an error.
    Error(String),
}

/// Translate a canonical frame (`smooth-operator-server` protocol) — the same
/// mapping `th code` uses (`stream_token` / `stream_chunk.rawResponse` /
/// `eventual_response`).
#[must_use]
pub fn translate(v: &Value) -> Option<Line> {
    match v.get("type").and_then(Value::as_str)? {
        "stream_token" => Some(Line::Token(v.get("token").and_then(Value::as_str).unwrap_or_default().to_string())),
        "stream_chunk" => {
            let raw = v.pointer("/data/state/rawResponse")?;
            if let Some(call) = raw.get("toolCall") {
                return Some(Line::ToolStart(call.get("name").and_then(Value::as_str).unwrap_or("?").to_string()));
            }
            let res = raw.get("toolResult")?;
            Some(Line::ToolDone {
                tool: res.get("name").and_then(Value::as_str).unwrap_or("?").to_string(),
                is_error: res.get("isError").and_then(Value::as_bool).unwrap_or(false),
            })
        }
        "eventual_response" => Some(Line::Done(extract_reply(v.pointer("/data/data/response")))),
        "error" => Some(Line::Error(smooth_cast::wire::error_message(v))),
        _ => None,
    }
}

fn extract_reply(response: Option<&Value>) -> String {
    let Some(resp) = response else { return String::new() };
    if let Some(parts) = resp.get("responseParts").and_then(Value::as_array) {
        let text: String = parts
            .iter()
            .filter_map(|p| {
                p.as_str()
                    .or_else(|| p.get("text").and_then(Value::as_str))
                    .or_else(|| p.get("content").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("");
        if !text.trim().is_empty() {
            return text.trim().to_string();
        }
    }
    resp.as_str().map_or_else(
        || resp.get("text").and_then(Value::as_str).map_or_else(|| resp.to_string(), str::to_string),
        str::to_string,
    )
}

fn ws_url() -> Result<String> {
    let token = crate::flow::local_token()
        .map(|t| format!("?token={}", urlencoding::encode(&t)))
        .unwrap_or_default();
    Ok(format!("ws://{}/ws{token}", crate::flow::daemon_addr()?))
}

/// Drive the turn and render it. Returns the agent's final reply.
///
/// # Errors
/// When the daemon's WS is unreachable, the turn errors, or times out.
pub async fn run_turn(args: &AgenticArgs) -> Result<String> {
    let (ws, _) = tokio_tungstenite::connect_async(ws_url()?)
        .await
        .context("Big Smooth WS connect failed\n  → is Big Smooth running? (th up); the token comes from SMOOTH_LOCAL_TOKEN or ~/.smooth/operator-token")?;
    let (mut sink, mut source) = ws.split();
    let rid = |n: u32| format!("th-harness-{}-{n}", std::process::id());
    // Same opening move as `th code` / the web SPA: the local flavor wants an
    // `agentId` (any uuid) and a display name on the session.
    sink.send(Message::Text(
        json!({ "action": "create_conversation_session", "requestId": rid(1), "agentId": uuid::Uuid::new_v4().to_string(), "userName": "th harness" })
            .to_string()
            .into(),
    ))
    .await?;
    let mut session_id: Option<String> = None;
    while session_id.is_none() {
        let frame = tokio::time::timeout(Duration::from_secs(30), source.next())
            .await
            .context("timed out waiting for Big Smooth to open a conversation")?
            .ok_or_else(|| anyhow!("Big Smooth closed the connection"))??;
        if let Message::Text(t) = frame {
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            if v.get("type").and_then(Value::as_str) == Some("error") {
                bail!("Big Smooth: {}", smooth_cast::wire::error_message(&v));
            }
            if let Some(s) = v.pointer("/data/sessionId").and_then(Value::as_str) {
                session_id = Some(s.to_string());
            }
        }
    }
    let session_id = session_id.unwrap_or_default();
    sink.send(Message::Text(
        json!({ "action": "send_message", "requestId": rid(2), "sessionId": session_id, "message": turn_message(args), "stream": true })
            .to_string()
            .into(),
    ))
    .await?;
    eprintln!(
        "{} asked Big Smooth to add `{}` — this launches the CLI for real, so it takes minutes",
        paint("●", |g| g.bold().to_string()),
        args.name
    );
    let mut streamed = false;
    loop {
        let frame = tokio::time::timeout(FRAME_TIMEOUT, source.next())
            .await
            .context("timed out waiting for Big Smooth (20 min) — the daemon log has the tool's progress")?
            .ok_or_else(|| anyhow!("Big Smooth closed the connection mid-turn"))??;
        let Message::Text(t) = frame else { continue };
        let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
        match translate(&v) {
            Some(Line::Token(tok)) => {
                streamed = true;
                print!("{tok}");
                let _ = std::io::Write::flush(&mut anstream::stdout());
            }
            Some(Line::ToolStart(tool)) => eprintln!("{} {tool}", paint("⚙", |g| g.dimmed().to_string())),
            Some(Line::ToolDone { tool, is_error }) => {
                if is_error {
                    eprintln!("{} {tool} failed", paint("○", |g| g.yellow().to_string()));
                } else {
                    eprintln!("{} {tool} done", paint("●", |g| g.dimmed().to_string()));
                }
            }
            Some(Line::Done(reply)) => {
                if !streamed && !reply.is_empty() {
                    println!("{reply}");
                } else if streamed {
                    println!();
                }
                let _ = sink.close().await;
                return Ok(reply);
            }
            Some(Line::Error(e)) => bail!("Big Smooth: {e}"),
            None => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn tool_args_forward_only_what_was_given() {
        let a = AgenticArgs {
            name: "gemini".into(),
            iterations: 2,
            ..AgenticArgs::default()
        };
        let v = a.tool_args();
        assert_eq!(v["name"], "gemini");
        assert_eq!(v["max_iterations"], 2);
        assert_eq!(v["force"], false);
        assert!(v.get("binary_hint").is_none());
        assert!(v.get("docs_url").is_none());
        let a = AgenticArgs {
            binary: Some("gem".into()),
            docs: Some("https://d".into()),
            model: Some("m".into()),
            force: true,
            ..a
        };
        let v = a.tool_args();
        assert_eq!(v["binary_hint"], "gem");
        assert_eq!(v["docs_url"], "https://d");
        assert_eq!(v["model"], "m");
        assert_eq!(v["force"], true);
    }

    #[test]
    fn turn_message_names_the_tool_and_embeds_the_args() {
        let m = turn_message(&AgenticArgs {
            name: "aider".into(),
            iterations: 3,
            ..AgenticArgs::default()
        });
        assert!(m.contains("`add_harness` tool exactly once"));
        assert!(m.contains("\"name\": \"aider\""));
        assert!(m.contains("do not call any other tool"));
    }

    #[test]
    fn provider_choice_maps_rows_in_order() {
        assert_eq!(provider_rows().len(), 3);
        assert_eq!(provider_choice(0), ProviderChoice::SmooGateway);
        assert_eq!(provider_choice(1), ProviderChoice::BringYourOwn);
        assert_eq!(provider_choice(2), ProviderChoice::Cancel);
        assert_eq!(provider_choice(9), ProviderChoice::Cancel);
        assert!(provider_rows()[0].contains("llm.smoo.ai"));
    }

    #[test]
    fn save_smoo_provider_registers_without_clobbering_and_routes_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        // Fresh file: the gateway is the default.
        save_smoo_provider(&p, "sk-1").unwrap();
        let reg = smooth_operator::providers::ProviderRegistry::load_from_file(&p).unwrap();
        assert_eq!(reg.list_providers(), vec!["smooai-gateway"]);
        let cfg = reg.default_llm_config().unwrap();
        assert!(cfg.api_url.contains("llm.smoo.ai"), "{}", cfg.api_url);
        assert_eq!(cfg.api_key, "sk-1");
        assert_eq!(cfg.model, SMOO_DEFAULT_MODEL);
        // Existing other provider survives; the gateway key updates in place.
        let mut reg = reg;
        reg.register_provider(smooth_operator::providers::ProviderConfig::ollama());
        reg.save_to_file(&p).unwrap();
        save_smoo_provider(&p, "sk-2").unwrap();
        let reg = smooth_operator::providers::ProviderRegistry::load_from_file(&p).unwrap();
        let mut ids = reg.list_providers();
        ids.sort_unstable();
        assert_eq!(ids, vec!["ollama", "smooai-gateway"]);
        assert_eq!(reg.get_provider("smooai-gateway").unwrap().api_key, "sk-2");
        assert!(save_smoo_provider(&p, "").is_err());
    }

    #[test]
    fn translate_maps_the_canonical_frames() {
        assert_eq!(translate(&json!({"type": "stream_token", "token": "hi"})), Some(Line::Token("hi".into())));
        assert_eq!(
            translate(&json!({"type": "stream_chunk", "data": {"state": {"rawResponse": {"toolCall": {"name": "add_harness"}}}}})),
            Some(Line::ToolStart("add_harness".into()))
        );
        assert_eq!(
            translate(&json!({"type": "stream_chunk", "data": {"state": {"rawResponse": {"toolResult": {"name": "add_harness", "isError": true}}}}})),
            Some(Line::ToolDone {
                tool: "add_harness".into(),
                is_error: true
            })
        );
        assert_eq!(
            translate(&json!({"type": "eventual_response", "data": {"data": {"response": {"responseParts": ["done ", {"text": "ok"}]}}}})),
            Some(Line::Done("done ok".into()))
        );
        assert!(matches!(translate(&json!({"type": "error", "error": {"message": "boom"}})), Some(Line::Error(m)) if m.contains("boom")));
        assert_eq!(translate(&json!({"type": "stream_reasoning", "token": "x"})), None, "reasoning never renders");
        assert_eq!(translate(&json!({"nope": 1})), None);
    }
}
