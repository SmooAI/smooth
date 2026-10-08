//! Big Smooth drives the SmoothFlow fleet (th-8b3918).
//!
//! The `flow_*` tools and `project_setup` are registered on the operator's per-turn registry so the
//! in-process agent can set up projects, start and steer coding agents, and
//! watch the fleet — the same things a person does in the SmoothFlow app.
//!
//! These call the flow [`Engine`] **directly**, in-process: no HTTP, no token.
//! The names, argument schemas and answers match the MCP tools in
//! `smooth-cli/src/mcp_flow.rs` (`th mcp serve`), and the list/line/turn rules
//! are shared through [`smooth_flow::vocab`], so a model sees one vocabulary
//! whichever door it came through.
//!
//! # Permission classes
//!
//! | class        | tools                                                                   | gate                                   |
//! | ------------ | ----------------------------------------------------------------------- | -------------------------------------- |
//! | read         | [`FLOW_READ_TOOLS`]                                                     | none; kept in Plan mode                |
//! | write        | [`FLOW_WRITE_TOOLS`]                                                    | confirm (HITL); dropped in Plan mode   |
//! | destructive  | [`FLOW_DESTRUCTIVE_TOOLS`]                                              | confirm (HITL); dropped in Plan mode   |
//! | approve      | [`FLOW_APPROVE_TOOL`]                                                   | confirm (HITL), always; dropped in Plan |
//!
//! The confirm gate is the daemon's `CONFIRM_TOOLS` floor (`operator.rs`),
//! which core's `ConfirmationHook` enforces on every call regardless of the
//! auto-mode posture — `SMOOTH_AUTO_MODE=bypass` does not skip it, and the env
//! can widen the list but never shrink it. The permission gate and Narc see
//! these calls like any other tool.
//!
//! A flow session is a process on this host, outside the kernel sandbox that
//! confines `bash` (that is what an agent session IS), and `project_setup`'s
//! `git clone` runs on the host too. Both are why every write parks for the
//! user.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use smooth_flow::protocol::CandidateSpec;
use smooth_flow::vocab::{self, session_line, turn_progress, Turn};
use smooth_flow::{Decision, Engine, NewRequest, ServerFrame, Session, SessionKind};
use smooth_operator::{Tool, ToolSchema};
use tokio::sync::broadcast;

/// Read-only: no confirmation, and they survive the Plan-mode filter.
pub const FLOW_READ_TOOLS: &[&str] = &["flow_list", "flow_snapshot", "flow_handoff", "flow_harnesses", "flow_repos", "flow_infer"];
/// Start or steer work. Confirm-gated; gone in Plan mode.
pub const FLOW_WRITE_TOOLS: &[&str] = &["flow_new", "flow_send", "flow_prompt_wait", "flow_fanout_new", "project_setup"];
/// Lose work in flight or can't be undone (kill, close, merge-and-GC).
/// Confirm-gated; gone in Plan mode.
pub const FLOW_DESTRUCTIVE_TOOLS: &[&str] = &["flow_kill", "flow_close", "flow_fanout_pick"];
/// Answering another agent's permission prompt: the user's call, always.
pub const FLOW_APPROVE_TOOL: &str = "flow_approve";

/// How `flow_prompt_wait` polls. Production uses [`WaitTiming::default`];
/// tests shrink it so a stall is seconds, not a minute.
#[derive(Debug, Clone, Copy)]
pub struct WaitTiming {
    /// How often the session row is re-read (state frames arrive in between).
    pub poll: Duration,
    /// A turn that has not started working this long after the send stalled.
    pub stall_after: Duration,
}

impl Default for WaitTiming {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(2),
            stall_after: Duration::from_secs(vocab::PROMPT_STALL_SECS),
        }
    }
}

/// Every flow tool over `engine`. `workspace` is the turn's cwd: relative
/// paths in arguments resolve against it.
#[must_use]
pub fn flow_tools(engine: &Engine, workspace: &Path) -> Vec<Arc<dyn Tool>> {
    flow_tools_with_timing(engine, workspace, WaitTiming::default())
}

/// [`flow_tools`] with explicit `flow_prompt_wait` timing.
#[must_use]
pub fn flow_tools_with_timing(engine: &Engine, workspace: &Path, timing: WaitTiming) -> Vec<Arc<dyn Tool>> {
    let ctx = Arc::new(Ctx {
        engine: engine.clone(),
        workspace: workspace.to_path_buf(),
        timing,
    });
    Verb::ALL
        .iter()
        .map(|&verb| Arc::new(FlowTool { verb, ctx: Arc::clone(&ctx) }) as Arc<dyn Tool>)
        .collect()
}

struct Ctx {
    engine: Engine,
    workspace: PathBuf,
    timing: WaitTiming,
}

/// Deadline for the flow verbs that create sessions, worktrees or clones.
const FLOW_SETUP_TIMEOUT: Duration = Duration::from_mins(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    List,
    Snapshot,
    Handoff,
    Harnesses,
    Repos,
    Infer,
    New,
    Send,
    PromptWait,
    Approve,
    Kill,
    Close,
    FanoutNew,
    FanoutPick,
    ProjectSetup,
}

impl Verb {
    const ALL: [Self; 15] = [
        Self::List,
        Self::Snapshot,
        Self::Handoff,
        Self::Harnesses,
        Self::Repos,
        Self::Infer,
        Self::New,
        Self::Send,
        Self::PromptWait,
        Self::Approve,
        Self::Kill,
        Self::Close,
        Self::FanoutNew,
        Self::FanoutPick,
        Self::ProjectSetup,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::List => "flow_list",
            Self::Snapshot => "flow_snapshot",
            Self::Handoff => "flow_handoff",
            Self::Harnesses => "flow_harnesses",
            Self::Repos => "flow_repos",
            Self::Infer => "flow_infer",
            Self::New => "flow_new",
            Self::Send => "flow_send",
            Self::PromptWait => "flow_prompt_wait",
            Self::Approve => "flow_approve",
            Self::Kill => "flow_kill",
            Self::Close => "flow_close",
            Self::FanoutNew => "flow_fanout_new",
            Self::FanoutPick => "flow_fanout_pick",
            Self::ProjectSetup => "project_setup",
        }
    }

    const fn is_read_only(self) -> bool {
        matches!(self, Self::List | Self::Snapshot | Self::Handoff | Self::Harnesses | Self::Repos | Self::Infer)
    }

    /// The engine's per-call deadline for this verb (core 1.14.2 bounds every
    /// sequential tool call, 120s by default). `flow_prompt_wait` bounds itself
    /// (its `timeout_secs`, at most [`vocab::PROMPT_WAIT_MAX_SECS`]), so its
    /// deadline sits just past that cap and the wait's own timeout reports first.
    /// Starting sessions, fanning out and `project_setup` create worktrees or
    /// clone repos on a blocking thread the engine can't cancel, so they get a
    /// long bound rather than a misleading 120s "timed out" mid-clone.
    fn timeout(self) -> Duration {
        match self {
            Self::PromptWait => Duration::from_secs(vocab::PROMPT_WAIT_MAX_SECS + 120),
            Self::New | Self::FanoutNew | Self::ProjectSetup => FLOW_SETUP_TIMEOUT,
            _ => smooth_operator::tool::DEFAULT_TOOL_TIMEOUT,
        }
    }

    /// The MCP tools' descriptions, verbatim where a tool exists there.
    const fn description(self) -> &'static str {
        match self {
            Self::List => "List SmoothFlow agent sessions (Claude Code, Codex, OpenCode, shells…) with their state (working, idle, needs_you, limited, done, dead), pearl, worktree, and any pending approval with its request_id.",
            Self::Snapshot => "The current terminal screen of a SmoothFlow session, as text — what the agent is showing right now.",
            Self::Handoff => "A SmoothFlow session's handoff packet: its pearl, worktree, branch, recent checkpoints, and PR/CI status.",
            Self::Harnesses => "The coding agents (harnesses) SmoothFlow can launch on this machine, whether each is installed, and any setup it still needs.",
            Self::Repos => "Search every git repo and worktree under the user's home folder by name, path or branch — use it to pick the directory for flow_new.",
            Self::Infer => "What SmoothFlow infers for a directory: its worktree, project, branch, pearl and Jira key — the context a new session there would carry.",
            Self::New => "Start a new SmoothFlow session: a coding agent (claude by default, or codex, opencode, gemini, th-code…) or a shell, in a directory, optionally with a first prompt and a pearl. Returns the session id.",
            Self::Send => "Send a message to a running SmoothFlow session (typed into the agent and submitted). Returns immediately; use flow_prompt_wait to wait for the reply.",
            Self::PromptWait => "Send a prompt to a SmoothFlow agent and wait until its turn ends (idle, needs_you, limited, done). Refuses when the agent is waiting on an approval. Returns the final state and the last lines of its screen.",
            Self::Approve => "Answer a SmoothFlow agent's pending permission request: allow, deny, or allow_session. ONLY with the user's explicit consent — relay what the agent wants to do and let the user decide.",
            Self::Kill => "Stop a SmoothFlow session's process (an agent killed mid-turn loses the work in flight). resume=true relaunches it resumed. Confirm with the user first.",
            Self::Close => "Close a SmoothFlow session out: kill it if live and drop it from the fleet; optionally close its pearl and remove its worktree once the branch is merged. Confirm with the user first.",
            Self::FanoutNew => "Race several agents on one prompt: each candidate gets its own worktree and child pearl. Pick the winner later with flow_fanout_pick.",
            Self::FanoutPick => "Pick a fan-out's winner: its branch is merged, the other candidates' worktrees and child pearls are cleaned up (transcripts kept). Confirm with the user first.",
            Self::ProjectSetup => "Set up a project and start a coding agent on it in one step: take a local repo path, or a git URL to clone under ~/dev (or clone_into); with a pearl_id or branch, create a worktree for the work; then start the harness (claude by default) there, optionally with a first prompt. Returns the checkout, the worktree and the session id. An agent started with a prompt is already working — watch it with flow_list, don't prompt it again.",
        }
    }

    /// JSON Schema for the arguments — the MCP tools' `schemars` shapes.
    #[allow(clippy::too_many_lines, reason = "one flat schema per tool reads best side by side")]
    fn parameters(self) -> Value {
        let id = json!({ "type": "string", "description": "The session id (`fs-…`) from `flow_list`." });
        match self {
            Self::List => json!({
                "type": "object",
                "properties": {
                    "state": { "type": "string", "description": "Only sessions in this state: starting, working, idle, needs_you, limited, done, dead." },
                    "include_finished": { "type": "boolean", "description": "Include finished sessions (done/dead). Default false." }
                }
            }),
            Self::Snapshot | Self::Handoff => json!({ "type": "object", "properties": { "id": id }, "required": ["id"] }),
            Self::Harnesses => json!({ "type": "object", "properties": {} }),
            Self::Repos => json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words to match against repo names, paths and branches. Empty lists the fleet's repos, then the most recent." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Max results (default 20)." }
                }
            }),
            Self::Infer => json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string", "description": "Directory to read context from (worktree, branch, pearl, Jira key). Default: the daemon's workspace." }
                }
            }),
            Self::New => json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "Harness to run: claude, codex, opencode, gemini, th-code, … (see `flow_harnesses`), or `shell`. Default claude." },
                    "directory": { "type": "string", "description": "Directory (git worktree) to run in. Find one with `flow_repos`. Default: inferred from the daemon's workspace." },
                    "prompt": { "type": "string", "description": "First prompt for the agent. Omit to open it idle." },
                    "pearl_id": { "type": "string", "description": "Pearl id to attach. With no directory, the engine creates a worktree for it." },
                    "title": { "type": "string", "description": "Session title." },
                    "model": { "type": "string", "description": "Model override for the harness." }
                }
            }),
            Self::Send => json!({
                "type": "object",
                "properties": { "id": id, "text": { "type": "string", "description": "Text to type into the agent and submit." } },
                "required": ["id", "text"]
            }),
            Self::PromptWait => json!({
                "type": "object",
                "properties": {
                    "id": id,
                    "text": { "type": "string", "description": "Text to type into the agent and submit." },
                    "timeout_secs": { "type": "integer", "minimum": 0, "description": "Seconds to wait for the turn to finish (default 600, max 3600)." }
                },
                "required": ["id", "text"]
            }),
            Self::Approve => json!({
                "type": "object",
                "properties": {
                    "id": id,
                    "decision": { "type": "string", "enum": ["allow", "deny", "allow_session"], "description": "allow | deny | allow_session." },
                    "request_id": { "type": "string", "description": "The pending request id (from `flow_list`'s attention). Default: the session's current one." }
                },
                "required": ["id", "decision"]
            }),
            Self::Kill => json!({
                "type": "object",
                "properties": { "id": id, "resume": { "type": "boolean", "description": "Relaunch it resumed after killing. Default false." } },
                "required": ["id"]
            }),
            Self::Close => json!({
                "type": "object",
                "properties": {
                    "id": id,
                    "close_pearl": { "type": "boolean", "description": "Also close its pearl. Default false." },
                    "remove_worktree": { "type": "boolean", "description": "Also remove its worktree once the branch is merged. Default false." }
                },
                "required": ["id"]
            }),
            Self::FanoutNew => json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "The one prompt every candidate gets." },
                    "pearl_id": { "type": "string", "description": "The pearl the race is for (each candidate gets a child pearl + worktree)." },
                    "candidates": {
                        "type": "array",
                        "description": "The racers.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": { "type": "string", "description": "Short label (becomes part of the branch name)." },
                                "kind": { "type": "string", "description": "Harness kind (default claude)." },
                                "model": { "type": "string", "description": "Model override." }
                            },
                            "required": ["label"]
                        }
                    },
                    "project": { "type": "string", "description": "Main checkout to fan out from. Default: the daemon's workspace." }
                },
                "required": ["prompt", "pearl_id", "candidates"]
            }),
            Self::FanoutPick => json!({
                "type": "object",
                "properties": {
                    "fan_out_id": { "type": "string", "description": "The fan-out id." },
                    "winner_session_id": { "type": "string", "description": "The winning candidate's session id." }
                },
                "required": ["fan_out_id", "winner_session_id"]
            }),
            Self::ProjectSetup => json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "A local repo path (absolute, ~/…, or relative to the workspace), or a git URL (https://, ssh://, git@host:owner/repo, file://) to clone." },
                    "clone_into": { "type": "string", "description": "Directory a git URL is cloned under. Default ~/dev." },
                    "pearl_id": { "type": "string", "description": "Pearl the work is for. Creates the worktree ../<repo>-<pearl>-<slug> on a new branch (unless branch is given)." },
                    "branch": { "type": "string", "description": "Branch to work on: a worktree ../<repo>-<branch> checked out on it (created from HEAD when it doesn't exist)." },
                    "kind": { "type": "string", "description": "Harness to start: claude, codex, opencode, gemini, th-code, … (see `flow_harnesses`), or `shell`. Default claude." },
                    "prompt": { "type": "string", "description": "First prompt for the agent. Omit to open it idle." },
                    "title": { "type": "string", "description": "Session title — what the user reads in the sidebar." },
                    "model": { "type": "string", "description": "Model override for the harness." }
                },
                "required": ["repo"]
            }),
        }
    }
}

/// One flow tool: a [`Verb`] over the shared engine handle.
struct FlowTool {
    verb: Verb,
    ctx: Arc<Ctx>,
}

#[async_trait]
impl Tool for FlowTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.verb.name().into(),
            description: self.verb.description().into(),
            parameters: self.verb.parameters(),
        }
    }

    fn is_read_only(&self) -> bool {
        self.verb.is_read_only()
    }

    fn is_concurrent_safe(&self) -> bool {
        // Reads are independent; a write changes the fleet another call may
        // be about to read, so writes go one at a time.
        self.verb.is_read_only()
    }

    fn timeout(&self) -> Option<Duration> {
        Some(self.verb.timeout())
    }

    async fn execute(&self, arguments: Value) -> Result<String> {
        let args = if arguments.is_null() { json!({}) } else { arguments };
        match self.verb {
            Verb::List => self.list(parse(args)?).await,
            Verb::Snapshot => {
                let a: IdArgs = parse(args)?;
                let text = screen(&self.ctx.engine, a.id).await?;
                Ok(if text.is_empty() { "(empty screen)".to_string() } else { text })
            }
            Verb::Handoff => {
                let a: IdArgs = parse(args)?;
                let v = blocking(&self.ctx.engine, move |e| e.handoff(&a.id)).await?;
                Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
            }
            Verb::Harnesses => {
                let hs = blocking(&self.ctx.engine, |e| e.harnesses(true)).await?;
                let rows = serde_json::to_value(hs)?;
                Ok(vocab::render_harnesses(rows.as_array().map_or(&[], Vec::as_slice)))
            }
            Verb::Repos => {
                let a: ReposArgs = parse(args)?;
                let limit = usize::try_from(a.limit.unwrap_or(20)).unwrap_or(20).clamp(1, 200);
                let q = a.query.unwrap_or_default();
                let list = blocking(&self.ctx.engine, move |e| e.repos(&q, limit)).await?;
                Ok(vocab::render_repos(&serde_json::to_value(list)?))
            }
            Verb::Infer => {
                let a: InferArgs = parse(args)?;
                let cwd = a.cwd.map(|c| self.ctx.resolve(&c));
                let inferred = blocking(&self.ctx.engine, move |e| Ok(e.infer_context(cwd.as_deref()))).await?;
                Ok(serde_json::to_string_pretty(&inferred).unwrap_or_default())
            }
            Verb::New => self.new_session(parse(args)?).await,
            Verb::Send => {
                let a: SendArgs = parse(args)?;
                let id = a.id.clone();
                blocking(&self.ctx.engine, move |e| e.send(&a.id, &a.text)).await?;
                Ok(format!("Sent to {id}."))
            }
            Verb::PromptWait => self.prompt_wait(parse(args)?).await,
            Verb::Approve => self.approve(parse(args)?).await,
            Verb::Kill => {
                let a: KillArgs = parse(args)?;
                let resume = a.resume.unwrap_or(false);
                let id = a.id.clone();
                blocking(&self.ctx.engine, move |e| e.kill(&a.id, resume)).await?;
                Ok(format!("{id} {}.", if resume { "restarted" } else { "stopped" }))
            }
            Verb::Close => {
                let a: CloseArgs = parse(args)?;
                let id = a.id.clone();
                // Never `force`: a dirty or unmerged worktree is reported, and
                // forcing past that is the user's call from the app or CLI.
                let out = blocking(&self.ctx.engine, move |e| {
                    e.close(&a.id, a.close_pearl.unwrap_or(false), a.remove_worktree.unwrap_or(false), false)
                })
                .await?;
                Ok(format!("Closed {id}.\n{}", serde_json::to_string_pretty(&out).unwrap_or_default()))
            }
            Verb::FanoutNew => self.fanout_new(parse(args)?).await,
            Verb::FanoutPick => {
                let a: PickArgs = parse(args)?;
                let (fan, win) = (a.fan_out_id.clone(), a.winner_session_id.clone());
                blocking(&self.ctx.engine, move |e| e.fanout_pick(&a.fan_out_id, &a.winner_session_id)).await?;
                Ok(format!("{win} won fan-out {fan}."))
            }
            Verb::ProjectSetup => self.project_setup(parse(args)?).await,
        }
    }
}

// ── arguments (the MCP `*Args` structs, field for field) ─────────────────────

#[derive(Debug, Deserialize)]
struct ListArgs {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    include_finished: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct IdArgs {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ReposArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct InferArgs {
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NewArgs {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    directory: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    pearl_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SendArgs {
    id: String,
    text: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ApproveArgs {
    id: String,
    decision: String,
    #[serde(default)]
    request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KillArgs {
    id: String,
    #[serde(default)]
    resume: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CloseArgs {
    id: String,
    #[serde(default)]
    close_pearl: Option<bool>,
    #[serde(default)]
    remove_worktree: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CandidateArg {
    label: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FanoutArgs {
    prompt: String,
    pearl_id: String,
    candidates: Vec<CandidateArg>,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PickArgs {
    fan_out_id: String,
    winner_session_id: String,
}

#[derive(Debug, Deserialize)]
struct SetupArgs {
    repo: String,
    #[serde(default)]
    clone_into: Option<String>,
    #[serde(default)]
    pearl_id: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

fn parse<T: serde::de::DeserializeOwned>(args: Value) -> Result<T> {
    serde_json::from_value(args).context("invalid arguments")
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Run a (blocking: git, sqlite, sleeps) engine call off the async runtime.
async fn blocking<T: Send + 'static>(engine: &Engine, f: impl FnOnce(Engine) -> Result<T> + Send + 'static) -> Result<T> {
    let e = engine.clone();
    tokio::task::spawn_blocking(move || f(e))
        .await
        .map_err(|e| anyhow!("flow task panicked: {e}"))?
}

fn row(s: &Session) -> Value {
    serde_json::to_value(s).unwrap_or(Value::Null)
}

async fn session(engine: &Engine, id: String) -> Result<Session> {
    let sid = id.clone();
    blocking(engine, move |e| e.get(&sid))
        .await?
        .ok_or_else(|| anyhow!("no SmoothFlow session {id} — see flow_list"))
}

async fn screen(engine: &Engine, id: String) -> Result<String> {
    match blocking(engine, move |e| e.snapshot(&id)).await? {
        ServerFrame::Screen { text, .. } => Ok(text),
        _ => Ok(String::new()),
    }
}

async fn screen_tail(engine: &Engine, id: &str, lines: usize) -> String {
    screen(engine, id.to_string()).await.map(|t| vocab::screen_tail(&t, lines)).unwrap_or_default()
}

fn kind_or_claude(kind: Option<&str>) -> Result<SessionKind> {
    kind.filter(|k| !k.trim().is_empty()).unwrap_or("claude").parse()
}

/// The next `flow.session` state for `id` from the broadcast, or `None` for
/// any other frame. A closed channel parks forever (the poll takes over).
async fn next_state(rx: &mut Option<broadcast::Receiver<ServerFrame>>, id: &str) -> Option<String> {
    let Some(r) = rx.as_mut() else {
        return std::future::pending().await;
    };
    match r.recv().await {
        Ok(ServerFrame::Session { session }) if session.id == id => Some(session.state.as_str().to_string()),
        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => None,
        Err(broadcast::error::RecvError::Closed) => {
            *rx = None;
            None
        }
    }
}

impl Ctx {
    /// `~/…` against the engine's home, relative against the turn's cwd.
    fn resolve(&self, p: &str) -> PathBuf {
        let p = p.trim();
        if p == "~" {
            return self.engine.home().to_path_buf();
        }
        if let Some(rest) = p.strip_prefix("~/") {
            return self.engine.home().join(rest);
        }
        let path = Path::new(p);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace.join(path)
        }
    }
}

impl FlowTool {
    async fn list(&self, a: ListArgs) -> Result<String> {
        let all = blocking(&self.ctx.engine, |e| e.list()).await?;
        let rows: Vec<Value> = all.iter().map(row).collect();
        Ok(vocab::render_list(&rows, a.state.as_deref(), a.include_finished.unwrap_or(false)))
    }

    async fn new_session(&self, a: NewArgs) -> Result<String> {
        let req = NewRequest {
            kind: kind_or_claude(a.kind.as_deref())?,
            worktree: a.directory.map(|d| self.ctx.resolve(&d).to_string_lossy().into_owned()),
            pearl_id: a.pearl_id,
            prompt: a.prompt,
            title: a.title,
            model: a.model,
            ..NewRequest::default()
        };
        let s = blocking(&self.ctx.engine, move |e| e.new_session(req)).await?;
        Ok(format!("Started {}\n{}", s.id, session_line(&row(&s))))
    }

    /// Send, then wait for the turn to end. State comes from the engine's
    /// `flow.session` broadcast (so a `working` blip between polls is still
    /// seen) with a periodic re-read as the backstop.
    async fn prompt_wait(&self, a: SendArgs) -> Result<String> {
        let engine = &self.ctx.engine;
        let before = session(engine, a.id.clone()).await?;
        if let Some(why) = vocab::prompt_refusal(&a.id, before.state.as_str()) {
            bail!(why);
        }
        let timeout = vocab::prompt_wait_timeout(a.timeout_secs);
        let stall_after = self.ctx.timing.stall_after.min(timeout);
        // Subscribe BEFORE the send so no transition is missed.
        let mut rx = Some(engine.subscribe());
        let (id, text) = (a.id.clone(), a.text);
        blocking(engine, move |e| e.send(&id, &text)).await?;
        let sent = tokio::time::Instant::now();
        let mut poll = tokio::time::interval(self.ctx.timing.poll);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut state = before.state.as_str().to_string();
        let mut started = false;
        loop {
            tokio::select! {
                s = next_state(&mut rx, &a.id) => {
                    if let Some(s) = s {
                        state = s;
                    }
                }
                _ = poll.tick() => {
                    state = session(engine, a.id.clone()).await?.state.as_str().to_string();
                }
            }
            started |= state == "working";
            match turn_progress(started, &state, sent.elapsed(), stall_after) {
                Turn::Running if sent.elapsed() < timeout => {}
                Turn::Running => {
                    return Ok(format!(
                        "Still {state} after {}s — the turn is running; check again with flow_list or flow_snapshot.\n\n{}",
                        timeout.as_secs(),
                        screen_tail(engine, &a.id, 20).await
                    ));
                }
                Turn::Stalled => {
                    return Ok(format!(
                        "The agent never started working within {}s of the prompt (state: {state}). It may not have received it — look with flow_snapshot.\n\n{}",
                        stall_after.as_secs(),
                        screen_tail(engine, &a.id, 20).await
                    ));
                }
                Turn::Settled => {
                    let now = session(engine, a.id.clone()).await?;
                    let mut out = format!("Turn ended: {}\n", session_line(&row(&now)));
                    let _ = write!(out, "\n{}", screen_tail(engine, &a.id, 40).await);
                    return Ok(out);
                }
            }
        }
    }

    async fn approve(&self, a: ApproveArgs) -> Result<String> {
        let decision: Decision = match a.decision.as_str() {
            "allow" => Decision::Allow,
            "deny" => Decision::Deny,
            "allow_session" => Decision::AllowSession,
            other => bail!("decision must be allow, deny or allow_session, not {other}"),
        };
        let request_id = match a.request_id.filter(|r| !r.is_empty()) {
            Some(r) => r,
            None => session(&self.ctx.engine, a.id.clone())
                .await?
                .attention
                .and_then(|att| att.request_id)
                .ok_or_else(|| anyhow!("{} has no pending approval", a.id))?,
        };
        let id = a.id.clone();
        blocking(&self.ctx.engine, move |e| e.approve(&id, &request_id, decision)).await?;
        Ok(format!("Answered {} with {}.", a.id, a.decision))
    }

    async fn fanout_new(&self, a: FanoutArgs) -> Result<String> {
        let candidates = a
            .candidates
            .into_iter()
            .map(|c| {
                Ok(CandidateSpec {
                    kind: kind_or_claude(c.kind.as_deref())?,
                    model: c.model,
                    label: c.label,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let project = a.project.map(|p| self.ctx.resolve(&p).to_string_lossy().into_owned());
        let (fan, sessions) = blocking(&self.ctx.engine, move |e| e.fanout_new(&a.prompt, &a.pearl_id, &candidates, project.as_deref())).await?;
        let mut out = format!("Fan-out {} started:\n", fan.id);
        for s in &sessions {
            let _ = writeln!(out, "{}", session_line(&row(s)));
        }
        Ok(out)
    }

    async fn project_setup(&self, a: SetupArgs) -> Result<String> {
        let kind = kind_or_claude(a.kind.as_deref())?;
        if let Some(p) = a.pearl_id.as_deref() {
            if !valid_pearl_id(p) {
                bail!("not a pearl id: {p:?}");
            }
        }
        let branch = a.branch.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
        let source = a.repo.trim().to_string();
        if source.is_empty() {
            bail!("repo is required: a local path or a git URL");
        }
        let clone_root = a
            .clone_into
            .as_deref()
            .map_or_else(|| self.ctx.engine.home().join("dev"), |d| self.ctx.resolve(d));
        let local = (!is_git_url(&source)).then(|| self.ctx.resolve(&source));
        let engine = self.ctx.engine.clone();
        let (pearl_id, prompt, title, model) = (a.pearl_id, a.prompt, a.title, a.model);
        let has_prompt = prompt.as_deref().is_some_and(|p| !p.trim().is_empty());
        let report = tokio::task::spawn_blocking(move || -> Result<String> {
            let mut out = String::new();
            // 1. The checkout: the local path, or a clone of the URL / bare repo.
            let checkout = match local {
                Some(path) if path.is_dir() && !is_bare_repo(&path) => {
                    let _ = writeln!(out, "Project: {}", path.display());
                    path
                }
                Some(path) if path.is_dir() => {
                    let (dest, fresh) = clone(&path.to_string_lossy(), &clone_root)?;
                    let _ = writeln!(
                        out,
                        "Project: {} ({} {})",
                        dest.display(),
                        if fresh { "cloned from" } else { "already cloned from" },
                        path.display()
                    );
                    dest
                }
                Some(path) => bail!("no such directory: {} — give a repo path or a git URL", path.display()),
                None => {
                    let (dest, fresh) = clone(&source, &clone_root)?;
                    let _ = writeln!(
                        out,
                        "Project: {} ({} {source})",
                        dest.display(),
                        if fresh { "cloned from" } else { "already cloned from" }
                    );
                    engine.rescan_repos(true);
                    dest
                }
            };
            // 2. Where the agent runs: a branch or pearl worktree, else the checkout.
            let wants_worktree = branch.is_some() || pearl_id.is_some();
            if wants_worktree && git(&checkout, &["rev-parse", "--git-dir"]).is_err() {
                bail!(
                    "{} is not a git repository, so there is no worktree to make for a pearl or branch",
                    checkout.display()
                );
            }
            let main = if wants_worktree {
                smooth_flow::engine::project_root(&checkout)
            } else {
                checkout.clone()
            };
            let mut req = NewRequest {
                kind,
                pearl_id: pearl_id.clone(),
                prompt,
                title,
                model,
                ..NewRequest::default()
            };
            if let Some(b) = branch.as_deref() {
                let wt = Engine::create_branch_worktree(&main, b, "HEAD")?;
                req.worktree = Some(wt.to_string_lossy().into_owned());
            } else if pearl_id.is_some() {
                // The engine's own pearl worktree: ../<repo>-<pearl>-<slug>.
                req.project = Some(main.to_string_lossy().into_owned());
            } else {
                req.worktree = Some(checkout.to_string_lossy().into_owned());
            }
            // 3. Start the harness there.
            let s = engine.new_session(req)?;
            let _ = writeln!(
                out,
                "Worktree: {}{}",
                s.worktree,
                s.branch.as_deref().map(|b| format!(" (branch {b})")).unwrap_or_default()
            );
            let _ = writeln!(out, "Started {}\n{}", s.id, session_line(&row(&s)));
            if has_prompt {
                out.push_str("It already has the prompt — watch it with flow_list; don't prompt it again.\n");
            }
            Ok(out)
        })
        .await
        .map_err(|e| anyhow!("project_setup panicked: {e}"))??;
        Ok(report)
    }
}

// ── project_setup plumbing ────────────────────────────────────────────────────

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        // Never block a tool call on a credential prompt nobody can see.
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .with_context(|| format!("git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn is_bare_repo(path: &Path) -> bool {
    git(path, &["rev-parse", "--is-bare-repository"]).is_ok_and(|s| s == "true")
}

/// A URL `git clone` may fetch. Only the ordinary transports: `ext::` and
/// friends (which run a command) are not URLs here, so they fall through to
/// "no such directory".
fn is_git_url(s: &str) -> bool {
    const SCHEMES: &[&str] = &["https://", "http://", "ssh://", "git://", "file://"];
    if SCHEMES.iter().any(|p| s.starts_with(p)) {
        return true;
    }
    // scp-like `user@host:owner/repo`: a colon before any slash, after an `@`.
    match (s.find('@'), s.find(':')) {
        (Some(at), Some(colon)) => at > 0 && at < colon && s.find('/').is_none_or(|slash| slash > colon) && !s.contains(char::is_whitespace),
        _ => false,
    }
}

/// The directory name a clone of `url` gets: its last path segment, minus
/// `.git`. Refuses anything that could escape the clone root or read as an
/// option.
fn repo_name(url: &str) -> Result<String> {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or("");
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.starts_with('-')
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        bail!("can't name a checkout for {url:?}");
    }
    Ok(name.to_string())
}

/// Clone `url` to `<root>/<name>`, or reuse a checkout already there whose
/// `origin` is `url`. Returns the checkout and whether it was cloned now.
fn clone(url: &str, root: &Path) -> Result<(PathBuf, bool)> {
    let dest = root.join(repo_name(url)?);
    if dest.exists() {
        let origin = git(&dest, &["remote", "get-url", "origin"]).unwrap_or_default();
        if same_remote(&origin, url) {
            return Ok((dest, false));
        }
        bail!(
            "{} already exists and is not a clone of {url} (origin: {}) — pass clone_into to put it elsewhere",
            dest.display(),
            if origin.is_empty() { "none" } else { &origin }
        );
    }
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let dest_s = dest.to_string_lossy().into_owned();
    git(root, &["clone", "--quiet", "--", url, &dest_s])?;
    Ok((dest, true))
}

fn same_remote(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('/').trim_end_matches(".git").to_string();
    !a.trim().is_empty() && norm(a) == norm(b)
}

fn valid_pearl_id(p: &str) -> bool {
    !p.is_empty() && p.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests;
