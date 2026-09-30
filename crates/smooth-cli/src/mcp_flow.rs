//! SmoothFlow as MCP tools (th-1efb59): Claude Desktop, Claude Code, Codex or
//! Cursor can see the fleet, start agent sessions, steer them, wait for a turn,
//! answer approvals, fan out, and close work out — the same engine the
//! SmoothFlow apps drive, over its local HTTP API (`crate::flow::call`).
//!
//! Read tools carry `read_only_hint`, so `SMOOTH_MCP_ALLOW_WRITE=0` leaves them
//! and hides every tool that changes anything (th-1d5ca8).

use std::fmt::Write as _;
use std::time::Duration;

use reqwest::Method;
use rmcp::{handler::server::wrapper::Parameters, model::ErrorData, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
// th-8b3918: the list/line/turn rules live in smooth-flow so Big Smooth's
// in-process flow tools give the model the same answers as these.
use smooth_flow::vocab::{self, session_line, turn_progress, Turn};

use crate::mcp_serve::SmoothMcp;

// ── args ────────────────────────────────────────────────────────────────────

/// Arguments for `flow_list`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowListArgs {
    /// Only sessions in this state: starting, working, idle, needs_you, limited, done, dead.
    #[serde(default)]
    pub state: Option<String>,
    /// Include finished sessions (done/dead). Default false.
    #[serde(default)]
    pub include_finished: Option<bool>,
}

/// A session id.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowIdArgs {
    /// The session id (`fs-…`) from `flow_list`.
    pub id: String,
}

/// Arguments for `flow_repos`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowReposArgs {
    /// Words to match against repo names, paths and branches. Empty lists the fleet's repos, then the most recent.
    #[serde(default)]
    pub query: Option<String>,
    /// Max results (default 20).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Arguments for `flow_infer`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowInferArgs {
    /// Directory to read context from (worktree, branch, pearl, Jira key). Default: the daemon's workspace.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Arguments for `flow_new`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowNewArgs {
    /// Harness to run: claude, codex, opencode, gemini, th-code, … (see `flow_harnesses`), or `shell`. Default claude.
    #[serde(default)]
    pub kind: Option<String>,
    /// Directory (git worktree) to run in. Find one with `flow_repos`. Default: inferred from the daemon's workspace.
    #[serde(default)]
    pub directory: Option<String>,
    /// First prompt for the agent. Omit to open it idle.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Pearl id to attach. With no directory, the engine creates a worktree for it.
    #[serde(default)]
    pub pearl_id: Option<String>,
    /// Session title.
    #[serde(default)]
    pub title: Option<String>,
    /// Model override for the harness.
    #[serde(default)]
    pub model: Option<String>,
}

/// Arguments for `flow_send` / `flow_prompt_wait`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowSendArgs {
    /// The session id.
    pub id: String,
    /// Text to type into the agent and submit.
    pub text: String,
    /// `flow_prompt_wait` only: seconds to wait for the turn to finish (default 600, max 3600).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Arguments for `flow_approve`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowApproveArgs {
    /// The session id.
    pub id: String,
    /// allow | deny | allow_session.
    pub decision: String,
    /// The pending request id (from `flow_list`'s attention). Default: the session's current one.
    #[serde(default)]
    pub request_id: Option<String>,
}

/// Arguments for `flow_kill`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowKillArgs {
    /// The session id.
    pub id: String,
    /// Relaunch it resumed after killing. Default false.
    #[serde(default)]
    pub resume: Option<bool>,
}

/// Arguments for `flow_close`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowCloseArgs {
    /// The session id.
    pub id: String,
    /// Also close its pearl. Default false.
    #[serde(default)]
    pub close_pearl: Option<bool>,
    /// Also remove its worktree once the branch is merged. Default false.
    #[serde(default)]
    pub remove_worktree: Option<bool>,
}

/// One fan-out candidate.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FanoutCandidateArg {
    /// Short label (becomes part of the branch name).
    pub label: String,
    /// Harness kind (default claude).
    #[serde(default)]
    pub kind: Option<String>,
    /// Model override.
    #[serde(default)]
    pub model: Option<String>,
}

/// Arguments for `flow_fanout_new`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowFanoutArgs {
    /// The one prompt every candidate gets.
    pub prompt: String,
    /// The pearl the race is for (each candidate gets a child pearl + worktree).
    pub pearl_id: String,
    /// The racers.
    pub candidates: Vec<FanoutCandidateArg>,
    /// Main checkout to fan out from. Default: the daemon's workspace.
    #[serde(default)]
    pub project: Option<String>,
}

/// Arguments for `flow_fanout_pick`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlowPickArgs {
    /// The fan-out id.
    pub fan_out_id: String,
    /// The winning candidate's session id.
    pub winner_session_id: String,
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn flow_err(e: &anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("SmoothFlow: {e}"), None)
}

async fn call(method: Method, path: &str, body: Option<Value>) -> Result<Value, ErrorData> {
    crate::flow::call(method, path, body).await.map_err(|e| flow_err(&e))
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

async fn session(id: &str) -> Result<Value, ErrorData> {
    let v = call(Method::GET, "/api/flow/sessions", None).await?;
    v.get("sessions")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|s| s.get("id").and_then(Value::as_str) == Some(id)).cloned())
        .ok_or_else(|| ErrorData::invalid_params(format!("no SmoothFlow session {id} — see flow_list"), None))
}

async fn screen_tail(id: &str, lines: usize) -> String {
    let Ok(v) = crate::flow::call(Method::GET, &format!("/api/flow/sessions/{id}/snapshot"), None).await else {
        return String::new();
    };
    vocab::screen_tail(v.pointer("/screen/text").and_then(Value::as_str).unwrap_or(""), lines)
}

// ── tools ───────────────────────────────────────────────────────────────────

#[tool_router(router = flow_tool_router, vis = "pub(crate)")]
impl SmoothMcp {
    /// The SmoothFlow fleet.
    ///
    /// # Errors
    /// When no flow engine is running or the request fails.
    #[tool(
        name = "flow_list",
        description = "List SmoothFlow agent sessions (Claude Code, Codex, OpenCode, shells…) with their state (working, idle, needs_you, limited, done, dead), pearl, worktree, and any pending approval with its request_id.",
        annotations(read_only_hint = true)
    )]
    pub async fn flow_list(&self, params: Parameters<FlowListArgs>) -> Result<String, ErrorData> {
        let args = params.0;
        let v = call(Method::GET, "/api/flow/sessions", None).await?;
        let all = v.get("sessions").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(vocab::render_list(&all, args.state.as_deref(), args.include_finished.unwrap_or(false)))
    }

    /// A session's current screen.
    ///
    /// # Errors
    /// When the session is unknown or the engine is unreachable.
    #[tool(
        name = "flow_snapshot",
        description = "The current terminal screen of a SmoothFlow session, as text — what the agent is showing right now.",
        annotations(read_only_hint = true)
    )]
    pub async fn flow_snapshot(&self, params: Parameters<FlowIdArgs>) -> Result<String, ErrorData> {
        let v = call(Method::GET, &format!("/api/flow/sessions/{}/snapshot", params.0.id), None).await?;
        Ok(v.pointer("/screen/text").and_then(Value::as_str).unwrap_or("(empty screen)").to_string())
    }

    /// A session's handoff packet.
    ///
    /// # Errors
    /// When the session is unknown or the engine is unreachable.
    #[tool(
        name = "flow_handoff",
        description = "A SmoothFlow session's handoff packet: its pearl, worktree, branch, recent checkpoints, and PR/CI status.",
        annotations(read_only_hint = true)
    )]
    pub async fn flow_handoff(&self, params: Parameters<FlowIdArgs>) -> Result<String, ErrorData> {
        let v = call(Method::GET, &format!("/api/flow/sessions/{}/handoff", params.0.id), None).await?;
        Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
    }

    /// The harnesses this machine can run.
    ///
    /// # Errors
    /// When the engine is unreachable.
    #[tool(
        name = "flow_harnesses",
        description = "The coding agents (harnesses) SmoothFlow can launch on this machine, whether each is installed, and any setup it still needs.",
        annotations(read_only_hint = true)
    )]
    pub async fn flow_harnesses(&self) -> Result<String, ErrorData> {
        let v = call(Method::GET, "/api/flow/harnesses", None).await?;
        Ok(vocab::render_harnesses(v.get("harnesses").and_then(Value::as_array).map_or(&[], Vec::as_slice)))
    }

    /// Search the git repos under the user's home.
    ///
    /// # Errors
    /// When the engine is unreachable.
    #[tool(
        name = "flow_repos",
        description = "Search every git repo and worktree under the user's home folder by name, path or branch — use it to pick the directory for flow_new.",
        annotations(read_only_hint = true)
    )]
    pub async fn flow_repos(&self, params: Parameters<FlowReposArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        let q = urlencoding::encode(a.query.as_deref().unwrap_or(""));
        let v = call(Method::GET, &format!("/api/flow/repos?q={q}&limit={}", a.limit.unwrap_or(20)), None).await?;
        Ok(vocab::render_repos(&v))
    }

    /// The context a new session in `cwd` would inherit.
    ///
    /// # Errors
    /// When the engine is unreachable.
    #[tool(
        name = "flow_infer",
        description = "What SmoothFlow infers for a directory: its worktree, project, branch, pearl and Jira key — the context a new session there would carry.",
        annotations(read_only_hint = true)
    )]
    pub async fn flow_infer(&self, params: Parameters<FlowInferArgs>) -> Result<String, ErrorData> {
        let path = params
            .0
            .cwd
            .map_or_else(|| "/api/flow/infer".to_string(), |c| format!("/api/flow/infer?cwd={}", urlencoding::encode(&c)));
        let v = call(Method::GET, &path, None).await?;
        Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
    }

    /// Start a session.
    ///
    /// # Errors
    /// When the harness is not installed, the directory is invalid, or the engine is unreachable.
    #[tool(
        name = "flow_new",
        description = "Start a new SmoothFlow session: a coding agent (claude by default, or codex, opencode, gemini, th-code…) or a shell, in a directory, optionally with a first prompt and a pearl. Returns the session id."
    )]
    pub async fn flow_new(&self, params: Parameters<FlowNewArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        let body = json!({
            "kind": a.kind.as_deref().unwrap_or("claude"),
            "worktree": a.directory,
            "pearl_id": a.pearl_id,
            "prompt": a.prompt,
            "title": a.title,
            "model": a.model,
        });
        let v = call(Method::POST, "/api/flow/sessions", Some(body)).await?;
        let sess = v.get("session").cloned().unwrap_or(Value::Null);
        Ok(format!("Started {}\n{}", s(&sess, "id"), session_line(&sess)))
    }

    /// Type text into a session and submit it.
    ///
    /// # Errors
    /// When the session is unknown, not live, or the engine is unreachable.
    #[tool(
        name = "flow_send",
        description = "Send a message to a running SmoothFlow session (typed into the agent and submitted). Returns immediately; use flow_prompt_wait to wait for the reply."
    )]
    pub async fn flow_send(&self, params: Parameters<FlowSendArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        call(Method::POST, &format!("/api/flow/sessions/{}/send", a.id), Some(json!({ "text": a.text }))).await?;
        Ok(format!("Sent to {}.", a.id))
    }

    /// Send a prompt and wait for the turn to finish.
    ///
    /// # Errors
    /// When the session is blocked on an approval, unknown, or the engine is unreachable.
    #[tool(
        name = "flow_prompt_wait",
        description = "Send a prompt to a SmoothFlow agent and wait until its turn ends (idle, needs_you, limited, done). Refuses when the agent is waiting on an approval. Returns the final state and the last lines of its screen."
    )]
    pub async fn flow_prompt_wait(&self, params: Parameters<FlowSendArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        let before = session(&a.id).await?;
        if let Some(why) = vocab::prompt_refusal(&a.id, s(&before, "state")) {
            return Err(ErrorData::invalid_request(why, None));
        }
        call(Method::POST, &format!("/api/flow/sessions/{}/send", a.id), Some(json!({ "text": a.text }))).await?;
        let timeout = vocab::prompt_wait_timeout(a.timeout_secs);
        let stall_after = Duration::from_secs(vocab::PROMPT_STALL_SECS).min(timeout);
        let sent = tokio::time::Instant::now();
        let mut started = false;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let now = session(&a.id).await?;
            let state = s(&now, "state").to_string();
            started |= state == "working";
            match turn_progress(started, &state, sent.elapsed(), stall_after) {
                Turn::Running if sent.elapsed() < timeout => {}
                Turn::Running => {
                    return Ok(format!(
                        "Still {state} after {}s — the turn is running; check again with flow_list or flow_snapshot.\n\n{}",
                        timeout.as_secs(),
                        screen_tail(&a.id, 20).await
                    ))
                }
                Turn::Stalled => {
                    return Ok(format!(
                    "The agent never started working within {}s of the prompt (state: {state}). It may not have received it — look with flow_snapshot.\n\n{}",
                    stall_after.as_secs(),
                    screen_tail(&a.id, 20).await
                ))
                }
                Turn::Settled => {
                    let mut out = format!("Turn ended: {}\n", session_line(&now));
                    let _ = write!(out, "\n{}", screen_tail(&a.id, 40).await);
                    return Ok(out);
                }
            }
        }
    }

    /// Answer a pending approval.
    ///
    /// # Errors
    /// For an unknown decision, no pending request, or an unreachable engine.
    #[tool(
        name = "flow_approve",
        description = "Answer a SmoothFlow agent's pending permission request: allow, deny, or allow_session. ONLY with the user's explicit consent — relay what the agent wants to do and let the user decide.",
        annotations(destructive_hint = true)
    )]
    pub async fn flow_approve(&self, params: Parameters<FlowApproveArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        if !matches!(a.decision.as_str(), "allow" | "deny" | "allow_session") {
            return Err(ErrorData::invalid_params(
                format!("decision must be allow, deny or allow_session, not {}", a.decision),
                None,
            ));
        }
        let request_id = match a.request_id {
            Some(r) => r,
            None => session(&a.id)
                .await?
                .pointer("/attention/request_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| ErrorData::invalid_request(format!("{} has no pending approval", a.id), None))?,
        };
        call(
            Method::POST,
            &format!("/api/flow/sessions/{}/approve", a.id),
            Some(json!({ "request_id": request_id, "decision": a.decision })),
        )
        .await?;
        Ok(format!("Answered {} with {}.", a.id, a.decision))
    }

    /// Kill a session (optionally relaunching it resumed).
    ///
    /// # Errors
    /// When the session is unknown or the engine is unreachable.
    #[tool(
        name = "flow_kill",
        description = "Stop a SmoothFlow session's process (an agent killed mid-turn loses the work in flight). resume=true relaunches it resumed. Confirm with the user first.",
        annotations(destructive_hint = true)
    )]
    pub async fn flow_kill(&self, params: Parameters<FlowKillArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        call(
            Method::POST,
            &format!("/api/flow/sessions/{}/kill", a.id),
            Some(json!({ "resume": a.resume.unwrap_or(false) })),
        )
        .await?;
        Ok(format!("{} {}.", a.id, if a.resume.unwrap_or(false) { "restarted" } else { "stopped" }))
    }

    /// Close a session out.
    ///
    /// # Errors
    /// When the session is unknown or the engine is unreachable.
    #[tool(
        name = "flow_close",
        description = "Close a SmoothFlow session out: kill it if live and drop it from the fleet; optionally close its pearl and remove its worktree once the branch is merged. Confirm with the user first.",
        annotations(destructive_hint = true)
    )]
    pub async fn flow_close(&self, params: Parameters<FlowCloseArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        let v = call(
            Method::POST,
            &format!("/api/flow/sessions/{}/close", a.id),
            Some(json!({ "close_pearl": a.close_pearl.unwrap_or(false), "remove_worktree": a.remove_worktree.unwrap_or(false) })),
        )
        .await?;
        Ok(format!("Closed {}.\n{}", a.id, serde_json::to_string_pretty(&v).unwrap_or_default()))
    }

    /// Race several agents on one prompt.
    ///
    /// # Errors
    /// When the pearl or project is invalid or the engine is unreachable.
    #[tool(
        name = "flow_fanout_new",
        description = "Race several agents on one prompt: each candidate gets its own worktree and child pearl. Pick the winner later with flow_fanout_pick."
    )]
    pub async fn flow_fanout_new(&self, params: Parameters<FlowFanoutArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        let candidates: Vec<Value> = a
            .candidates
            .iter()
            .map(|c| json!({ "label": c.label, "kind": c.kind.as_deref().unwrap_or("claude"), "model": c.model }))
            .collect();
        let v = call(
            Method::POST,
            "/api/flow/fanout",
            Some(json!({ "prompt": a.prompt, "pearl_id": a.pearl_id, "candidates": candidates, "project": a.project })),
        )
        .await?;
        let mut out = format!("Fan-out {} started:\n", v.pointer("/fan_out/id").and_then(Value::as_str).unwrap_or("?"));
        for c in v.get("candidates").and_then(Value::as_array).cloned().unwrap_or_default() {
            let _ = writeln!(out, "{}", session_line(&c));
        }
        Ok(out)
    }

    /// Pick a fan-out's winner.
    ///
    /// # Errors
    /// When the fan-out or winner is unknown, or the merge fails.
    #[tool(
        name = "flow_fanout_pick",
        description = "Pick a fan-out's winner: its branch is merged, the other candidates' worktrees and child pearls are cleaned up (transcripts kept). Confirm with the user first.",
        annotations(destructive_hint = true)
    )]
    pub async fn flow_fanout_pick(&self, params: Parameters<FlowPickArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        call(
            Method::POST,
            &format!("/api/flow/fanout/{}/pick", a.fan_out_id),
            Some(json!({ "winner_session_id": a.winner_session_id })),
        )
        .await?;
        Ok(format!("{} won fan-out {}.", a.winner_session_id, a.fan_out_id))
    }
}

// The list/line/turn rules these tools share with Big Smooth's in-process
// flow tools are tested where they live: `smooth_flow::vocab`.
