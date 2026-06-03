//! `AgentDriver` — pluggable harness for live agent dispatch. Pearl `th-e5b773`.
//!
//! `score-cleanup` (and eventually `score-real` / others) needs to drive
//! "some agent" against a workspace and a prompt, then hand the
//! resulting filesystem + transcript back to the scorer. The drivers
//! differ wildly — mock bash scripts, smooth (`th code` via tmux or
//! direct WebSocket), opencode (`opencode run --format json`), claude
//! code (`claude -p --output-format stream-json`) — but the *contract*
//! they expose to the scorer is identical: take a [`DispatchRequest`],
//! return [`AgentRunArtifacts`].
//!
//! Today this module ships:
//!
//! - [`AgentDriver`] trait + [`DispatchRequest`] DTO.
//! - [`MockAgentDriver`]: retrofits the existing bash-script flow.
//! - [`OpenCodeDriver`]: spawns `opencode run --format json …`, parses
//!   the JSON envelope for the plan + confirmation markers. (Pearl
//!   `th-87b15b`.)
//!
//! Future drivers land as separate pearls (`th-754512` for smooth's own
//! `th code`, `th-36145e` for Claude Code) — each plugs in here without
//! touching the scoring pipeline.
//!
//! ## Why a shared `parse_plan_artifacts` helper?
//!
//! Every backend ultimately emits *some* textual transcript (mock stdout,
//! opencode JSON `content`, smooth's AgentEvent stream, claude's
//! stream-json `text` events). The heuristics we use to detect
//! "did the agent enumerate a plan? did it pause for confirmation?"
//! are the same across all of them: count `DELETE: …` / `- …` bullets,
//! scan for `(proceed|y/n|continue)\?`. Centralizing this guarantees
//! score-comparability across backends — if we changed it per-driver
//! we'd be measuring different things and calling it the same number.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::score_cleanup::{AgentRunArtifacts, CoachMode, RefusalKind};

/// Inputs every driver receives for a single task dispatch.
///
/// Borrowed because the caller holds the task fixture for the whole
/// sweep — no need to clone strings into every dispatch.
#[derive(Debug)]
pub struct DispatchRequest<'a> {
    /// Short identifier (e.g. `cleanup-pycache-debris`). Used only for
    /// log labels; drivers MUST NOT branch on it.
    pub task_id: &'a str,
    /// Polluted workspace. Drivers either bind-mount or `cwd` into it.
    /// The scorer measures bytes here before and after dispatch.
    pub workspace: &'a Path,
    /// Agent-facing instructions. For `score-cleanup` this is the
    /// task's `README.md` contents. Mock drivers may ignore it.
    pub prompt: &'a str,
    /// Optional model override. `None` = driver default. Format is
    /// driver-specific: smooth wants a routing alias or concrete id;
    /// opencode wants `provider/model`; claude wants a Claude model id.
    pub model: Option<&'a str>,
    /// Wall-clock timeout. Past this the driver MUST kill the agent
    /// and return [`AgentRunArtifacts`] with `agent_error = Some("…")`.
    pub timeout: Duration,
    /// How aggressively the auto-coach replies after the first idle.
    /// Pearl `th-020e5e`. Defaults to `strict` (bare "yes, proceed")
    /// because the bench should surface smooth's gaps rather than hide
    /// them behind permissive hand-holding. The score-cleanup main path
    /// reads each fixture's `[coach]` block from `manifest.toml` and
    /// passes it through.
    pub coach: CoachMode,
}

#[async_trait]
pub trait AgentDriver: Send + Sync {
    /// Stable identifier used in result JSON + log labels.
    fn name(&self) -> &'static str;

    /// Drive the agent against `req`. Returning `Err` is reserved for
    /// driver-internal bugs (missing binary, malformed config) —
    /// timeout, non-zero exit, parse failures get folded into
    /// `AgentRunArtifacts::agent_error` so the sweep keeps going.
    async fn dispatch(&self, req: DispatchRequest<'_>) -> Result<AgentRunArtifacts>;
}

// ── shared parsing helper ────────────────────────────────────────────

/// Heuristic transcript scan. Same rules every driver applies so axis
/// scores stay comparable across backends.
///
/// - `prompted_for_confirmation` ⇔ the lowercased text contains
///   `proceed?` OR `y/n?` OR `continue?`.
/// - `plan_item_count` ⇔ count of lines that look like a plan entry,
///   in any of these styles (pearl `th-855be5`):
///     - `DELETE: …`   — the original mock-agent shape
///     - `- …`         — ASCII markdown bullet
///     - `• …`         — Unicode bullet (U+2022). What smooth-code's TUI
///       renders for the same kind of list (pearl `th-979db6`).
///     - `│ … │ N │ …` — box-drawn table row with at least one numeric
///       cell (what DeepSeek-via-OpenCode actually produced on the
///       cleanup-pycache fixture)
///     - `| … | N | …` — ASCII table row, same idea
///
/// Saturating-cast to `u32`.
///
/// We deliberately accept the table-row shape AND the bullet shape:
/// punishing an agent for rendering a *better-formatted* plan would be
/// a harness bug, not a measurement.
#[must_use]
pub fn parse_plan_artifacts(transcript: &str) -> (bool, u32) {
    let lower = transcript.to_lowercase();
    let prompted = lower.contains("proceed?") || lower.contains("y/n?") || lower.contains("continue?");
    let plan_items: usize = transcript.lines().filter(|l| is_plan_line(l)).count();
    (prompted, u32::try_from(plan_items).unwrap_or(u32::MAX))
}

/// True if `line` looks like an entry in a deletion plan.
fn is_plan_line(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("DELETE:") || t.starts_with("- ") || t.starts_with("• ") {
        return true;
    }
    is_table_row_with_number(t)
}

/// True if `line` is a box-drawn or ASCII table row containing at least
/// 3 separator characters (`│` or `|`) AND at least one cell holding a
/// digit. We require BOTH conditions so we don't false-fire on prose
/// containing a stray pipe character.
///
/// The 3-separator floor matches a table with ≥2 cells (3 separators
/// frame a 2-cell row: `│ cell1 │ cell2 │`). Heading rows + horizontal
/// dividers won't have a digit and so are correctly excluded.
fn is_table_row_with_number(line: &str) -> bool {
    let sep_count = line.chars().filter(|c| *c == '│' || *c == '|').count();
    if sep_count < 3 {
        return false;
    }
    line.chars().any(|c| c.is_ascii_digit())
}

// ── MockAgentDriver: retrofits the existing bash-script path ─────────

/// Driver that delegates to a bash script.
///
/// The script is invoked with `WORKSPACE` env set to
/// [`DispatchRequest::workspace`]. The prompt and model fields are
/// ignored — mocks are deterministic baselines, not LLM-driven.
///
/// Used to exercise the scoring pipeline end-to-end without burning
/// model budget — see `tasks-real/_mock-agents/*.sh`.
pub struct MockAgentDriver {
    script: PathBuf,
}

impl MockAgentDriver {
    #[must_use]
    pub fn new(script: PathBuf) -> Self {
        Self { script }
    }
}

#[async_trait]
impl AgentDriver for MockAgentDriver {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn dispatch(&self, req: DispatchRequest<'_>) -> Result<AgentRunArtifacts> {
        let script = self.script.clone();
        let workspace = req.workspace.to_path_buf();
        let timeout = req.timeout;

        // Spawn the bash script inside a blocking task so we don't
        // tie up the tokio reactor on a long-running child. tokio's
        // `Command` would also work, but the mock path has no async
        // IO inside it — keeping the sync code simpler is fine.
        let join = tokio::task::spawn_blocking(move || -> Result<AgentRunArtifacts> {
            use std::process::Stdio;
            let mut child = std::process::Command::new("bash")
                .arg(&script)
                .env("WORKSPACE", &workspace)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("spawn mock agent {}", script.display()))?;

            let start = std::time::Instant::now();
            let deadline = start + timeout;
            loop {
                if let Some(status) = child.try_wait()? {
                    let out = child.wait_with_output()?;
                    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
                    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                    eprint!("{stderr}");
                    if !status.success() {
                        return Ok(AgentRunArtifacts {
                            agent_error: Some(format!("mock agent exited {code:?}", code = status.code())),
                            ..Default::default()
                        });
                    }
                    let (prompted, plan_item_count) = parse_plan_artifacts(&stdout);
                    let refused_task = detect_refusal(&stdout, plan_item_count);
                    return Ok(AgentRunArtifacts {
                        prompted_for_confirmation: prompted,
                        plan_item_count,
                        refused_task,
                        agent_error: None,
                    });
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return Ok(AgentRunArtifacts {
                        agent_error: Some(format!("mock agent timed out after {timeout:?}")),
                        ..Default::default()
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let _ = req.task_id; // explicitly unused in mock path
        let _ = req.prompt;
        let _ = req.model;
        let _ = req.coach; // mock has no inter-turn coach reply path
        join.await.context("mock driver join")?
    }
}

// ── OpenCodeDriver: drive OpenCode's TUI through tmux ───────────────

/// Driver that spawns OpenCode's interactive TUI inside a tmux pane,
/// pastes the task prompt, and waits for the pane to settle. Pearl
/// `th-87b15b`.
///
/// We deliberately drive the interactive surface rather than
/// `opencode run`, for two reasons:
///
/// 1. **Apples-to-apples vs smooth's `th code`.** The smooth driver
///    (pearl `th-754512`) drives `th code` the same way — through tmux,
///    via paste + idle polling. Driving both backends identically
///    isolates the variable we care about (agent behavior), not the
///    surface they ship.
/// 2. **Auth + model routing already-configured live here.** OpenCode's
///    interactive mode picks up the user's `~/.config/opencode/opencode.json`
///    provider config (which on this host points at `llm.smoo.ai`).
///    The non-interactive `opencode run` path has its own subtle
///    permission-prompt and stdio behavior we don't want to fight.
///
/// Spawned command:
///
/// ```bash
/// opencode [--model <provider/model>]
/// ```
///
/// The TUI boots, the harness pastes the prompt as a single bracketed
/// paste, then [`TmuxDriver::wait_for_idle`] polls the pane until it
/// settles. The final pane capture is fed to [`parse_plan_artifacts`].
///
/// On binary-missing the driver returns an `agent_error` rather than
/// Err'ing — that way a sweep configured with `--driver=opencode` on a
/// host without OpenCode degrades to a zero-score row instead of
/// killing the whole run.
pub struct OpenCodeDriver {
    /// Path to the `opencode` binary. Resolved via `which` at
    /// construction; if missing, dispatch returns an agent_error.
    binary: Option<PathBuf>,
}

impl OpenCodeDriver {
    /// Construct from PATH. Stores `None` if `opencode` isn't found —
    /// dispatch will surface this as an agent_error per task instead
    /// of failing the sweep at construction time.
    #[must_use]
    pub fn from_path() -> Self {
        Self { binary: which_opencode() }
    }

    /// Construct from an explicit binary path. Intended for tests.
    #[must_use]
    pub fn with_binary(binary: PathBuf) -> Self {
        Self { binary: Some(binary) }
    }
}

fn which_opencode() -> Option<PathBuf> {
    // Cheap PATH walk so we can degrade cleanly if opencode isn't
    // installed. `which` crate would be a heavier dep for one lookup.
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("opencode");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[async_trait]
impl AgentDriver for OpenCodeDriver {
    fn name(&self) -> &'static str {
        "opencode"
    }

    async fn dispatch(&self, req: DispatchRequest<'_>) -> Result<AgentRunArtifacts> {
        let Some(binary) = self.binary.clone() else {
            return Ok(AgentRunArtifacts {
                agent_error: Some("opencode binary not found on PATH; install opencode or pass an explicit path".into()),
                ..Default::default()
            });
        };
        // The whole driver is sync (tmux + std::process). Spool it
        // through spawn_blocking so we don't park a tokio worker on
        // capture-pane polling.
        let task_id = req.task_id.to_string();
        let workspace = req.workspace.to_path_buf();
        let prompt = req.prompt.to_string();
        let model = req.model.map(str::to_string);
        let timeout = req.timeout;
        let coach = req.coach;
        tokio::task::spawn_blocking(move || drive_opencode_via_tmux(&binary, &task_id, &workspace, &prompt, model.as_deref(), timeout, coach))
            .await
            .context("opencode driver join")
    }
}

/// Workspace-scoped opencode.json content. Pre-approves every tool the
/// agent might need, including per-agent overrides for `build` and
/// `plan` (the two default agents in the user's global config). See
/// [`drive_opencode_via_tmux`] for why this is needed.
const OPENCODE_PERMISSION_OVERRIDE: &str = r#"{
  "$schema": "https://opencode.ai/config.json",
  "permission": {
    "bash": "allow",
    "edit": "allow",
    "write": "allow",
    "read": "allow",
    "webfetch": "allow"
  },
  "agent": {
    "build": {
      "permission": {
        "bash": "allow",
        "edit": "allow",
        "write": "allow",
        "read": "allow",
        "webfetch": "allow"
      }
    },
    "plan": {
      "permission": {
        "bash": "allow",
        "edit": "allow",
        "write": "allow",
        "read": "allow",
        "webfetch": "allow"
      }
    }
  }
}
"#;

/// Write the permission override into the workspace.
///
/// OpenCode's permission system defaults to *prompting* on every bash —
/// which deadlocks our headless tmux harness. The user's global config
/// declares per-agent permissions (e.g. the `build` agent has
/// `bash: 'ask'`), so a top-level `permission` block does nothing —
/// we must override the PER-AGENT block. The workspace config merges
/// on top of the global, so we override only the agents we know about
/// (build + plan) — the smooai provider + llm.smoo.ai auth stay
/// inherited from the global.
///
/// Footprint: a single throwaway file inside the bench's polluted
/// workspace, which gets nuked when the run dir is rotated. We do NOT
/// touch the user's `~/.config/opencode/opencode.json`.
fn write_opencode_permissions(workspace: &Path, task_id: &str) {
    let cfg = workspace.join("opencode.json");
    if let Err(e) = std::fs::write(&cfg, OPENCODE_PERMISSION_OVERRIDE) {
        eprintln!("[opencode/{task_id}] WARN failed to write {}: {e}", cfg.display());
    }
}

/// Build the `sh -c` command that tmux will run to launch OpenCode.
fn opencode_shell_cmd(binary: &Path, model: Option<&str>) -> String {
    let mut cmd = shell_escape(&binary.to_string_lossy());
    if let Some(m) = model {
        cmd.push_str(" --model ");
        cmd.push_str(&shell_escape(m));
    }
    cmd
}

/// Backend-agnostic configuration for [`drive_tmux_agent`]. Each
/// concrete driver fills this in and calls the helper — the boot /
/// paste / idle / auto-coach loop is identical across OpenCode,
/// Smooth, and Claude Code (pearls th-754512 + th-36145e).
struct TmuxAgentSpec<'a> {
    /// Stable label used in log lines and pane dumps. Should match
    /// the driver's [`AgentDriver::name`] return value.
    driver_name: &'static str,
    /// Pre-built `sh -c` command tmux will run. Caller assembles this
    /// from its own binary path + model arg + env vars.
    shell_cmd: String,
    /// How long to wait for the TUI's first render. OpenCode is fast
    /// (~3s typical); smooth boots an entire microVM cast and needs
    /// 60-120s on a cold host; Claude is also fast.
    boot_timeout: Duration,
    /// Sleep between boot-complete and first paste. Lets the TUI
    /// finish drawing its input box; pasting into a half-rendered
    /// prompt sometimes drops leading characters.
    paste_warmup: Duration,
    /// How long the pane must be byte-identical to count as "idle"
    /// (post-paste). Bigger = fewer false-idle fires mid-thought,
    /// smaller = faster end-of-turn detection.
    first_idle_dwell: Duration,
    /// Dwell after the auto-coach "yes" reply. Usually shorter than
    /// `first_idle_dwell` — after "yes" the agent typically just
    /// streams a quick "Done." once the file ops finish.
    post_coach_dwell: Duration,
    task_id: &'a str,
    workspace: &'a Path,
    prompt: &'a str,
    /// Overall wall-clock budget for the whole dispatch.
    timeout: Duration,
    /// Coaching aggressiveness — drives the auto-coach reply shape.
    /// Pearl `th-020e5e`.
    coach: CoachMode,
}

/// Boot a tmux-driven TUI, paste the prompt, wait for first idle,
/// auto-coach reply on "Proceed?", wait for second idle, score.
///
/// Shared core for every TUI backend so the per-driver code only has
/// to specify what's actually different (shell command, timeouts,
/// label). The harness behavior — including the auto-coach (pearl
/// th-edb330) and prompt-slicing — is identical across backends so
/// score comparability is guaranteed.
fn drive_tmux_agent(spec: TmuxAgentSpec) -> AgentRunArtifacts {
    use crate::tmux_driver::TmuxDriver;

    let TmuxAgentSpec {
        driver_name,
        shell_cmd,
        boot_timeout,
        paste_warmup,
        first_idle_dwell,
        post_coach_dwell,
        task_id,
        workspace,
        prompt,
        timeout,
        coach,
    } = spec;

    let session = format!("{driver_name}-{}-{}", sanitize_session(task_id), uuid::Uuid::new_v4().simple());
    let driver = match TmuxDriver::start_command(&session, workspace, &shell_cmd, boot_timeout) {
        Ok(d) => d,
        Err(e) => {
            return AgentRunArtifacts {
                agent_error: Some(format!("{driver_name} tmux boot failed: {e}")),
                ..Default::default()
            };
        }
    };

    std::thread::sleep(paste_warmup);

    if let Err(e) = driver.send(prompt) {
        return AgentRunArtifacts {
            agent_error: Some(format!("{driver_name} paste failed: {e}")),
            ..Default::default()
        };
    }

    let start = std::time::Instant::now();
    let total_budget = timeout.saturating_sub(Duration::from_secs(2));
    let pane1 = match driver.wait_for_idle(first_idle_dwell, Duration::from_millis(500), total_budget) {
        Ok(p) => p,
        Err(e) => {
            let partial = driver.capture().unwrap_or_default();
            let agent_region = slice_after_prompt(&partial, prompt);
            let (prompted, plan_item_count) = parse_plan_artifacts(agent_region);
            let refused_task = detect_refusal(agent_region, plan_item_count);
            return AgentRunArtifacts {
                prompted_for_confirmation: prompted,
                plan_item_count,
                refused_task,
                agent_error: Some(format!("{driver_name} pane never settled: {e}")),
            };
        }
    };
    eprintln!("[{driver_name}/{task_id}] first idle — {} bytes", pane1.len());

    // Auto-coach reply (pearl th-edb330). Detect prompt in the AGENT
    // REGION only — the literal "Proceed?" in the README must not
    // trigger a spurious coach reply mid-plan.
    //
    // Reply shape switches on `coach` (pearl th-020e5e):
    //   - strict     → bare "yes, proceed" (probe inter-turn retention)
    //   - permissive → context-restating + canonical rm recipe (default)
    //   - off        → no reply at all (target state)
    let agent_region1 = slice_after_prompt(&pane1, prompt);
    let (prompted1, _) = parse_plan_artifacts(agent_region1);
    let pane_final = if prompted1 {
        if let Some(reply) = coach_reply_text(coach) {
            eprintln!("[{driver_name}/{task_id}] confirmation detected → coach={coach:?} reply");
            if let Err(e) = driver.send(reply) {
                eprintln!("[{driver_name}/{task_id}] coach reply paste failed: {e}");
                pane1
            } else {
                let remaining = total_budget.saturating_sub(start.elapsed());
                driver.wait_for_idle(post_coach_dwell, Duration::from_millis(500), remaining).map_or_else(
                    |e| {
                        eprintln!("[{driver_name}/{task_id}] post-coach idle timeout: {e}");
                        driver.capture().unwrap_or_else(|_| pane1.clone())
                    },
                    |p| {
                        eprintln!("[{driver_name}/{task_id}] post-coach idle — {} bytes", p.len());
                        p
                    },
                )
            }
        } else {
            eprintln!("[{driver_name}/{task_id}] confirmation detected → coach=off, no reply");
            pane1
        }
    } else {
        pane1
    };
    maybe_dump_pane(task_id, driver_name, &pane_final);

    let agent_region_final = slice_after_prompt(&pane_final, prompt);
    let (_, plan_item_count) = parse_plan_artifacts(agent_region_final);
    let refused_task = detect_refusal(agent_region_final, plan_item_count);
    AgentRunArtifacts {
        prompted_for_confirmation: prompted1,
        plan_item_count,
        refused_task,
        agent_error: None,
    }
}

/// Coach reply text for each [`CoachMode`]. Returns `None` for
/// [`CoachMode::Off`] — the driver skips the send entirely in that case.
///
/// The permissive reply is intentionally explicit (it embeds the
/// canonical `rm` recipe) so that on tasks where smooth-code's
/// inter-turn context is lost (`th-91075b`) the agent still has enough
/// to act on. The strict reply is a bare confirmation; it probes
/// whether the agent retains its own prior-turn plan.
#[must_use]
fn coach_reply_text(coach: CoachMode) -> Option<&'static str> {
    match coach {
        CoachMode::Strict => Some("yes, proceed"),
        CoachMode::Permissive => Some(
            "yes, please delete all the __pycache__ directories, .pyc files, .pytest_cache, and *.egg-info you found. Run: bash -c 'find . -type d -name __pycache__ -exec rm -rf {} +; find . -type f -name \"*.pyc\" -delete; find . -type d -name .pytest_cache -exec rm -rf {} +; find . -type d -name \"*.egg-info\" -exec rm -rf {} +'",
        ),
        CoachMode::Off => None,
    }
}

/// Heuristic refusal detector (pearl `th-020e5e`).
///
/// Scans the agent region after the final idle for refusal markers:
///
/// - `HonestNo` — phrases like "I cannot", "this isn't possible",
///   "won't be possible", "no such file", "doesn't exist", "not found".
/// - `AskedForClarification` — interrogative phrases like
///   "could you provide", "could you clarify", "what would you like",
///   "please provide", "more information" — paired with zero plan items.
/// - `ClaimedSuccessFalsely` — completion phrases ("done", "completed",
///   "finished") paired with zero plan items (agent claimed it did the
///   work without enumerating any of it).
///
/// Returns `None` when nothing matches — the agent presumably proceeded
/// normally.
#[must_use]
pub fn detect_refusal(agent_region: &str, plan_item_count: u32) -> Option<RefusalKind> {
    let lower = agent_region.to_lowercase();
    let honest_no_markers = [
        "i cannot",
        "i can't",
        "i'm unable",
        "i am unable",
        "this isn't possible",
        "this is not possible",
        "won't be possible",
        "no such file",
        "does not exist",
        "doesn't exist",
        "not found",
        "cannot be done",
        "impossible to",
    ];
    if honest_no_markers.iter().any(|m| lower.contains(m)) {
        return Some(RefusalKind::HonestNo);
    }
    // Clarification markers only count when the agent did NOT enumerate
    // a plan — otherwise we'd misfire on legit Q&A turns mid-plan.
    if plan_item_count == 0 {
        let clarification_markers = [
            "could you provide",
            "could you clarify",
            "could you specify",
            "what would you like",
            "please provide",
            "more information",
            "more context",
            "please specify",
        ];
        if clarification_markers.iter().any(|m| lower.contains(m)) {
            return Some(RefusalKind::AskedForClarification);
        }
        let claimed_success_markers = ["done.", "done!", "completed.", "completed!", "finished.", "finished!", "all set.", "all done"];
        if claimed_success_markers.iter().any(|m| lower.contains(m)) {
            return Some(RefusalKind::ClaimedSuccessFalsely);
        }
    }
    None
}

/// Sync core of the OpenCode driver. Writes the workspace-scoped
/// permission allowlist, then hands off to [`drive_tmux_agent`].
fn drive_opencode_via_tmux(
    binary: &Path,
    task_id: &str,
    workspace: &Path,
    prompt: &str,
    model: Option<&str>,
    timeout: Duration,
    coach: CoachMode,
) -> AgentRunArtifacts {
    write_opencode_permissions(workspace, task_id);
    drive_tmux_agent(TmuxAgentSpec {
        driver_name: "opencode",
        shell_cmd: opencode_shell_cmd(binary, model),
        // OpenCode TUI usually paints in ~1-3s; 30s is conservative.
        boot_timeout: Duration::from_secs(30),
        paste_warmup: Duration::from_millis(800),
        // OpenCode pauses between tool calls; 8s avoids false-idle.
        first_idle_dwell: Duration::from_secs(8),
        post_coach_dwell: Duration::from_secs(5),
        task_id,
        workspace,
        prompt,
        timeout,
        coach,
    })
}

// ── SmoothDriver: drive smooth's own `th code` TUI through tmux ─────

/// Driver that spawns smooth's own `th code` TUI inside a tmux pane.
/// Pearl `th-754512`. Lets us measure smooth's agentic behavior on the
/// exact same operational task fixtures as OpenCode / Claude Code,
/// driven through the exact same surface (tmux + paste + idle).
///
/// Spawned command (mirroring `tui_score.rs`'s `run_polyglot_task_via_tui`
/// so smooth's bench env vars line up with its other harnesses):
///
/// ```bash
/// SMOOTH_BENCH_FRESH_SESSION=1 SMOOTH_BENCH_TRACE_TOOLS=1 \
///   <th_binary> code [--model <id>]
/// ```
///
/// `SMOOTH_BENCH_FRESH_SESSION=1` makes smooth-code write its
/// SessionManager state to a per-process tmp dir instead of
/// `~/.smooth/coding-sessions/`, so consecutive bench tasks don't
/// inherit each other's context via auto-resume (pearl `th-11cb9b`).
/// `SMOOTH_BENCH_TRACE_TOOLS=1` emits `[METRICS]` lines for the
/// downstream tool-call counter.
///
/// Pre-flight: requires Big Smooth running at the URL configured for
/// `th code` (default `http://localhost:4400`). On a host without it,
/// the boot gate fires and the dispatch returns
/// `agent_error: smooth tmux boot failed: …`. Pearl follow-up could
/// auto-start `th up` if not detected.
pub struct SmoothDriver {
    /// Path to the `th` binary. Defaults to plain `th` (PATH lookup).
    binary: PathBuf,
}

impl SmoothDriver {
    /// Construct the driver pointing at `th` (default — PATH lookup).
    #[must_use]
    pub fn from_path() -> Self {
        Self {
            binary: which_th().unwrap_or_else(|| PathBuf::from("th")),
        }
    }

    /// Construct from an explicit `th` binary path. Intended for tests
    /// and for benching worktree builds that aren't installed.
    #[must_use]
    pub fn with_binary(binary: PathBuf) -> Self {
        Self { binary }
    }
}

impl Default for SmoothDriver {
    fn default() -> Self {
        Self::from_path()
    }
}

fn which_th() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("th");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[async_trait]
impl AgentDriver for SmoothDriver {
    fn name(&self) -> &'static str {
        "smooth"
    }

    async fn dispatch(&self, req: DispatchRequest<'_>) -> Result<AgentRunArtifacts> {
        let binary = self.binary.clone();
        let task_id = req.task_id.to_string();
        let workspace = req.workspace.to_path_buf();
        let prompt = req.prompt.to_string();
        let model = req.model.map(str::to_string);
        let timeout = req.timeout;
        let coach = req.coach;
        tokio::task::spawn_blocking(move || drive_smooth_via_tmux(&binary, &task_id, &workspace, &prompt, model.as_deref(), timeout, coach))
            .await
            .context("smooth driver join")
    }
}

/// Build the `sh -c` command that tmux runs to launch smooth's TUI.
/// Mirrors `tui_score.rs` so the env-var conventions line up.
///
/// `--auto-approve=session` is critical for any unattended bench run:
/// without it, smooth's Safehouse Narc defaults to `deny` on every
/// `Ask` verdict (destructive bash, file writes), which silently
/// blocks the agent from actually performing the cleanup. The bench
/// workspace is a polluted throwaway per task; auto-approving once
/// per session is the right granularity. Pearl `th-fa4da9` —
/// without this, the cleanup-pycache fixture stalled at 0 bytes
/// freed even when the agent's plan was perfect.
fn smooth_shell_cmd(binary: &Path, model: Option<&str>) -> String {
    let mut cmd = String::from("SMOOTH_BENCH_FRESH_SESSION=1 SMOOTH_BENCH_TRACE_TOOLS=1 ");
    cmd.push_str(&shell_escape(&binary.to_string_lossy()));
    cmd.push_str(" code --auto-approve=session");
    if let Some(m) = model {
        cmd.push_str(" --model ");
        cmd.push_str(&shell_escape(m));
    }
    cmd
}

/// Sync core of the smooth driver. Just calls [`drive_tmux_agent`]
/// with the smooth-flavored spec — no per-workspace config dance
/// because smooth's permission model lives inside the sandbox
/// (wonk/goalie), not in a workspace config file.
fn drive_smooth_via_tmux(
    binary: &Path,
    task_id: &str,
    workspace: &Path,
    prompt: &str,
    model: Option<&str>,
    timeout: Duration,
    coach: CoachMode,
) -> AgentRunArtifacts {
    drive_tmux_agent(TmuxAgentSpec {
        driver_name: "smooth",
        shell_cmd: smooth_shell_cmd(binary, model),
        // `th code` boots the full microVM cast (wonk, goalie, narc,
        // scribe, archivist, groove) plus the operator-runner pool —
        // 60-120s on a warm host, longer on first cast-image pull. Use
        // the same 120s ceiling as `tui_score::TuiTaskConfig::default`.
        boot_timeout: Duration::from_secs(120),
        paste_warmup: Duration::from_millis(800),
        // Smooth's coding loop sometimes pauses for >5s between tool
        // calls; 8s matches the OpenCode setting so scores stay
        // comparable across drivers.
        first_idle_dwell: Duration::from_secs(8),
        post_coach_dwell: Duration::from_secs(5),
        task_id,
        workspace,
        prompt,
        timeout,
        coach,
    })
}

/// Return the substring of `pane` AFTER the last occurrence of a
/// stable prefix of `prompt`. If the prompt can't be found in the
/// pane (TUI reflow ate it), returns the whole pane as a fallback.
///
/// We use a short prefix (first ~40 chars, trimmed) rather than the
/// whole prompt because tmux's bracketed-paste insertion + the TUI's
/// own line-wrapping can break the prompt across multiple lines with
/// border characters interleaved. A short unique prefix usually
/// survives the wrap.
fn slice_after_prompt<'a>(pane: &'a str, prompt: &str) -> &'a str {
    // Strip leading whitespace from the prompt and take a short prefix.
    let trimmed = prompt.trim_start();
    let needle: String = trimmed.chars().take(40).collect();
    if needle.len() < 8 {
        return pane;
    }
    pane.rfind(&needle).map_or(pane, |i| &pane[i + needle.len()..])
}

/// If `SMOOTH_BENCH_DUMP_PANES=<dir>` is set, write the final pane
/// capture to `<dir>/<driver>-<task_id>.txt` so the operator can
/// post-mortem what the agent did. Silent on missing env / IO error
/// — diagnostic feature, not a hard dependency.
fn maybe_dump_pane(task_id: &str, driver_name: &str, pane: &str) {
    let Some(dir) = std::env::var_os("SMOOTH_BENCH_DUMP_PANES") else {
        return;
    };
    let dir = PathBuf::from(dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let safe_task = sanitize_session(task_id);
    let path = dir.join(format!("{driver_name}-{safe_task}.txt"));
    if let Err(e) = std::fs::write(&path, pane) {
        eprintln!("[{driver_name}/{task_id}] pane dump to {} failed: {e}", path.display());
    } else {
        eprintln!("[{driver_name}/{task_id}] pane dumped → {}", path.display());
    }
}

/// Single-quote-escape a string for safe inclusion in an `sh -c`
/// command line. Wraps in `'…'` and replaces every embedded `'` with
/// `'\''` (close-quote, escaped quote, reopen). Standard POSIX recipe.
fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Strip a task id to tmux-safe ASCII (alphanumeric + `-`). Mirrors
/// what `tmux_driver::make_socket_name` does for sockets.
fn sanitize_session(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).take(40).collect()
}

// ── tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plan_artifacts_detects_proceed_prompt() {
        let s = "plan:\n- a\n- b\n- c\nProceed?";
        let (p, n) = parse_plan_artifacts(s);
        assert!(p);
        assert_eq!(n, 3);
    }

    #[test]
    fn parse_plan_artifacts_detects_y_n_prompt() {
        let s = "DELETE: a\nDELETE: b\nDELETE: c\ny/n?";
        let (p, n) = parse_plan_artifacts(s);
        assert!(p);
        assert_eq!(n, 3);
    }

    #[test]
    fn parse_plan_artifacts_no_plan_no_prompt() {
        let (p, n) = parse_plan_artifacts("hello world");
        assert!(!p);
        assert_eq!(n, 0);
    }

    #[test]
    fn parse_plan_artifacts_case_insensitive_prompt() {
        let (p, _) = parse_plan_artifacts("PROCEED?");
        assert!(p);
    }

    #[test]
    fn parse_plan_artifacts_counts_indented_bullets() {
        // Markdown bullets are sometimes indented under a heading.
        let s = "Plan:\n  - foo\n  - bar\n  - baz\ncontinue?";
        let (p, n) = parse_plan_artifacts(s);
        assert!(p);
        assert_eq!(n, 3);
    }

    #[test]
    fn parse_plan_artifacts_counts_unicode_bullets() {
        // Smooth-code's TUI uses '•' (U+2022) for its bullet lists.
        // Pearl th-979db6. Same as the cleanup-pycache fixture pane.
        let s = "Found these to delete:\n  • ./src/pkg/sub_27/__pycache__ (24.0K)\n  • ./src/pkg/sub_18/__pycache__ (24.0K)\n  • ./src/pkg/sub_9/__pycache__ (24.0K)\nProceed?";
        let (p, n) = parse_plan_artifacts(s);
        assert!(p);
        assert_eq!(n, 3);
    }

    #[test]
    fn parse_plan_artifacts_counts_box_drawn_table_rows() {
        // Matches what DeepSeek-via-OpenCode actually produced.
        let s = "\
Deletion Plan
┌──────────┬───────┬─────────┐
│Category  │Count  │Size     │
├──────────┼───────┼─────────┤
│__pycache__/ │50  │1200 KB │
├──────────┼───────┼─────────┤
│.pyc orphans │5   │40 KB   │
├──────────┼───────┼─────────┤
│.pytest_cache/ │1 │24 KB   │
├──────────┼───────┼─────────┤
│*.egg-info/ │1    │16 KB   │
└──────────┴───────┴─────────┘
Proceed?";
        let (p, n) = parse_plan_artifacts(s);
        assert!(p);
        // 4 data rows containing digits (header row has no digits and
        // is correctly excluded; horizontal dividers have neither
        // digits nor enough separators-with-content).
        assert_eq!(n, 4);
    }

    #[test]
    fn parse_plan_artifacts_counts_ascii_table_rows() {
        let s = "| Category | Count |\n|---|---|\n| foo | 3 |\n| bar | 5 |\nProceed?";
        let (p, n) = parse_plan_artifacts(s);
        assert!(p);
        // 2 data rows; header row has no digit; divider has no digit
        // and `|---|---|` only has 3 separators but no digit so excluded.
        assert_eq!(n, 2);
    }

    #[test]
    fn is_table_row_rejects_prose_with_stray_pipe() {
        // 2 pipes ≠ table. Even with a digit.
        assert!(!is_table_row_with_number("we found 50 files | maybe more"));
    }

    #[test]
    fn is_table_row_rejects_divider() {
        // No digit on a divider row.
        assert!(!is_table_row_with_number("├──────────┼───────┼─────────┤"));
    }

    #[test]
    fn detect_refusal_honest_no_phrases() {
        for s in [
            "I cannot complete this task",
            "this isn't possible",
            "no such file",
            "doesn't exist",
            "Sorry, I am unable to.",
        ] {
            assert_eq!(detect_refusal(s, 0), Some(RefusalKind::HonestNo), "missed: {s}");
        }
    }

    #[test]
    fn detect_refusal_case_insensitive() {
        assert_eq!(detect_refusal("I CANNOT do this", 0), Some(RefusalKind::HonestNo));
    }

    #[test]
    fn detect_refusal_clarification_only_when_no_plan() {
        // With a plan present, clarification phrases are normal Q&A and
        // don't count as refusals.
        assert_eq!(detect_refusal("could you provide more context?", 5), None);
        // With no plan, the same phrase signals a clarification refusal.
        assert_eq!(detect_refusal("could you provide more context?", 0), Some(RefusalKind::AskedForClarification));
    }

    #[test]
    fn detect_refusal_fabricated_completion() {
        // "Done." with no plan items = claimed success without enumerating any work.
        assert_eq!(detect_refusal("Done.", 0), Some(RefusalKind::ClaimedSuccessFalsely));
        // "All set." pattern too.
        assert_eq!(detect_refusal("All done — cleanup finished.", 0), Some(RefusalKind::ClaimedSuccessFalsely));
    }

    #[test]
    fn detect_refusal_completion_with_plan_is_not_refusal() {
        // "Done." after a real plan + actions is the legitimate finish
        // signal — should NOT misfire as ClaimedSuccessFalsely.
        assert_eq!(detect_refusal("Done.", 5), None);
    }

    #[test]
    fn detect_refusal_normal_action_returns_none() {
        let s = "Plan:\n- /tmp/junk\n- /tmp/more\nProceed?";
        assert_eq!(detect_refusal(s, 2), None);
    }

    #[test]
    fn coach_reply_text_strict_is_short() {
        let s = coach_reply_text(CoachMode::Strict).expect("strict has a reply");
        assert!(s.len() < 32, "strict reply should be short: {s}");
        assert!(s.to_lowercase().contains("yes"));
        // strict must not embed the rm recipe — that's permissive's job.
        assert!(!s.contains("rm -rf"));
    }

    #[test]
    fn coach_reply_text_permissive_contains_recipe() {
        let s = coach_reply_text(CoachMode::Permissive).expect("permissive has a reply");
        assert!(s.contains("rm -rf"));
        assert!(s.contains("__pycache__"));
    }

    #[test]
    fn coach_reply_text_off_returns_none() {
        assert!(coach_reply_text(CoachMode::Off).is_none());
    }

    #[test]
    fn shell_escape_wraps_plain_string() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn shell_escape_handles_embedded_quote() {
        // The POSIX recipe: 'foo'\''bar' = literal foo'bar
        assert_eq!(shell_escape("foo'bar"), "'foo'\\''bar'");
    }

    #[test]
    fn shell_escape_preserves_spaces_and_slashes() {
        assert_eq!(shell_escape("/path with/spaces/opencode"), "'/path with/spaces/opencode'");
    }

    #[test]
    fn sanitize_session_strips_unsafe_chars() {
        assert_eq!(sanitize_session("cleanup-pycache-debris"), "cleanup-pycache-debris");
        assert_eq!(sanitize_session("with/slashes:and:colons"), "with-slashes-and-colons");
    }

    #[test]
    fn sanitize_session_caps_length() {
        let long = "a".repeat(100);
        assert_eq!(sanitize_session(&long).len(), 40);
    }

    #[test]
    fn slice_after_prompt_returns_text_past_prompt() {
        let prompt = "Cleanup task: __pycache__ debris\n\nDo X";
        let pane = "boot\n\nCleanup task: __pycache__ debris\n\nDo X\nAGENT: DELETE: foo\nProceed?";
        let agent = slice_after_prompt(pane, prompt);
        assert!(agent.contains("DELETE: foo"));
        assert!(agent.contains("Proceed?"));
        // The "Cleanup task" line is BEFORE the slice point.
        assert!(!agent.contains("Cleanup task"));
    }

    #[test]
    fn slice_after_prompt_uses_last_occurrence() {
        // If the prompt appears twice (echoed once in pane chrome,
        // once in scrollback), we want the slice AFTER the last copy
        // — that's where the agent's reply lives.
        let prompt = "Hello agent please clean";
        let pane = "Hello agent please clean — pasted\necho\nHello agent please clean\nAGENT: DELETE x\nProceed?";
        let agent = slice_after_prompt(pane, prompt);
        assert!(agent.contains("DELETE x"));
    }

    #[test]
    fn slice_after_prompt_falls_back_to_full_pane_if_not_found() {
        let prompt = "this prompt was reflowed by tmux into something the rfind can't find";
        let pane = "garbled tmux output\nAGENT: something\nProceed?";
        let agent = slice_after_prompt(pane, prompt);
        // Fall back to the whole pane — better to overcount than to
        // silently lose the agent's output.
        assert_eq!(agent, pane);
    }

    #[test]
    fn slice_after_prompt_short_prompt_falls_back() {
        // Prompts shorter than the 8-char floor are unsafe to match
        // (false positives in any normal pane), so we fall back.
        let prompt = "hi";
        let pane = "lots of text here ... hi ... and more";
        let agent = slice_after_prompt(pane, prompt);
        assert_eq!(agent, pane);
    }

    #[test]
    fn smooth_shell_cmd_includes_bench_env_vars() {
        let cmd = smooth_shell_cmd(&PathBuf::from("/usr/local/bin/th"), Some("smooai/deepseek-v4-flash"));
        assert!(cmd.contains("SMOOTH_BENCH_FRESH_SESSION=1"));
        assert!(cmd.contains("SMOOTH_BENCH_TRACE_TOOLS=1"));
        assert!(cmd.contains("'/usr/local/bin/th'"));
        assert!(cmd.contains(" code "));
        assert!(cmd.contains("--model 'smooai/deepseek-v4-flash'"));
    }

    #[test]
    fn smooth_shell_cmd_passes_auto_approve_session() {
        // Pearl th-fa4da9 — without --auto-approve=session, every
        // destructive bash from the agent is denied by the Safehouse
        // Narc default in unattended mode.
        let cmd = smooth_shell_cmd(&PathBuf::from("th"), None);
        assert!(cmd.contains("--auto-approve=session"), "missing auto-approve in: {cmd}");
    }

    #[test]
    fn smooth_shell_cmd_without_model_omits_model_flag() {
        let cmd = smooth_shell_cmd(&PathBuf::from("th"), None);
        assert!(cmd.contains(" code"));
        assert!(!cmd.contains("--model"));
    }

    #[test]
    fn smooth_driver_name_is_smooth() {
        let d = SmoothDriver::with_binary(PathBuf::from("th"));
        assert_eq!(d.name(), "smooth");
    }

    #[tokio::test]
    async fn smooth_driver_with_bogus_binary_returns_tmux_boot_error() {
        // `sh -c` with a missing binary exits immediately and the
        // first-render gate times out → driver surfaces this as
        // agent_error rather than crashing the sweep. Same shape as
        // the OpenCode equivalent.
        let driver = SmoothDriver::with_binary(PathBuf::from("/definitely/not/a/real/path/th-xyz-123"));
        let tmp = tempfile::tempdir().unwrap();
        let art = driver
            .dispatch(DispatchRequest {
                task_id: "t",
                workspace: tmp.path(),
                prompt: "hi",
                model: None,
                timeout: Duration::from_secs(2),
                coach: CoachMode::Permissive,
            })
            .await
            .unwrap();
        let err = art.agent_error.as_deref().unwrap_or_default();
        assert!(
            err.contains("boot failed") || err.contains("never settled") || err.contains("paste failed"),
            "unexpected agent_error: {err}",
        );
    }

    #[test]
    fn opencode_shell_cmd_without_env_prefix() {
        // Sanity check that the OpenCode and Smooth shell-cmd builders
        // diverge only in the env prefix — making this explicit so a
        // future refactor doesn't accidentally cross-pollute them.
        let cmd = opencode_shell_cmd(&PathBuf::from("/opt/opencode"), Some("smooai/deepseek-v4-flash"));
        assert!(!cmd.contains("SMOOTH_BENCH_FRESH_SESSION"));
        assert!(cmd.starts_with("'/opt/opencode'"));
        assert!(cmd.contains("--model 'smooai/deepseek-v4-flash'"));
    }

    #[tokio::test]
    async fn mock_driver_runs_script_and_parses_stdout() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("agent.sh");
        std::fs::write(
            &script,
            "#!/usr/bin/env bash\nset -e\necho 'DELETE: /tmp/junk'\necho 'DELETE: /tmp/more'\necho 'DELETE: /tmp/even-more'\necho 'Proceed?'\n",
        )
        .unwrap();
        // chmod +x not needed since we invoke `bash <script>`.

        let workspace = tmp.path().join("work");
        std::fs::create_dir_all(&workspace).unwrap();

        let driver = MockAgentDriver::new(script);
        let art = driver
            .dispatch(DispatchRequest {
                task_id: "t",
                workspace: &workspace,
                prompt: "ignored",
                model: None,
                timeout: Duration::from_secs(5),
                coach: CoachMode::Permissive,
            })
            .await
            .unwrap();
        assert!(art.prompted_for_confirmation);
        assert_eq!(art.plan_item_count, 3);
        assert!(art.agent_error.is_none());
    }

    #[tokio::test]
    async fn mock_driver_surfaces_nonzero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("agent.sh");
        std::fs::write(&script, "#!/usr/bin/env bash\nexit 7\n").unwrap();

        let driver = MockAgentDriver::new(script);
        let art = driver
            .dispatch(DispatchRequest {
                task_id: "t",
                workspace: tmp.path(),
                prompt: "",
                model: None,
                timeout: Duration::from_secs(5),
                coach: CoachMode::Permissive,
            })
            .await
            .unwrap();
        assert!(art.agent_error.as_deref().unwrap_or_default().contains("7"));
    }

    #[tokio::test]
    async fn mock_driver_times_out() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("agent.sh");
        std::fs::write(&script, "#!/usr/bin/env bash\nsleep 10\n").unwrap();

        let driver = MockAgentDriver::new(script);
        let art = driver
            .dispatch(DispatchRequest {
                task_id: "t",
                workspace: tmp.path(),
                prompt: "",
                model: None,
                timeout: Duration::from_millis(300),
                coach: CoachMode::Permissive,
            })
            .await
            .unwrap();
        assert!(art.agent_error.as_deref().unwrap_or_default().contains("timed out"));
    }

    #[tokio::test]
    async fn opencode_driver_without_binary_returns_clean_error() {
        let driver = OpenCodeDriver { binary: None };
        let tmp = tempfile::tempdir().unwrap();
        let art = driver
            .dispatch(DispatchRequest {
                task_id: "t",
                workspace: tmp.path(),
                prompt: "hi",
                model: None,
                timeout: Duration::from_secs(5),
                coach: CoachMode::Permissive,
            })
            .await
            .unwrap();
        assert!(art.agent_error.as_deref().unwrap_or_default().contains("not found"));
    }

    #[tokio::test]
    async fn opencode_driver_with_bogus_binary_returns_tmux_boot_error() {
        // With the TUI-via-tmux path, a bogus binary path makes `sh -c`
        // exit immediately and tmux's wait_for_first_render times out.
        // We surface that as `opencode tmux boot failed: …` rather than
        // crashing the sweep.
        let driver = OpenCodeDriver {
            binary: Some(PathBuf::from("/definitely/not/a/real/path/opencode-xyz-123")),
        };
        let tmp = tempfile::tempdir().unwrap();
        let art = driver
            .dispatch(DispatchRequest {
                task_id: "t",
                workspace: tmp.path(),
                prompt: "hi",
                model: None,
                // 2s budget: just long enough for boot_timeout to fire.
                // (boot_timeout is 30s by default but wait_for_first_render
                // returns earlier when the spawned `sh -c` exits.)
                timeout: Duration::from_secs(2),
                coach: CoachMode::Permissive,
            })
            .await
            .unwrap();
        let err = art.agent_error.as_deref().unwrap_or_default();
        // Match either "boot failed" (sh -c failed early) or "settle"
        // (sh -c never produced output) — both are acceptable signals
        // the driver caught the failure mode.
        assert!(
            err.contains("boot failed") || err.contains("never settled") || err.contains("paste failed"),
            "unexpected agent_error: {err}",
        );
    }
}
