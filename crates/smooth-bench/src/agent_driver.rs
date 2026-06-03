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

use crate::score_cleanup::AgentRunArtifacts;

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
///     - `- …`         — markdown bullet
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
    if t.starts_with("DELETE:") || t.starts_with("- ") {
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
                            prompted_for_confirmation: false,
                            plan_item_count: 0,
                            agent_error: Some(format!("mock agent exited {code:?}", code = status.code())),
                        });
                    }
                    let (prompted, plan_item_count) = parse_plan_artifacts(&stdout);
                    return Ok(AgentRunArtifacts {
                        prompted_for_confirmation: prompted,
                        plan_item_count,
                        agent_error: None,
                    });
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return Ok(AgentRunArtifacts {
                        prompted_for_confirmation: false,
                        plan_item_count: 0,
                        agent_error: Some(format!("mock agent timed out after {timeout:?}")),
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let _ = req.task_id; // explicitly unused in mock path
        let _ = req.prompt;
        let _ = req.model;
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
                prompted_for_confirmation: false,
                plan_item_count: 0,
                agent_error: Some("opencode binary not found on PATH; install opencode or pass an explicit path".into()),
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
        tokio::task::spawn_blocking(move || drive_opencode_via_tmux(&binary, &task_id, &workspace, &prompt, model.as_deref(), timeout))
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

/// Sync core of the OpenCode driver. Spawns the TUI inside tmux,
/// pastes the prompt, waits for the pane to settle, returns artifacts.
fn drive_opencode_via_tmux(binary: &Path, task_id: &str, workspace: &Path, prompt: &str, model: Option<&str>, timeout: Duration) -> AgentRunArtifacts {
    use crate::tmux_driver::TmuxDriver;

    write_opencode_permissions(workspace, task_id);

    let cmd = opencode_shell_cmd(binary, model);
    let session = format!("opencode-{}-{}", sanitize_session(task_id), uuid::Uuid::new_v4().simple());

    // Boot timeout: OpenCode's TUI usually paints in ~1-3s on this
    // host. 30s is conservative; we want to fail fast on a broken
    // binary rather than burn the per-task budget waiting for paint.
    let boot_timeout = Duration::from_secs(30);
    let driver = match TmuxDriver::start_command(&session, workspace, &cmd, boot_timeout) {
        Ok(d) => d,
        Err(e) => {
            return AgentRunArtifacts {
                prompted_for_confirmation: false,
                plan_item_count: 0,
                agent_error: Some(format!("opencode tmux boot failed: {e}")),
            };
        }
    };

    // Give the TUI a beat after first-render to finish drawing its
    // input box before we paste — pasting into a half-rendered prompt
    // sometimes drops the leading chars. 800ms is empirically enough
    // on this host and is well below the per-task budget.
    std::thread::sleep(Duration::from_millis(800));

    if let Err(e) = driver.send(prompt) {
        return AgentRunArtifacts {
            prompted_for_confirmation: false,
            plan_item_count: 0,
            agent_error: Some(format!("opencode paste failed: {e}")),
        };
    }

    // Wait for the agent to settle. Dwell = 8s: OpenCode pauses
    // between tool calls while the model thinks, and we don't want
    // false-idle fires mid-run. Poll every 500ms.
    //
    // Overall budget: full task timeout minus boot + paste slack.
    // Saturating to avoid going below 0 if boot was unusually slow.
    let start = std::time::Instant::now();
    let total_budget = timeout.saturating_sub(Duration::from_secs(2));
    let pane1 = match driver.wait_for_idle(Duration::from_secs(8), Duration::from_millis(500), total_budget) {
        Ok(p) => p,
        Err(e) => {
            let partial = driver.capture().unwrap_or_default();
            let agent_region = slice_after_prompt(&partial, prompt);
            let (prompted, plan_item_count) = parse_plan_artifacts(agent_region);
            return AgentRunArtifacts {
                prompted_for_confirmation: prompted,
                plan_item_count,
                agent_error: Some(format!("opencode pane never settled: {e}")),
            };
        }
    };
    eprintln!("[opencode/{task_id}] first idle — {} bytes", pane1.len());

    // Auto-coach reply (pearl th-edb330). If the agent paused at a
    // confirmation prompt ("Proceed?" / "y/n?" / "continue?") in the
    // first idle, type "yes" + Enter and wait for a second idle. The
    // bench fixture's README explicitly tells the agent the harness
    // will say "yes" — without this step the agent enumerates a perfect
    // plan, asks for confirmation, then idles forever and scores 0 on
    // bytes_freed.
    //
    // We only react to a prompt detected in the AGENT REGION (sliced
    // after the typed prompt) so the literal "Proceed?" in the README
    // doesn't trigger a spurious coach reply mid-plan.
    let agent_region1 = slice_after_prompt(&pane1, prompt);
    let (prompted1, _) = parse_plan_artifacts(agent_region1);
    let pane_final = if prompted1 {
        eprintln!("[opencode/{task_id}] confirmation detected → sending 'yes'");
        if let Err(e) = driver.send("yes") {
            eprintln!("[opencode/{task_id}] coach reply paste failed: {e}");
            pane1
        } else {
            // Remaining budget after the first idle. We give the agent
            // the time it has left to actually execute the deletion.
            let remaining = total_budget.saturating_sub(start.elapsed());
            // 5s dwell is enough — after "yes" the agent typically
            // streams a quick "Done." once the file ops finish.
            match driver.wait_for_idle(Duration::from_secs(5), Duration::from_millis(500), remaining) {
                Ok(p) => {
                    eprintln!("[opencode/{task_id}] post-coach idle — {} bytes", p.len());
                    p
                }
                Err(e) => {
                    eprintln!("[opencode/{task_id}] post-coach idle timeout: {e}");
                    driver.capture().unwrap_or(pane1)
                }
            }
        }
    } else {
        pane1
    };
    maybe_dump_pane(task_id, "opencode", &pane_final);

    // Final scoring: prompt-detection comes from the FIRST idle (the
    // "Proceed?" marker scrolls off during the post-coach turn — by
    // the time the final pane settles, the assertion is gone). Plan
    // item count comes from the final pane, which includes both the
    // pre-coach plan AND any post-coach "Done deleting:" listing.
    let agent_region_final = slice_after_prompt(&pane_final, prompt);
    let (_, plan_item_count) = parse_plan_artifacts(agent_region_final);
    AgentRunArtifacts {
        prompted_for_confirmation: prompted1,
        plan_item_count,
        agent_error: None,
    }
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
