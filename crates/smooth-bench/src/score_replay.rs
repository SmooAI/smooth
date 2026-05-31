//! `smooth-bench score-replay` — replay real-world merged PRs through
//! the agent under test.
//!
//! Why this exists: aider-polyglot is a curated set of well-shaped
//! exercises, which is great for trend tracking but a poor proxy for
//! "would this agent ship code at a real company?". Real PRs are
//! noisier, larger, and often have ambiguous instructions. The replay
//! harness harvests merged PRs from a target repo (default: the same
//! repo the bench runs in), checks out the base SHA, hands the agent
//! the PR title + body as the prompt, and grades by re-running the
//! test files the human added.
//!
//! Per-PR flow:
//!
//! 1. [`pr_harvest::harvest_prs`] — `gh pr list --json …` for eligible
//!    PRs (≥3 files, ≥1 test file).
//! 2. `git clone --filter=blob:none` of the repo (cached across PRs
//!    in the same sweep), `git fetch` the specific base SHA, check
//!    it out into a per-PR work dir.
//! 3. Write `PROMPT.txt` with the PR title and `INSTRUCTIONS.md`
//!    with the body + a pointer to the test files the human PR added.
//! 4. Drive `th code` via tmux (reuses [`tui_score::run_polyglot_task_via_tui`]'s
//!    inner loop) — see `replay_one` for the boundary we expose so
//!    tests can swap in a no-op driver.
//! 5. Run the test command for the detected workspace via
//!    [`lang_detect::test_command`]; declare `solved=true` if ALL
//!    tests pass.
//!
//! Caveats / known limitations (will surface in the `--help` and
//! README once `main.rs` is wired):
//!
//! - **Auth**: `gh` must be logged in. We surface a clear error at
//!   the start of the harvest call, not mid-sweep.
//! - **Specific-SHA fetch**: `--depth=1` alone can't fetch arbitrary
//!   commits. We clone shallow, then `git fetch origin <sha>` to
//!   pull the base.
//! - **Test selection**: v1 runs *all* tests in test files the human
//!   touched; we don't parse out the specific test names the human
//!   added. False positives possible if those files already had
//!   passing tests pre-PR.
//! - **Mocking `gh` in tests**: the real `gh` call site is hidden
//!   behind [`pr_harvest::GhCli`]; tests inject a stub. There's also
//!   the `SMOOTH_BENCH_GH_BIN` env override for end-to-end tests
//!   that want to point at a fixture script.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::human_driver::DriverPersona;
use crate::lang_detect::{detect, test_command, Workspace};
use crate::pr_harvest::{harvest_prs_with, GhCli, HarvestedPR, RealGh};
use crate::score::{median_ms, LanguageScore, Score};

/// Top-level config for a `score-replay` sweep.
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    /// `"owner/repo"` form for `gh pr list --repo`.
    pub repo: String,
    /// Lower bound on merge date — only PRs merged on or after this
    /// date are considered.
    pub since: NaiveDate,
    /// Maximum number of PRs to replay. Eligibility filtering
    /// happens upstream so this is the cap on actual runs, not on
    /// the harvest pool.
    pub task_limit: usize,
    /// Forwarded to `th code --model NAME`. Empty string = use the
    /// agent's default routing.
    pub under_test_model: String,
    /// Driver-LLM persona for the human-loop (user vs. coach).
    pub driver_persona: DriverPersona,
    /// Per-run scratch root. Typically `~/.smooth/bench-runs/<id>`.
    pub work_root: PathBuf,
}

/// Per-PR outcome — captured into the sweep aggregate. Mirrors
/// `TaskOutcome` plus replay-specific fields (PR number, the human
/// test files we ran).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayOutcome {
    pub pr_number: u64,
    pub solved: bool,
    pub cost_usd: f64,
    pub duration_ms: u64,
    pub workspace: String,
    pub ran_test_files: Vec<PathBuf>,
    /// Parsed pass count from the test runner. Best-effort; 0 if the
    /// runner shape isn't one we parse natively.
    pub tests_passed: u32,
    /// Parsed fail count. > 0 → `solved = false`.
    pub tests_failed: u32,
}

/// What `run_replay_sweep` returns — the canonical aggregate `Score`
/// (so it slots into the same artifact path as the polyglot sweep)
/// plus the per-PR outcomes for diagnostics.
#[derive(Debug, Clone)]
pub struct ReplaySweepRun {
    pub score: Score,
    pub per_pr: Vec<ReplayOutcome>,
}

/// Drive the agent under test through one prepared workdir. The
/// production impl spawns `th code` in tmux and runs the human-loop;
/// tests inject a no-op or hand-rolled driver so the end-to-end test
/// can verify aggregation without an LLM.
///
/// `workdir` is the per-PR directory after `git checkout <base_sha>`
/// and after `PROMPT.txt`/`INSTRUCTIONS.md` have been written. The
/// driver's job is to edit the workdir; the harness scores it.
///
/// Returning `(turns, cost_usd)` is enough for the aggregator —
/// solved/failed is decided downstream by re-running the human's
/// test files.
#[async_trait]
pub trait ReplayDriver: Send + Sync {
    async fn drive_workdir(&self, workdir: &Path, prompt: &str) -> Result<DriveSummary>;
}

/// Output of one [`ReplayDriver::drive_workdir`] call. The downstream
/// scorer runs the test command itself, so the driver doesn't have to
/// report pass/fail.
#[derive(Debug, Clone, Default)]
pub struct DriveSummary {
    pub turns: usize,
    pub cost_usd: f64,
}

/// Boundary for "fetch the PR's source code into `target`". Production
/// implementation shells out to `git`. Tests inject a stub that
/// pre-seeds the directory.
#[async_trait]
pub trait RepoFetcher: Send + Sync {
    /// Place the repo content as it was at `base_sha` into `target`.
    /// `target` is already created and empty.
    async fn fetch_at(&self, repo: &str, base_sha: &str, target: &Path) -> Result<()>;
}

/// Production fetcher: shallow clone + targeted SHA fetch.
///
/// `--filter=blob:none` keeps the initial clone fast; we then do a
/// targeted `git fetch origin <sha>` because a naive `--depth=1`
/// clone of the default branch won't have the PR's base SHA in its
/// history.
pub struct GitFetcher;

#[async_trait]
impl RepoFetcher for GitFetcher {
    async fn fetch_at(&self, repo: &str, base_sha: &str, target: &Path) -> Result<()> {
        let url = format!("https://github.com/{repo}.git");
        let status = tokio::process::Command::new("git")
            .args(["clone", "--filter=blob:none", "--no-checkout", &url, "."])
            .current_dir(target)
            .status()
            .await
            .with_context(|| format!("spawning git clone {url}"))?;
        if !status.success() {
            anyhow::bail!("git clone of {repo} failed with status {:?}", status.code());
        }
        let status = tokio::process::Command::new("git")
            .args(["fetch", "origin", base_sha])
            .current_dir(target)
            .status()
            .await
            .context("spawning git fetch")?;
        if !status.success() {
            anyhow::bail!("git fetch origin {base_sha} failed with status {:?}", status.code());
        }
        let status = tokio::process::Command::new("git")
            .args(["checkout", base_sha])
            .current_dir(target)
            .status()
            .await
            .context("spawning git checkout")?;
        if !status.success() {
            anyhow::bail!("git checkout {base_sha} failed with status {:?}", status.code());
        }
        Ok(())
    }
}

/// Run the full replay sweep against the real `gh`, `git`, and `th`
/// binaries. Wraps [`run_replay_sweep_with`] with production
/// implementations for each boundary.
///
/// # Errors
/// Setup errors propagate (auth, missing tools, scratch dir).
/// Per-PR runner failures degrade to `solved=false` and are logged
/// to stderr.
pub async fn run_replay_sweep(cfg: &ReplayConfig) -> Result<Score> {
    let run = run_replay_sweep_with(cfg, &RealGh, &GitFetcher, &ThCodeReplayDriver::new(cfg)).await?;
    Ok(run.score)
}

/// `run_replay_sweep` with injectable boundaries.
///
/// # Errors
/// As [`run_replay_sweep`].
pub async fn run_replay_sweep_with(cfg: &ReplayConfig, gh: &dyn GhCli, fetcher: &dyn RepoFetcher, driver: &dyn ReplayDriver) -> Result<ReplaySweepRun> {
    std::fs::create_dir_all(&cfg.work_root).with_context(|| format!("mkdir {}", cfg.work_root.display()))?;

    let harvested = harvest_prs_with(gh, &cfg.repo, cfg.since, cfg.task_limit)
        .await
        .with_context(|| format!("harvesting PRs from {}", cfg.repo))?;
    if harvested.is_empty() {
        // Surface this as an empty Score, not an error. The CI gate
        // can treat "no eligible PRs" as a no-op rather than a hard
        // failure.
        return Ok(ReplaySweepRun {
            score: empty_score(cfg),
            per_pr: Vec::new(),
        });
    }

    let mut per_pr: Vec<ReplayOutcome> = Vec::with_capacity(harvested.len());
    let mut durations_ms: Vec<u64> = Vec::with_capacity(harvested.len());
    let mut cumulative_cost = 0.0_f64;

    for (idx, pr) in harvested.iter().enumerate() {
        let outcome = match replay_one(cfg, fetcher, driver, pr).await {
            Ok(o) => o,
            Err(e) => {
                eprintln!("score-replay: PR #{} runner error: {e:#}", pr.number);
                ReplayOutcome {
                    pr_number: pr.number,
                    solved: false,
                    cost_usd: 0.0,
                    duration_ms: 0,
                    workspace: "unknown".into(),
                    ran_test_files: pr.test_files.clone(),
                    tests_passed: 0,
                    tests_failed: 0,
                }
            }
        };
        cumulative_cost += outcome.cost_usd;
        durations_ms.push(outcome.duration_ms);
        let tag = if outcome.solved { "PASS" } else { "FAIL" };
        println!(
            "[{idx:>3}/{total:>3}] {tag}  PR #{pr:<6}  {ws:<8}  {ms:>6}ms  ${cost:.4}  (total ${cum:.4})",
            idx = idx + 1,
            total = harvested.len(),
            pr = pr.number,
            ws = outcome.workspace,
            ms = outcome.duration_ms,
            cost = outcome.cost_usd,
            cum = cumulative_cost,
        );
        per_pr.push(outcome);
    }

    let score = aggregate(cfg, &per_pr, cumulative_cost, &durations_ms);
    Ok(ReplaySweepRun { score, per_pr })
}

/// Run a single PR through the replay flow. Exposed so callers can
/// drive a one-PR debug run without building a full `ReplayConfig`
/// + sweep loop.
///
/// # Errors
/// Setup (clone, write prompt) and driver errors propagate. Test-
/// command failures are reflected as `solved=false` in the outcome,
/// not propagated.
pub async fn replay_one(cfg: &ReplayConfig, fetcher: &dyn RepoFetcher, driver: &dyn ReplayDriver, pr: &HarvestedPR) -> Result<ReplayOutcome> {
    let t0 = Instant::now();
    let workdir = cfg.work_root.join(format!("pr-{}", pr.number));
    if workdir.exists() {
        // Idempotent re-runs of the same sweep id reuse the dir but
        // do not silently inherit half-clobbered state — wipe first.
        std::fs::remove_dir_all(&workdir).with_context(|| format!("removing stale workdir {}", workdir.display()))?;
    }
    std::fs::create_dir_all(&workdir).with_context(|| format!("mkdir {}", workdir.display()))?;

    fetcher
        .fetch_at(&cfg.repo, &pr.base_sha, &workdir)
        .await
        .with_context(|| format!("fetching {} @ {}", cfg.repo, pr.base_sha))?;

    let (prompt, instructions) = build_prompt_and_instructions(pr);
    std::fs::write(workdir.join("PROMPT.txt"), &prompt).context("writing PROMPT.txt")?;
    std::fs::write(workdir.join("INSTRUCTIONS.md"), &instructions).context("writing INSTRUCTIONS.md")?;

    let drive = driver
        .drive_workdir(&workdir, &prompt)
        .await
        .context("agent under test failed to drive workdir")?;

    let ws = detect(&workdir);
    let workspace_label = workspace_label(&ws);
    let argv_list = test_command(&ws, &pr.test_files);
    let mut tests_passed = 0u32;
    let mut tests_failed = 0u32;
    let mut solved = !argv_list.is_empty();
    for argv in &argv_list {
        let (passed, failed) = run_one_test_command(&workdir, argv).await;
        tests_passed = tests_passed.saturating_add(passed);
        tests_failed = tests_failed.saturating_add(failed);
        if failed > 0 || passed == 0 {
            solved = false;
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let duration_ms = (t0.elapsed().as_secs_f64() * 1000.0).max(0.0) as u64;

    Ok(ReplayOutcome {
        pr_number: pr.number,
        solved,
        cost_usd: drive.cost_usd,
        duration_ms,
        workspace: workspace_label.into(),
        ran_test_files: pr.test_files.clone(),
        tests_passed,
        tests_failed,
    })
}

/// Build the agent-facing prompt + the supporting INSTRUCTIONS.md.
///
/// The prompt is intentionally one line (TUI handles \n as Enter —
/// see lib.rs::build_prompt comment for the historical pearl).
/// INSTRUCTIONS.md gets the full body + the test-file list since the
/// agent is told to read it.
#[must_use]
pub fn build_prompt_and_instructions(pr: &HarvestedPR) -> (String, String) {
    let test_files: Vec<String> = pr.test_files.iter().map(|p| p.display().to_string()).collect();
    let test_files_md = if test_files.is_empty() {
        "(none — workspace test command will run the full suite)".into()
    } else {
        test_files.iter().map(|p| format!("- `{p}`")).collect::<Vec<_>>().join("\n")
    };
    let prompt = format!(
        "Reproduce the behavior change described in PR #{}: \"{}\". Read INSTRUCTIONS.md for the full description and the list of test files that grade the work. Edit source files until the test command for this workspace passes. Do NOT modify the listed test files; they are the grader.",
        pr.number, pr.title,
    );
    let instructions = format!(
        "# PR #{number}: {title}\n\n## Description\n\n{body}\n\n## Grading test files\n\n{tests}\n\nThese are the files we run after you finish. Your edits should make their tests pass. Modifying them is forbidden — the harness will detect changes.\n",
        number = pr.number,
        title = pr.title,
        body = if pr.body.is_empty() { "(no body provided)" } else { pr.body.as_str() },
        tests = test_files_md,
    );
    (prompt, instructions)
}

fn workspace_label(ws: &Workspace) -> &'static str {
    match ws {
        Workspace::Cargo => "cargo",
        Workspace::Pytest => "pytest",
        Workspace::Npm => "npm",
        Workspace::Mixed(_) => "mixed",
        Workspace::Unknown => "unknown",
    }
}

/// Run one test command and best-effort parse `(passed, failed)`.
async fn run_one_test_command(workdir: &Path, argv: &[String]) -> (u32, u32) {
    let program = &argv[0];
    let rest = &argv[1..];
    let isolated_target = workdir.join("target");
    let output = match tokio::process::Command::new(program)
        .args(rest)
        .current_dir(workdir)
        .env("CARGO_TARGET_DIR", &isolated_target)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("score-replay: spawning `{}` failed: {e:#}", argv.join(" "));
            return (0, 0);
        }
    };
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    parse_summary_any(&combined)
}

/// Try the native regex parsers in order. We try every parser since
/// we don't know the language at parse time — at most one matches.
fn parse_summary_any(combined: &str) -> (u32, u32) {
    if let Some(c) = crate::parse_cargo_summary(combined) {
        return (c.passed, c.failed);
    }
    if let Some(c) = crate::parse_pytest_summary(combined) {
        return (c.passed, c.failed);
    }
    if let Some(c) = crate::parse_jest_summary(combined) {
        return (c.passed, c.failed);
    }
    (0, 0)
}

fn empty_score(_cfg: &ReplayConfig) -> Score {
    Score {
        smooth_version: env!("CARGO_PKG_VERSION").into(),
        commit_sha: String::new(),
        ran_at: chrono::Utc::now(),
        overall_pass_rate: 0.0,
        by_language: BTreeMap::new(),
        tasks_attempted: 0,
        tasks_green: 0,
        tasks_inconclusive: 0,
        cost_usd: 0.0,
        median_task_ms: 0,
        budget_usd_cap: 0.0,
        budget_usd_hit: false,
        // Carry repo + driver info through ran_at? No — Score's
        // fields are fixed. The per-PR detail lives in `ReplaySweepRun.per_pr`.
        // We surface `cfg.repo` only for the empty-result diagnostic
        // log line above; not in the persisted Score.
        // (We intentionally do NOT add new fields to Score from this
        // module — Score is a stable serialization contract.)
    }
}

fn aggregate(_cfg: &ReplayConfig, per_pr: &[ReplayOutcome], cost_usd: f64, durations_ms: &[u64]) -> Score {
    let mut by_language: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut tasks_attempted: u32 = 0;
    let mut tasks_green: u32 = 0;
    for outcome in per_pr {
        tasks_attempted += 1;
        if outcome.solved {
            tasks_green += 1;
        }
        let entry = by_language.entry(outcome.workspace.clone()).or_insert((0, 0));
        entry.0 += 1;
        if outcome.solved {
            entry.1 += 1;
        }
    }
    let overall_pass_rate = if tasks_attempted == 0 {
        0.0
    } else {
        f64::from(tasks_green) / f64::from(tasks_attempted)
    };
    let by_language = by_language
        .into_iter()
        .map(|(k, (att, green))| (k, LanguageScore::from_counts(att, green)))
        .collect();
    Score {
        smooth_version: env!("CARGO_PKG_VERSION").into(),
        commit_sha: String::new(),
        ran_at: chrono::Utc::now(),
        overall_pass_rate,
        by_language,
        tasks_attempted,
        tasks_green,
        tasks_inconclusive: 0,
        cost_usd,
        median_task_ms: median_ms(durations_ms),
        budget_usd_cap: 0.0,
        budget_usd_hit: false,
    }
}

// -- Production driver -----------------------------------------------
//
// The production `ReplayDriver` shells out to `th code` via tmux and
// runs the LLM-as-human loop. It's a thin wrapper around the existing
// `tui_score` plumbing (TmuxDriver + run_human_loop), specialized to
// the score-replay workdir layout.

/// Production driver: spawns `th code` in tmux against the workdir
/// and runs the LLM-as-human loop. Kept thin — most of the heavy
/// lifting is in `tui_score::run_polyglot_task_via_tui`. We don't
/// reuse that fn directly because it assumes the polyglot dataset
/// shape (calls `prepare_task` / `finalize_and_score`); the replay
/// path has its own setup + scoring.
pub struct ThCodeReplayDriver {
    th_binary: String,
    tmux_session_prefix: String,
    under_test_model: String,
    driver_persona: DriverPersona,
}

impl ThCodeReplayDriver {
    /// Build from a `ReplayConfig`. Picks reasonable defaults for the
    /// tmux session prefix.
    #[must_use]
    pub fn new(cfg: &ReplayConfig) -> Self {
        Self {
            th_binary: "th".into(),
            tmux_session_prefix: "smooth-bench-replay".into(),
            under_test_model: cfg.under_test_model.clone(),
            driver_persona: cfg.driver_persona,
        }
    }
}

#[async_trait]
impl ReplayDriver for ThCodeReplayDriver {
    async fn drive_workdir(&self, workdir: &Path, prompt: &str) -> Result<DriveSummary> {
        // Importing types fresh here (rather than reusing
        // tui_score::run_polyglot_task_via_tui) because the polyglot
        // path mixes setup + score; replay has its own. The actual
        // tmux + driver-loop wiring is identical though.
        use std::time::Duration;

        use crate::human_driver::{run_human_loop, LlmDriverModel, LoopConfig, LoopExit};
        use crate::tmux_driver::TmuxDriver;

        let session = format!(
            "{}-pr-{}",
            self.tmux_session_prefix,
            workdir.file_name().and_then(|n| n.to_str()).unwrap_or("x")
        );
        let cost_sidecar_path = workdir.join("replay.cost.json");
        let cost_sidecar_str = cost_sidecar_path.to_string_lossy().to_string();
        // Match the env-prefix shape used by tui_score so any
        // smooth-code instrumentation behaves identically (fresh
        // session store, cost sidecar).
        let env_prefix = format!(
            "SMOOTH_BENCH_FRESH_SESSION=1 SMOOTH_BENCH_TRACE_TOOLS=1 SMOOTH_BENCH_COST_SIDECAR={}",
            shell_quote(&cost_sidecar_str)
        );
        let shell_cmd = if self.under_test_model.is_empty() {
            format!("{} {} code", env_prefix, shell_quote(&self.th_binary))
        } else {
            format!(
                "{} {} code --model {}",
                env_prefix,
                shell_quote(&self.th_binary),
                shell_quote(&self.under_test_model)
            )
        };

        let driver = TmuxDriver::start_command(&session, workdir, &shell_cmd, Duration::from_secs(120)).context("spawn th code in tmux for replay")?;

        let driver_model = LlmDriverModel::from_activity_with_persona(smooth_operator::providers::Activity::Summarize, self.driver_persona)
            .context("constructing driver LLM")?;

        let cfg = LoopConfig::default();
        let loop_result = run_human_loop(&driver, &driver_model, prompt, prompt, &cfg)
            .await
            .context("running human loop in replay")?;

        // Cost: sidecar JSON written by smooth-code. We deliberately
        // do NOT fall back to pane-scrape here — the replay path is
        // new and we'd rather report 0.0 + a warning than silently
        // fabricate a number from a stale status-line capture.
        let cost_usd = read_cost_sidecar(&cost_sidecar_path).unwrap_or(0.0);

        drop(driver);
        let turns = loop_result.turns;
        // exit reason is logged for human diagnosis but does NOT
        // factor into the outcome — scoring is by test-result alone.
        if !matches!(loop_result.exit, LoopExit::Complete) {
            eprintln!(
                "score-replay: human-loop exited as {:?} after {} turns (scoring proceeds anyway)",
                loop_result.exit, turns
            );
        }
        Ok(DriveSummary { turns, cost_usd })
    }
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '=')) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn read_cost_sidecar(path: &Path) -> Option<f64> {
    let bytes = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("cost_usd").and_then(serde_json::Value::as_f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_harvest::PrFile;
    use std::sync::Mutex;
    use tempfile::tempdir;

    fn fake_pr(number: u64) -> HarvestedPR {
        HarvestedPR {
            number,
            title: "Add greet to lib".to_string(),
            body: "Implements pub fn greet() returning \"hello\".".into(),
            base_sha: "base".into(),
            merge_sha: "merge".into(),
            files: vec![
                PrFile {
                    path: PathBuf::from("src/lib.rs"),
                    additions: 1,
                    deletions: 0,
                },
                PrFile {
                    path: PathBuf::from("tests/greet.rs"),
                    additions: 5,
                    deletions: 0,
                },
                PrFile {
                    path: PathBuf::from("Cargo.toml"),
                    additions: 1,
                    deletions: 0,
                },
            ],
            test_files: vec![PathBuf::from("tests/greet.rs")],
        }
    }

    /// Stub `gh` that returns a single eligible PR.
    struct StubGh;
    #[async_trait]
    impl GhCli for StubGh {
        async fn pr_list_json(&self, _repo: &str, _since: NaiveDate, _limit: usize) -> Result<String> {
            // Three files (≥3), with a test file → eligible.
            Ok(serde_json::json!([{
                "number": 42,
                "title": "Add greet to lib",
                "body": "Implements pub fn greet() returning \"hello\".",
                "baseRefOid": "base",
                "mergeCommit": { "oid": "merge" },
                "mergedAt": "2026-05-01T00:00:00Z",
                "files": [
                    { "path": "src/lib.rs", "additions": 1, "deletions": 0 },
                    { "path": "tests/greet.rs", "additions": 5, "deletions": 0 },
                    { "path": "Cargo.toml", "additions": 1, "deletions": 0 }
                ]
            }])
            .to_string())
        }
    }

    /// Stub fetcher that pre-seeds the workdir with a tiny cargo
    /// crate. Tracks whether it ran (no PR run should reach scoring
    /// without going through the fetcher first).
    struct SeedFetcher {
        seen: Mutex<Vec<(String, String)>>,
        /// If true, the seeded crate's source ALREADY implements
        /// `greet()` correctly — the test will pass without a
        /// driver edit.
        ///
        /// If false, the seed leaves `greet()` returning the wrong
        /// string and the driver is expected to fix it (handled by
        /// the corresponding ReplayDriver stub).
        seed_passes: bool,
    }
    #[async_trait]
    impl RepoFetcher for SeedFetcher {
        async fn fetch_at(&self, repo: &str, sha: &str, target: &Path) -> Result<()> {
            self.seen.lock().unwrap().push((repo.into(), sha.into()));
            std::fs::create_dir_all(target.join("src"))?;
            std::fs::create_dir_all(target.join("tests"))?;
            std::fs::write(
                target.join("Cargo.toml"),
                "[package]\nname = \"replay_test\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[lib]\npath = \"src/lib.rs\"\n",
            )?;
            let body = if self.seed_passes {
                "pub fn greet() -> &'static str { \"hello\" }\n"
            } else {
                "pub fn greet() -> &'static str { \"wrong\" }\n"
            };
            std::fs::write(target.join("src/lib.rs"), body)?;
            std::fs::write(
                target.join("tests/greet.rs"),
                "#[test]\nfn says_hello() { assert_eq!(replay_test::greet(), \"hello\"); }\n",
            )?;
            Ok(())
        }
    }

    /// No-op driver — leaves the workdir as the fetcher seeded it.
    /// Used to test "test command alone decides solved/unsolved".
    struct NoopDriver;
    #[async_trait]
    impl ReplayDriver for NoopDriver {
        async fn drive_workdir(&self, _workdir: &Path, _prompt: &str) -> Result<DriveSummary> {
            Ok(DriveSummary { turns: 0, cost_usd: 0.0 })
        }
    }

    /// Driver that edits the seeded source to make tests pass.
    /// Mimics what a competent agent under test would do.
    struct FixingDriver;
    #[async_trait]
    impl ReplayDriver for FixingDriver {
        async fn drive_workdir(&self, workdir: &Path, _prompt: &str) -> Result<DriveSummary> {
            std::fs::write(workdir.join("src/lib.rs"), "pub fn greet() -> &'static str { \"hello\" }\n")?;
            Ok(DriveSummary { turns: 3, cost_usd: 0.0023 })
        }
    }

    #[test]
    fn build_prompt_mentions_pr_number_and_title() {
        let pr = fake_pr(99);
        let (prompt, instructions) = build_prompt_and_instructions(&pr);
        assert!(prompt.contains("PR #99"), "{prompt}");
        assert!(prompt.contains("Add greet to lib"), "{prompt}");
        assert!(instructions.contains("tests/greet.rs"), "{instructions}");
        // Single-line prompt (TUI Enter footgun — same as polyglot
        // path).
        assert!(!prompt.contains('\n'), "prompt must be single-line for TUI submission");
    }

    #[test]
    fn build_prompt_handles_empty_body() {
        let mut pr = fake_pr(1);
        pr.body.clear();
        let (_p, instructions) = build_prompt_and_instructions(&pr);
        assert!(instructions.contains("(no body provided)"), "{instructions}");
    }

    #[test]
    fn workspace_label_covers_all_variants() {
        assert_eq!(workspace_label(&Workspace::Cargo), "cargo");
        assert_eq!(workspace_label(&Workspace::Pytest), "pytest");
        assert_eq!(workspace_label(&Workspace::Npm), "npm");
        assert_eq!(workspace_label(&Workspace::Mixed(vec![Workspace::Cargo])), "mixed");
        assert_eq!(workspace_label(&Workspace::Unknown), "unknown");
    }

    #[tokio::test]
    async fn empty_harvest_returns_empty_score_not_error() {
        struct EmptyGh;
        #[async_trait]
        impl GhCli for EmptyGh {
            async fn pr_list_json(&self, _repo: &str, _since: NaiveDate, _limit: usize) -> Result<String> {
                Ok("[]".into())
            }
        }
        let dir = tempdir().unwrap();
        let cfg = ReplayConfig {
            repo: "owner/repo".into(),
            since: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            task_limit: 5,
            under_test_model: String::new(),
            driver_persona: DriverPersona::default(),
            work_root: dir.path().to_path_buf(),
        };
        let run = run_replay_sweep_with(
            &cfg,
            &EmptyGh,
            &SeedFetcher {
                seen: Mutex::new(vec![]),
                seed_passes: true,
            },
            &NoopDriver,
        )
        .await
        .unwrap();
        assert_eq!(run.score.tasks_attempted, 0);
        assert_eq!(run.score.tasks_green, 0);
        assert!(run.per_pr.is_empty());
    }

    // Cargo-based tests in CI are gated on the host having `cargo`
    // available. The smooth-bench crate ALWAYS builds via cargo so
    // this is implicit — but we still gate behind an env var to
    // avoid running the cargo subprocess inside a sandbox-less CI
    // that doesn't have the target dir warmed up.
    fn cargo_available() -> bool {
        // The bench crate is itself a cargo crate, so `cargo` must
        // be on PATH for `cargo test -p smooai-smooth-bench` to even
        // invoke this test. Just verify it.
        std::process::Command::new("cargo")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn end_to_end_fixture_repo_with_passing_seed_scores_one() {
        if !cargo_available() {
            eprintln!("cargo not available, skipping");
            return;
        }
        let dir = tempdir().unwrap();
        let cfg = ReplayConfig {
            repo: "owner/repo".into(),
            since: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            task_limit: 5,
            under_test_model: String::new(),
            driver_persona: DriverPersona::default(),
            work_root: dir.path().to_path_buf(),
        };
        let fetcher = SeedFetcher {
            seen: Mutex::new(vec![]),
            seed_passes: true,
        };
        let run = run_replay_sweep_with(&cfg, &StubGh, &fetcher, &NoopDriver).await.unwrap();
        assert_eq!(run.score.tasks_attempted, 1, "should have run one eligible PR");
        assert_eq!(run.score.tasks_green, 1, "seed already passes greet() test");
        assert!((run.score.overall_pass_rate - 1.0).abs() < f64::EPSILON);
        // The single language bucket should be `cargo`.
        assert!(
            run.score.by_language.contains_key("cargo"),
            "expected cargo bucket, got {:?}",
            run.score.by_language
        );
        // Fetcher was actually invoked with the harvested SHA.
        assert_eq!(fetcher.seen.lock().unwrap().as_slice(), &[("owner/repo".into(), "base".into())]);
        assert_eq!(run.per_pr.len(), 1);
        assert_eq!(run.per_pr[0].pr_number, 42);
        assert!(run.per_pr[0].solved);
        assert!(run.per_pr[0].tests_passed >= 1);
    }

    #[tokio::test]
    async fn end_to_end_fixture_repo_with_failing_seed_and_fixing_driver_scores_one() {
        if !cargo_available() {
            eprintln!("cargo not available, skipping");
            return;
        }
        let dir = tempdir().unwrap();
        let cfg = ReplayConfig {
            repo: "owner/repo".into(),
            since: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            task_limit: 5,
            under_test_model: String::new(),
            driver_persona: DriverPersona::default(),
            work_root: dir.path().to_path_buf(),
        };
        let fetcher = SeedFetcher {
            seen: Mutex::new(vec![]),
            seed_passes: false,
        };
        let run = run_replay_sweep_with(&cfg, &StubGh, &fetcher, &FixingDriver).await.unwrap();
        assert_eq!(run.score.tasks_attempted, 1);
        assert_eq!(run.score.tasks_green, 1, "fixing driver wrote correct greet() body");
        assert!(run.score.cost_usd > 0.0, "fixing driver reports nonzero cost");
        assert_eq!(run.per_pr[0].tests_failed, 0);
    }

    #[tokio::test]
    async fn end_to_end_fixture_repo_with_failing_seed_and_noop_driver_scores_zero() {
        if !cargo_available() {
            eprintln!("cargo not available, skipping");
            return;
        }
        let dir = tempdir().unwrap();
        let cfg = ReplayConfig {
            repo: "owner/repo".into(),
            since: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            task_limit: 5,
            under_test_model: String::new(),
            driver_persona: DriverPersona::default(),
            work_root: dir.path().to_path_buf(),
        };
        let fetcher = SeedFetcher {
            seen: Mutex::new(vec![]),
            seed_passes: false,
        };
        let run = run_replay_sweep_with(&cfg, &StubGh, &fetcher, &NoopDriver).await.unwrap();
        assert_eq!(run.score.tasks_attempted, 1);
        assert_eq!(run.score.tasks_green, 0, "noop driver did not fix the failing seed");
        assert!(run.per_pr[0].tests_failed >= 1);
    }

    #[tokio::test]
    async fn fetcher_error_degrades_to_failed_outcome_not_propagated() {
        struct FailingFetcher;
        #[async_trait]
        impl RepoFetcher for FailingFetcher {
            async fn fetch_at(&self, _repo: &str, _sha: &str, _target: &Path) -> Result<()> {
                Err(anyhow::anyhow!("fake clone failure"))
            }
        }
        let dir = tempdir().unwrap();
        let cfg = ReplayConfig {
            repo: "owner/repo".into(),
            since: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            task_limit: 5,
            under_test_model: String::new(),
            driver_persona: DriverPersona::default(),
            work_root: dir.path().to_path_buf(),
        };
        let run = run_replay_sweep_with(&cfg, &StubGh, &FailingFetcher, &NoopDriver).await.unwrap();
        // Sweep completes, the PR is recorded as failed.
        assert_eq!(run.score.tasks_attempted, 1);
        assert_eq!(run.score.tasks_green, 0);
        assert!(!run.per_pr[0].solved);
    }

    #[test]
    fn shell_quote_handles_simple_strings() {
        assert_eq!(shell_quote("th"), "th");
        assert_eq!(shell_quote("/usr/bin/th"), "/usr/bin/th");
        assert_eq!(shell_quote("smooth-coding"), "smooth-coding");
    }

    #[test]
    fn shell_quote_escapes_spaces_and_quotes() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
        assert!(shell_quote("it's").starts_with('\''));
    }
}
