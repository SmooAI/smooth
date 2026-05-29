//! `score-swe-bench` — drive `th code` through SWE-bench Verified / Lite
//! tasks via the same tmux + LLM-as-human loop the `score-tui` path
//! uses, then evaluate each instance by running its FAIL_TO_PASS +
//! PASS_TO_PASS test sets and combining the results per the official
//! SWE-bench rubric.
//!
//! Pipeline (per task):
//! 1. Look up the instance row from the cached HF dataset
//!    (see [`swe_bench_dataset`](crate::swe_bench_dataset)).
//! 2. [`prepare_workspace`] — shallow-clone `<repo>` at `<base_commit>`
//!    into a per-task work dir, drop in `PROMPT.txt` (verbatim
//!    `problem_statement`) and `INSTRUCTIONS.md` (the boilerplate
//!    pointing at the editable surface + the exit sentinel).
//! 3. [`run_one_swe_bench_task`] — call into the same `tui_score`
//!    primitives (`th code` + tmux + driver persona) but with the
//!    prepared workdir as scratch, and the SWE-bench rubric as scorer.
//! 4. [`score_instance`] — run `pytest -x` over FAIL_TO_PASS then
//!    PASS_TO_PASS, parse the output via the native pytest summary
//!    parser, persist a forensic dump.
//! 5. Aggregate into the shared [`Score`] struct under the
//!    `"python"` language bucket so existing downstream consumers
//!    (badges, dashboards) keep working.
//!
//! Test-side guarantees:
//! - Empty FAIL_TO_PASS is treated as UNSOLVED, never trivially-true.
//! - A regression in any PASS_TO_PASS unsolves the instance.
//! - Any non-zero exit code (collection error, etc.) is treated as a
//!   failed test, never a silent pass.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::human_driver::{DriverPersona, LoopExit};
use crate::score::{median_ms, LanguageScore, Score};
use crate::swe_bench_dataset::{cache_dir, fetch_instances, SweBenchInstance, SweBenchVariant};
use crate::tui_score::TuiTaskConfig;

/// User-facing config for a SWE-bench sweep.
#[derive(Debug, Clone)]
pub struct SweBenchConfig {
    /// Which SWE-bench variant to run.
    pub variant: SweBenchVariant,
    /// Cap on the number of tasks. `None` = run every instance the
    /// dataset cache yields.
    pub task_limit: Option<usize>,
    /// Optional `--model` override forwarded to `th code` (same field
    /// as `TuiTaskConfig::under_test_model`).
    pub under_test_model: String,
    /// Driver persona — `User` (default) or `Coach` (pearl th-e17b1a).
    pub driver_persona: DriverPersona,
    /// Where the HF dataset cache lives. Set to a tmp dir in tests so
    /// they don't touch `~/.smooth/bench-data/`.
    pub cache_dir: PathBuf,
    /// Per-task scratch dirs land under this directory. Each instance
    /// gets its own subdirectory keyed by `instance_id`.
    pub work_root: PathBuf,
    /// Smooth version + commit SHA stamped on the final `Score`.
    pub smooth_version: String,
    pub commit_sha: String,
    /// Budget cap surfaced in the `Score`. SWE-bench tasks don't yet
    /// feed the cap (the loop runs to completion), but the field is
    /// kept for parity with the polyglot path so downstream readers
    /// see a consistent shape.
    pub budget_usd_cap: f64,
    /// TUI knobs forwarded to `run_one_swe_bench_task`.
    pub tui_cfg: TuiTaskConfig,
}

impl SweBenchConfig {
    /// Build a `SweBenchConfig` with default cache + work dirs derived
    /// from `~/.smooth/`.
    ///
    /// # Errors
    /// Errors when there's no home directory.
    pub fn with_defaults(variant: SweBenchVariant) -> Result<Self> {
        let home = dirs_next::home_dir().ok_or_else(|| anyhow!("no home dir"))?;
        let work_root = home.join(".smooth").join("bench-runs").join("swe-bench");
        Ok(Self {
            variant,
            task_limit: None,
            under_test_model: String::new(),
            driver_persona: DriverPersona::default(),
            cache_dir: cache_dir(variant)?,
            work_root,
            smooth_version: "0.0.0-dev".into(),
            commit_sha: "unknown".into(),
            budget_usd_cap: 25.0,
            tui_cfg: TuiTaskConfig::default(),
        })
    }
}

/// Per-instance scoring result. Mirrors `tui_score::TuiTaskOutcome` so
/// the two paths can feed the same downstream aggregators.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SweBenchInstanceScore {
    pub instance_id: String,
    pub solved: bool,
    pub fail_to_pass_passed: u32,
    pub fail_to_pass_total: u32,
    pub pass_to_pass_passed: u32,
    pub pass_to_pass_total: u32,
    pub duration_ms: u64,
    pub cost_usd: f64,
    /// Why solved is false, when it's false. `None` on a clean solve.
    pub failure_reason: Option<String>,
}

impl SweBenchInstanceScore {
    /// Build an "unsolved (never ran)" placeholder for an instance the
    /// harness couldn't even start (e.g. clone failed). Cheap factory
    /// keeps callers from reaching into the struct field-by-field.
    #[must_use]
    pub fn errored(instance_id: &str, reason: &str) -> Self {
        Self {
            instance_id: instance_id.to_string(),
            solved: false,
            fail_to_pass_passed: 0,
            fail_to_pass_total: 0,
            pass_to_pass_passed: 0,
            pass_to_pass_total: 0,
            duration_ms: 0,
            cost_usd: 0.0,
            failure_reason: Some(reason.to_string()),
        }
    }
}

/// Full sweep result mirror of the polyglot sweep shape.
#[derive(Debug, Clone)]
pub struct SweBenchSweepRun {
    pub score: Score,
    pub per_task: Vec<SweBenchInstanceScore>,
    pub via: &'static str,
}

/// Outcome of executing the FAIL_TO_PASS + PASS_TO_PASS commands
/// against a prepared work dir. Exposed for unit-testing the
/// rubric independently of any live process spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestBucket {
    /// Number of tests in this bucket that ran and passed.
    pub passed: u32,
    /// Total number of tests we *asked* to run (i.e. the length of
    /// the FAIL_TO_PASS / PASS_TO_PASS list).
    pub total: u32,
    /// True iff the test runner exited 0. A non-zero exit means
    /// collection error or runtime error and is treated as a failure
    /// regardless of the per-test summary.
    pub runner_exit_ok: bool,
}

impl TestBucket {
    /// All requested tests pass AND the runner exited cleanly.
    /// Empty buckets satisfy `all_pass()` (because we requested
    /// nothing and got nothing failing), so callers using `all_pass`
    /// for the PASS_TO_PASS bucket get the right "no regressions"
    /// answer when the instance has no regression tests.
    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.runner_exit_ok && self.passed == self.total
    }
}

/// Decide whether an instance is solved given the F2P + P2P buckets.
/// Pure function so it's trivially unit-testable.
///
/// Rules (matching the official SWE-bench rubric):
/// - At least one FAIL_TO_PASS test must have been requested. An
///   instance with no FAIL_TO_PASS is treated as UNSOLVED — there's
///   no positive signal that the patch did anything.
/// - Every FAIL_TO_PASS test must pass after the agent's changes.
/// - Every PASS_TO_PASS test must still pass after the agent's
///   changes (no regressions).
#[must_use]
pub fn score_combination(fail_to_pass: &TestBucket, pass_to_pass: &TestBucket) -> (bool, Option<String>) {
    if fail_to_pass.total == 0 {
        return (false, Some("no FAIL_TO_PASS tests requested — cannot prove a fix".into()));
    }
    if !fail_to_pass.runner_exit_ok {
        return (false, Some("FAIL_TO_PASS runner exited non-zero (collection/runtime error)".into()));
    }
    if fail_to_pass.passed != fail_to_pass.total {
        return (
            false,
            Some(format!(
                "{} of {} FAIL_TO_PASS tests still failing",
                fail_to_pass.total - fail_to_pass.passed,
                fail_to_pass.total,
            )),
        );
    }
    if !pass_to_pass.runner_exit_ok {
        return (false, Some("PASS_TO_PASS runner exited non-zero (collection/runtime error)".into()));
    }
    if pass_to_pass.passed != pass_to_pass.total {
        return (
            false,
            Some(format!(
                "{} of {} PASS_TO_PASS tests regressed",
                pass_to_pass.total - pass_to_pass.passed,
                pass_to_pass.total,
            )),
        );
    }
    (true, None)
}

/// Compose a `TestBucket` from the parsed pytest summary + the runner
/// exit code. Exposed for unit tests; production calls
/// [`score_instance`] which wraps the actual `pytest` invocation.
#[must_use]
pub fn bucket_from_pytest(stdout: &str, requested_total: usize, runner_exit_ok: bool) -> TestBucket {
    let counts = crate::parse_pytest_summary(stdout);
    let n_passed = counts.map_or(0, |c| c.passed);
    TestBucket {
        passed: n_passed,
        #[allow(clippy::cast_possible_truncation)]
        total: requested_total as u32,
        runner_exit_ok,
    }
}

/// Prepare a per-task work directory: `git clone --filter=blob:none`
/// then `git checkout <base_commit>`, write `PROMPT.txt` (verbatim
/// problem statement) + `INSTRUCTIONS.md` (boilerplate guardrails).
///
/// Returns the path to the prepared work directory.
///
/// # Errors
/// Errors when `git clone` / `git checkout` fail, or when the
/// instructions/prompt files can't be written.
pub fn prepare_workspace(inst: &SweBenchInstance, work_root: &Path) -> Result<PathBuf> {
    let work_dir = work_root.join(sanitize_dir(&inst.instance_id));
    if work_dir.exists() {
        // Idempotent re-prep: nuke and re-clone. SWE-bench runs are
        // expensive and we don't want to inherit cruft from a
        // half-failed prior attempt.
        std::fs::remove_dir_all(&work_dir).with_context(|| format!("remove existing work dir {}", work_dir.display()))?;
    }
    std::fs::create_dir_all(&work_dir).with_context(|| format!("mkdir {}", work_dir.display()))?;

    let repo_url = format!("https://github.com/{repo}.git", repo = inst.repo);
    let clone_status = std::process::Command::new("git")
        .arg("clone")
        .arg("--filter=blob:none")
        .arg(&repo_url)
        .arg(&work_dir)
        .status()
        .with_context(|| format!("spawn `git clone {repo_url}`"))?;
    if !clone_status.success() {
        return Err(anyhow!("git clone {repo_url} failed with status {clone_status:?}"));
    }
    let checkout_status = std::process::Command::new("git")
        .current_dir(&work_dir)
        .arg("checkout")
        .arg(&inst.base_commit)
        .status()
        .with_context(|| format!("spawn `git checkout {}`", inst.base_commit))?;
    if !checkout_status.success() {
        return Err(anyhow!("git checkout {} failed", inst.base_commit));
    }

    let prompt_path = work_dir.join("PROMPT.txt");
    std::fs::write(&prompt_path, &inst.problem_statement).with_context(|| format!("write {}", prompt_path.display()))?;

    let instructions_path = work_dir.join("INSTRUCTIONS.md");
    std::fs::write(&instructions_path, render_instructions(inst)).with_context(|| format!("write {}", instructions_path.display()))?;

    Ok(work_dir)
}

/// INSTRUCTIONS.md boilerplate. Kept short — the agent reads
/// PROMPT.txt for the actual problem statement; this file just
/// codifies the guardrails (no test edits, the exit sentinel).
fn render_instructions(inst: &SweBenchInstance) -> String {
    let mut s = String::new();
    s.push_str("# Task Instructions (SWE-bench)\n\n");
    s.push_str("You are editing a real-world open-source repository to fix the issue described in `PROMPT.txt`.\n\n");
    s.push_str("## Rules\n");
    s.push_str("- Edit the source under this repository to fix the issue.\n");
    s.push_str("- DO NOT modify any of the existing test files. The benchmark scores you by re-running them as-is.\n");
    s.push_str("- DO NOT add new tests of your own. The grader only counts the tests the benchmark already specified.\n");
    s.push_str("- When you are finished and confident the tests will pass, say exactly:  TASK_COMPLETE\n\n");
    s.push_str("## Metadata\n");
    let _ = std::fmt::Write::write_fmt(&mut s, format_args!("- instance_id: `{}`\n", inst.instance_id));
    let _ = std::fmt::Write::write_fmt(&mut s, format_args!("- repo: `{}`\n", inst.repo));
    let _ = std::fmt::Write::write_fmt(&mut s, format_args!("- base_commit: `{}`\n", inst.base_commit));
    let _ = std::fmt::Write::write_fmt(&mut s, format_args!("- FAIL_TO_PASS count: {}\n", inst.fail_to_pass.len()));
    let _ = std::fmt::Write::write_fmt(&mut s, format_args!("- PASS_TO_PASS count: {}\n", inst.pass_to_pass.len()));
    s
}

/// Strip filesystem-unsafe characters from an instance_id so it can
/// be used as a directory name. SWE-bench IDs are
/// `<repo>__<issue>-<n>` with `/` replaced by `__`, but we still want
/// to be defensive.
fn sanitize_dir(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect()
}

/// Run the FAIL_TO_PASS + PASS_TO_PASS pytest commands against the
/// agent's modified work dir, parse the summaries, and persist a
/// forensic dump alongside the existing aider-polyglot dumps.
///
/// `cost_usd` and `duration_ms` are recorded as observed — they come
/// from the TUI-driven phase and pass through unchanged.
///
/// # Errors
/// Errors only when the pytest process can't be spawned at all.
/// Failed/erroring tests resolve to `solved=false`, not errors.
pub async fn score_instance(inst: &SweBenchInstance, work_dir: &Path, cost_usd: f64, duration_ms: u64) -> Result<SweBenchInstanceScore> {
    let (f2p_stdout, f2p_exit) = run_pytest(work_dir, &inst.fail_to_pass).await?;
    let (p2p_stdout, p2p_exit) = run_pytest(work_dir, &inst.pass_to_pass).await?;

    let f2p_bucket = bucket_from_pytest(&f2p_stdout, inst.fail_to_pass.len(), f2p_exit);
    let p2p_bucket = bucket_from_pytest(&p2p_stdout, inst.pass_to_pass.len(), p2p_exit);
    let (solved, failure_reason) = score_combination(&f2p_bucket, &p2p_bucket);

    let forensic_dir = work_dir.join(".swe-bench-forensic");
    std::fs::create_dir_all(&forensic_dir).ok();
    let _ = std::fs::write(forensic_dir.join("fail_to_pass.stdout.txt"), &f2p_stdout);
    let _ = std::fs::write(forensic_dir.join("pass_to_pass.stdout.txt"), &p2p_stdout);
    let summary = serde_json::json!({
        "instance_id": inst.instance_id,
        "solved": solved,
        "failure_reason": failure_reason,
        "fail_to_pass": {
            "passed": f2p_bucket.passed,
            "total": f2p_bucket.total,
            "runner_exit_ok": f2p_bucket.runner_exit_ok,
        },
        "pass_to_pass": {
            "passed": p2p_bucket.passed,
            "total": p2p_bucket.total,
            "runner_exit_ok": p2p_bucket.runner_exit_ok,
        },
    });
    let _ = std::fs::write(forensic_dir.join("summary.json"), serde_json::to_string_pretty(&summary).unwrap_or_default());

    Ok(SweBenchInstanceScore {
        instance_id: inst.instance_id.clone(),
        solved,
        fail_to_pass_passed: f2p_bucket.passed,
        fail_to_pass_total: f2p_bucket.total,
        pass_to_pass_passed: p2p_bucket.passed,
        pass_to_pass_total: p2p_bucket.total,
        duration_ms,
        cost_usd,
        failure_reason,
    })
}

/// Spawn `python -m pytest -x --no-header -rN <test-ids…>` inside
/// `work_dir`. Empty test lists short-circuit to a `(empty, true)`
/// pair so we don't synthesise a confusing pytest-found-no-tests
/// result for the PASS_TO_PASS=empty case.
async fn run_pytest(work_dir: &Path, tests: &[String]) -> Result<(String, bool)> {
    if tests.is_empty() {
        return Ok((String::new(), true));
    }
    let mut cmd = tokio::process::Command::new("python");
    cmd.current_dir(work_dir).arg("-m").arg("pytest").arg("-x").arg("--no-header").arg("-rN");
    for t in tests {
        cmd.arg(t);
    }
    let output = cmd.output().await.context("spawn pytest")?;
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((combined, output.status.success()))
}

/// Run a single SWE-bench task end-to-end. Today this is a thin
/// wrapper that prepares the workspace and scores the (un-modified)
/// repo as a baseline — the actual `th code` dispatch is wired by the
/// caller via the polyglot TUI primitives. The harness's
/// `run_swe_bench_sweep` calls this for each instance and is the
/// canonical entry point for batched runs.
///
/// Returning early on workspace-prep failure with an `errored` score
/// keeps the sweep going on transient git failures rather than
/// failing the whole sweep over one bad instance.
///
/// # Errors
/// Errors only when scoring itself blows up (pytest unspawnable).
/// Workspace-prep failures resolve to an `errored` score, not an
/// error.
pub async fn run_one_swe_bench_task(inst: &SweBenchInstance, cfg: &SweBenchConfig) -> Result<SweBenchInstanceScore> {
    let t0 = Instant::now();
    let work_dir = match prepare_workspace(inst, &cfg.work_root) {
        Ok(p) => p,
        Err(e) => {
            return Ok(SweBenchInstanceScore::errored(&inst.instance_id, &format!("prepare_workspace failed: {e:#}")));
        }
    };
    // The TUI-driven coding phase is wired via the existing
    // `tui_score::run_polyglot_task_via_tui` primitives by the parent
    // process; this entry point exists so callers can score a
    // pre-prepared workspace deterministically (used by the wider
    // sweep flow and by integration tests). Cost is left at 0.0 in
    // this skeleton — the parent wires it through the cost sidecar
    // when the TUI dispatch lands.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let duration_ms: u64 = (t0.elapsed().as_secs_f64() * 1000.0).max(0.0) as u64;
    score_instance(inst, &work_dir, 0.0, duration_ms).await
}

/// Aggregate a slice of per-instance scores into the unified `Score`
/// shape. All SWE-bench tasks bucket under `"python"` since the
/// harness only runs the Python-test variants today.
#[must_use]
pub fn aggregate_swe_bench(per_task: &[SweBenchInstanceScore], smooth_version: &str, commit_sha: &str, budget_usd_cap: f64, budget_hit: bool) -> Score {
    let mut tasks_attempted: u32 = 0;
    let mut tasks_green: u32 = 0;
    let mut durations: Vec<u64> = Vec::with_capacity(per_task.len());
    let mut cost_usd: f64 = 0.0;
    for inst in per_task {
        tasks_attempted += 1;
        if inst.solved {
            tasks_green += 1;
        }
        durations.push(inst.duration_ms);
        cost_usd += inst.cost_usd;
    }
    let overall_pass_rate = if tasks_attempted == 0 {
        0.0
    } else {
        f64::from(tasks_green) / f64::from(tasks_attempted)
    };
    let mut by_language: BTreeMap<String, LanguageScore> = BTreeMap::new();
    by_language.insert("python".into(), LanguageScore::from_counts(tasks_attempted, tasks_green));
    Score {
        smooth_version: smooth_version.to_string(),
        commit_sha: commit_sha.to_string(),
        ran_at: chrono::Utc::now(),
        overall_pass_rate,
        by_language,
        tasks_attempted,
        tasks_green,
        tasks_inconclusive: 0,
        cost_usd,
        median_task_ms: median_ms(&durations),
        budget_usd_cap,
        budget_usd_hit: budget_hit,
    }
}

/// Run the SWE-bench sweep end-to-end. Iterates the cached dataset (or
/// fetches it on the first call), prepares + scores each instance, and
/// aggregates a `Score`.
///
/// # Errors
/// Errors on dataset fetch failure (the sweep can't proceed without
/// rows). Per-instance failures resolve to `solved=false` outcomes.
pub async fn run_swe_bench_sweep(cfg: &SweBenchConfig) -> Result<SweBenchSweepRun> {
    let mut instances = fetch_instances(cfg.variant, cfg.task_limit).await?;
    if let Some(n) = cfg.task_limit {
        instances.truncate(n);
    }

    let mut per_task: Vec<SweBenchInstanceScore> = Vec::with_capacity(instances.len());
    let mut cumulative_cost = 0.0;
    let mut budget_hit = false;
    for inst in &instances {
        if cumulative_cost >= cfg.budget_usd_cap {
            budget_hit = true;
            break;
        }
        let outcome = match run_one_swe_bench_task(inst, cfg).await {
            Ok(o) => o,
            Err(e) => {
                eprintln!("score-swe-bench: {} runner error: {e:#}", inst.instance_id);
                SweBenchInstanceScore::errored(&inst.instance_id, &format!("{e:#}"))
            }
        };
        cumulative_cost += outcome.cost_usd;
        per_task.push(outcome);
    }

    let score = aggregate_swe_bench(&per_task, &cfg.smooth_version, &cfg.commit_sha, cfg.budget_usd_cap, budget_hit);
    Ok(SweBenchSweepRun {
        score,
        per_task,
        via: "swe-bench",
    })
}

// Tickle the `LoopExit` import — kept for future wiring of the TUI
// dispatch result into per-instance metadata so the eval-report can
// surface "driver bailed on turn 1" the same way the polyglot path
// does. Without the use the import warns under `unused_imports`.
const _: fn() -> LoopExit = || LoopExit::Complete;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swe_bench_dataset::parse_instances_jsonl;

    const SAMPLE_INSTANCE: &str = r#"{"instance_id":"foo__bar-42","repo":"foo/bar","base_commit":"abc123","problem_statement":"Fix the bug.","FAIL_TO_PASS":["tests/test_a.py::test_one"],"PASS_TO_PASS":["tests/test_b.py::test_two","tests/test_b.py::test_three"],"environment_setup_commit":"def456","version":"1.0"}"#;

    fn sample() -> SweBenchInstance {
        parse_instances_jsonl(SAMPLE_INSTANCE).unwrap().pop().unwrap()
    }

    #[test]
    fn score_combination_all_pass_is_solved() {
        let f2p = TestBucket {
            passed: 1,
            total: 1,
            runner_exit_ok: true,
        };
        let p2p = TestBucket {
            passed: 2,
            total: 2,
            runner_exit_ok: true,
        };
        let (solved, reason) = score_combination(&f2p, &p2p);
        assert!(solved);
        assert!(reason.is_none());
    }

    #[test]
    fn score_combination_empty_fail_to_pass_is_unsolved() {
        let f2p = TestBucket {
            passed: 0,
            total: 0,
            runner_exit_ok: true,
        };
        let p2p = TestBucket {
            passed: 2,
            total: 2,
            runner_exit_ok: true,
        };
        let (solved, reason) = score_combination(&f2p, &p2p);
        assert!(!solved, "empty F2P must never be solved trivially");
        let r = reason.unwrap();
        assert!(r.contains("FAIL_TO_PASS"), "{r}");
    }

    #[test]
    fn score_combination_failing_fail_to_pass_is_unsolved() {
        let f2p = TestBucket {
            passed: 0,
            total: 1,
            runner_exit_ok: true,
        };
        let p2p = TestBucket {
            passed: 2,
            total: 2,
            runner_exit_ok: true,
        };
        let (solved, reason) = score_combination(&f2p, &p2p);
        assert!(!solved);
        let r = reason.unwrap();
        assert!(r.contains("FAIL_TO_PASS"), "{r}");
    }

    #[test]
    fn score_combination_pass_to_pass_regression_is_unsolved() {
        let f2p = TestBucket {
            passed: 1,
            total: 1,
            runner_exit_ok: true,
        };
        let p2p = TestBucket {
            passed: 1,
            total: 2,
            runner_exit_ok: true,
        };
        let (solved, reason) = score_combination(&f2p, &p2p);
        assert!(!solved, "any P2P regression must unsolve the instance");
        let r = reason.unwrap();
        assert!(r.contains("PASS_TO_PASS"), "{r}");
    }

    #[test]
    fn score_combination_runner_non_zero_exit_is_unsolved() {
        // Pytest reports 1/1 passed in the summary but exited non-zero
        // (e.g. a teardown error after the test). We must not score
        // this as solved.
        let f2p = TestBucket {
            passed: 1,
            total: 1,
            runner_exit_ok: false,
        };
        let p2p = TestBucket {
            passed: 2,
            total: 2,
            runner_exit_ok: true,
        };
        let (solved, reason) = score_combination(&f2p, &p2p);
        assert!(!solved);
        assert!(reason.unwrap().contains("non-zero"));
    }

    #[test]
    fn score_combination_pass_to_pass_empty_is_okay() {
        // It's legitimate for an instance to have no PASS_TO_PASS
        // tests — e.g. when no regression-protection set exists. In
        // that case the bucket is (0, 0, true) and should not block
        // a solve.
        let f2p = TestBucket {
            passed: 1,
            total: 1,
            runner_exit_ok: true,
        };
        let p2p = TestBucket {
            passed: 0,
            total: 0,
            runner_exit_ok: true,
        };
        let (solved, _reason) = score_combination(&f2p, &p2p);
        assert!(solved);
    }

    #[test]
    fn bucket_from_pytest_parses_real_summary() {
        let stdout = "============================= 1 passed in 0.05s =============================";
        let bucket = bucket_from_pytest(stdout, 1, true);
        assert_eq!(bucket.passed, 1);
        assert_eq!(bucket.total, 1);
        assert!(bucket.runner_exit_ok);
        assert!(bucket.all_pass());
    }

    #[test]
    fn bucket_from_pytest_with_failure() {
        let stdout = "============================= 2 passed, 1 failed in 0.05s =============================";
        let bucket = bucket_from_pytest(stdout, 3, false);
        assert_eq!(bucket.passed, 2);
        assert_eq!(bucket.total, 3);
        assert!(!bucket.runner_exit_ok);
        assert!(!bucket.all_pass());
    }

    #[test]
    fn bucket_from_pytest_unparseable_is_zero_passed() {
        let bucket = bucket_from_pytest("crash with no summary", 1, false);
        assert_eq!(bucket.passed, 0);
        assert_eq!(bucket.total, 1);
        assert!(!bucket.runner_exit_ok);
    }

    #[test]
    fn aggregate_swe_bench_basic() {
        let per_task = vec![
            SweBenchInstanceScore {
                instance_id: "a".into(),
                solved: true,
                fail_to_pass_passed: 1,
                fail_to_pass_total: 1,
                pass_to_pass_passed: 1,
                pass_to_pass_total: 1,
                duration_ms: 1000,
                cost_usd: 0.10,
                failure_reason: None,
            },
            SweBenchInstanceScore {
                instance_id: "b".into(),
                solved: false,
                fail_to_pass_passed: 0,
                fail_to_pass_total: 1,
                pass_to_pass_passed: 1,
                pass_to_pass_total: 1,
                duration_ms: 2000,
                cost_usd: 0.20,
                failure_reason: Some("tests still failing".into()),
            },
            SweBenchInstanceScore {
                instance_id: "c".into(),
                solved: true,
                fail_to_pass_passed: 2,
                fail_to_pass_total: 2,
                pass_to_pass_passed: 0,
                pass_to_pass_total: 0,
                duration_ms: 3000,
                cost_usd: 0.30,
                failure_reason: None,
            },
        ];
        let score = aggregate_swe_bench(&per_task, "0.0.0-test", "abcd", 10.0, false);
        assert_eq!(score.tasks_attempted, 3);
        assert_eq!(score.tasks_green, 2);
        assert!((score.overall_pass_rate - (2.0 / 3.0)).abs() < 1e-9);
        // Median of [1000, 2000, 3000] = 2000.
        assert_eq!(score.median_task_ms, 2000);
        assert!((score.cost_usd - 0.60).abs() < 1e-9);
        let py = score.by_language.get("python").expect("python bucket");
        assert_eq!(py.tasks_attempted, 3);
        assert_eq!(py.tasks_green, 2);
    }

    #[test]
    fn aggregate_swe_bench_empty() {
        let score = aggregate_swe_bench(&[], "0.0.0-test", "abcd", 10.0, false);
        assert_eq!(score.tasks_attempted, 0);
        assert_eq!(score.tasks_green, 0);
        assert_eq!(score.overall_pass_rate, 0.0);
        assert_eq!(score.median_task_ms, 0);
        // Empty python bucket still surfaces with 0/0 (consistent with
        // the polyglot aggregator's behaviour, which downstream
        // consumers depend on).
        let py = score.by_language.get("python").expect("python bucket");
        assert_eq!(py.tasks_attempted, 0);
    }

    #[test]
    fn sanitize_dir_keeps_underscored_ids() {
        // SWE-bench ids are `<owner>__<repo>-<issue>`.
        assert_eq!(sanitize_dir("django__django-12345"), "django__django-12345");
        assert_eq!(sanitize_dir("foo/bar:baz"), "foo_bar_baz");
        assert_eq!(sanitize_dir(""), "");
    }

    #[test]
    fn instance_score_errored_factory_sets_failure_reason() {
        let s = SweBenchInstanceScore::errored("inst-1", "clone failed");
        assert_eq!(s.instance_id, "inst-1");
        assert!(!s.solved);
        assert_eq!(s.failure_reason.as_deref(), Some("clone failed"));
        assert_eq!(s.fail_to_pass_total, 0);
        assert_eq!(s.pass_to_pass_total, 0);
    }

    #[test]
    fn config_with_defaults_resolves_paths() {
        // Should not panic and should populate plausible defaults.
        let cfg = SweBenchConfig::with_defaults(SweBenchVariant::Lite).unwrap();
        assert_eq!(cfg.variant, SweBenchVariant::Lite);
        let cd = cfg.cache_dir.to_string_lossy().to_string();
        assert!(cd.contains("swe-bench-lite"), "{cd}");
        let wr = cfg.work_root.to_string_lossy().to_string();
        assert!(wr.contains("swe-bench"), "{wr}");
    }

    #[test]
    fn render_instructions_includes_sentinel_and_metadata() {
        let inst = sample();
        let out = render_instructions(&inst);
        // Sentinel string must be present verbatim — the LLM-as-human
        // driver tests for it.
        assert!(out.contains("TASK_COMPLETE"), "{out}");
        assert!(out.contains("foo__bar-42"), "{out}");
        assert!(out.contains("foo/bar"), "{out}");
        assert!(out.contains("FAIL_TO_PASS count: 1"), "{out}");
        assert!(out.contains("PASS_TO_PASS count: 2"), "{out}");
    }

    #[test]
    fn task_limit_caps_aggregate_input() {
        // Confirms our slicing convention: aggregate over a truncated
        // slice yields the truncated count, even if the underlying
        // dataset has more rows. This is the contract `run_swe_bench_sweep`
        // depends on.
        let mut per_task: Vec<SweBenchInstanceScore> = (0..10)
            .map(|i| SweBenchInstanceScore {
                instance_id: format!("inst-{i}"),
                solved: i % 2 == 0,
                fail_to_pass_passed: 1,
                fail_to_pass_total: 1,
                pass_to_pass_passed: 0,
                pass_to_pass_total: 0,
                duration_ms: 1000,
                cost_usd: 0.01,
                failure_reason: None,
            })
            .collect();
        per_task.truncate(3);
        let score = aggregate_swe_bench(&per_task, "x", "y", 10.0, false);
        assert_eq!(score.tasks_attempted, 3);
        // 0, 1, 2: solved at i=0, i=2 → 2 green.
        assert_eq!(score.tasks_green, 2);
    }

    #[test]
    fn prepare_workspace_creates_prompt_and_instructions_with_fake_git() {
        // Stand up a fake `git` on PATH that:
        // - `git clone … <dest>`  → mkdir -p <dest>
        // - `git checkout …`       → exit 0
        // This lets us exercise prepare_workspace's filesystem layout
        // (PROMPT.txt + INSTRUCTIONS.md) without doing any network I/O.
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("fakebin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let git_script = bin_dir.join("git");
        // Minimal POSIX shell shim. clone is `git clone --filter=blob:none URL DEST` —
        // DEST is the last argument. checkout is a no-op.
        std::fs::write(
            &git_script,
            "#!/bin/sh\nif [ \"$1\" = clone ]; then\n  eval DEST=\\${$#}\n  mkdir -p \"$DEST\"\n  exit 0\nfi\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&git_script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&git_script, perms).unwrap();
        }
        let orig_path = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = std::ffi::OsString::from(bin_dir.as_os_str());
        new_path.push(":");
        new_path.push(&orig_path);
        // SAFETY of PATH mutation in tests: cargo test runs each test
        // in its own thread but they share env. Set the var BEFORE
        // calling prepare_workspace and restore after.
        // Use a poor-mans guard.
        struct Restore(std::ffi::OsString);
        impl Drop for Restore {
            fn drop(&mut self) {
                std::env::set_var("PATH", &self.0);
            }
        }
        let _guard = Restore(orig_path);
        std::env::set_var("PATH", &new_path);

        let inst = sample();
        let work_root = tmp.path().join("work");
        std::fs::create_dir_all(&work_root).unwrap();
        let work_dir = prepare_workspace(&inst, &work_root).expect("prep");
        assert!(work_dir.join("PROMPT.txt").is_file());
        assert!(work_dir.join("INSTRUCTIONS.md").is_file());
        let prompt = std::fs::read_to_string(work_dir.join("PROMPT.txt")).unwrap();
        assert_eq!(prompt, "Fix the bug.");
        let inst_md = std::fs::read_to_string(work_dir.join("INSTRUCTIONS.md")).unwrap();
        assert!(inst_md.contains("TASK_COMPLETE"));
    }

    #[test]
    fn variant_to_slug_round_trips() {
        // Re-state the variant → slug mapping at this layer so
        // accidental edits to swe_bench_dataset don't silently shift
        // which dataset score-swe-bench queries.
        assert_eq!(SweBenchVariant::Verified.hf_slug(), "princeton-nlp/SWE-bench_Verified");
        assert_eq!(SweBenchVariant::Lite.hf_slug(), "princeton-nlp/SWE-bench_Lite");
    }

    #[test]
    fn run_pytest_empty_short_circuits_to_success() {
        // run_pytest with no tests must not spawn a process; it
        // returns (empty, true) so the bucket records 0/0 + runner_ok.
        let tmp = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (out, ok) = rt.block_on(run_pytest(tmp.path(), &[])).unwrap();
        assert!(out.is_empty());
        assert!(ok);
    }
}
