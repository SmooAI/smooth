//! `score-real` — multi-axis scoring against curated real-world tasks.
//!
//! Where `aider-polyglot` measures pass/fail on small held-out test
//! suites, `score-real` rewards the harder-to-fake parts of a real
//! engineering loop:
//!
//! - **pass** — did the hidden test suite go green?
//! - **edits** — did the agent's diff stay close to a human's
//!   baseline file count + line count?
//! - **verify** — did the agent self-check enough times (run the
//!   tests, type-check, etc.) before declaring done?
//! - **tools** — did the agent use the right tools (greppable
//!   substrings against the AgentEvent stream)?
//! - **cost** — did it stay under budget?
//!
//! Each axis is normalised to `[0.0, 1.0]` and combined via the
//! `[weights]` table in the task's `grade.toml`.
//!
//! ## Task layout
//!
//! ```text
//! crates/smooth-bench/tasks-real/<id>/
//!   README.md           # task prose, shown to the human driver
//!   workspace/          # starting code, copied into the scratch dir
//!   hidden-tests/       # held-out tests, overlaid after TASK_COMPLETE
//!   grade.toml          # axis weights + verify cmd + baselines
//! ```
//!
//! ## What this module is NOT (yet)
//!
//! Per the plan: this module defines the post-hoc scorer + sweep
//! shape, but the LIVE driver path that actually invokes `th code`
//! and tails AgentEvents is intentionally a TODO — the parent
//! process wires the CLI subcommand. Tests cover the scorer with
//! fixture inputs (AgentEvent streams + work-dir snapshots).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use smooth_operator::agent::AgentEvent;

use crate::grade::{combine_axes, AxisWeights, GradeToml};
use crate::human_driver::DriverPersona;
use crate::score::{median_ms, LanguageScore, Score};

/// Sweep-level config for `score-real`.
#[derive(Debug, Clone)]
pub struct RealConfig {
    /// Run at most N tasks (None = all). Mostly useful for harness
    /// debug runs — the full set is small enough that a `--release`
    /// gate runs everything by default.
    pub task_limit: Option<usize>,
    /// Routing alias (or concrete model id) forwarded to `th code`
    /// when the live driver path eventually wires in. Treated as
    /// opaque metadata by the scorer.
    pub under_test_model: String,
    /// Driver persona — matches the existing `tui_score` knob so the
    /// CLI shape is uniform.
    pub driver_persona: DriverPersona,
    /// Directory containing one subdir per task (each a
    /// `grade.toml`-bearing task layout). Default
    /// `crates/smooth-bench/tasks-real/`.
    pub tasks_dir: PathBuf,
    /// Smooth version this sweep is being attributed to. Forwarded
    /// straight into the embedded `Score`.
    pub smooth_version: String,
    /// Git commit sha at the time of the sweep. Forwarded straight
    /// into the embedded `Score`.
    pub commit_sha: String,
    /// Overall USD budget cap for the sweep. Once cumulative cost
    /// crosses this, the sweep aborts before the next task.
    pub budget_usd_cap: f64,
}

impl Default for RealConfig {
    fn default() -> Self {
        Self {
            task_limit: None,
            under_test_model: "smooth-coding".into(),
            driver_persona: DriverPersona::default(),
            tasks_dir: PathBuf::from("crates/smooth-bench/tasks-real"),
            smooth_version: env!("CARGO_PKG_VERSION").to_string(),
            commit_sha: "unknown".into(),
            budget_usd_cap: 5.0,
        }
    }
}

/// Five per-axis sub-scores, each in `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct AxisScores {
    pub pass: f64,
    pub edits: f64,
    pub verify: f64,
    pub tools: f64,
    pub cost: f64,
}

impl AxisScores {
    /// Per-axis mean across a slice of results. Empty input → all
    /// zeros (consistent with the rest of the harness's "no data →
    /// 0.0, never NaN" convention).
    #[must_use]
    pub fn mean(results: &[RealTaskResult]) -> Self {
        if results.is_empty() {
            return Self {
                pass: 0.0,
                edits: 0.0,
                verify: 0.0,
                tools: 0.0,
                cost: 0.0,
            };
        }
        #[allow(clippy::cast_precision_loss)]
        let n = results.len() as f64;
        let mut acc = (0.0, 0.0, 0.0, 0.0, 0.0);
        for r in results {
            acc.0 += r.axes.pass;
            acc.1 += r.axes.edits;
            acc.2 += r.axes.verify;
            acc.3 += r.axes.tools;
            acc.4 += r.axes.cost;
        }
        Self {
            pass: acc.0 / n,
            edits: acc.1 / n,
            verify: acc.2 / n,
            tools: acc.3 / n,
            cost: acc.4 / n,
        }
    }
}

/// Raw measurements pulled out of a task run — kept alongside the
/// derived axis scores so failures can be debugged without re-running.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawMetrics {
    pub files_edited: u32,
    pub lines_added: u32,
    pub verify_invocations: u32,
    pub tool_pattern_hits: u32,
    pub cost_usd: f64,
    pub hidden_tests_passed: bool,
}

/// Per-task result. The serialised `weighted` is what feeds the
/// aggregate; the per-axis breakdown is preserved for the operator UI
/// and CSV exports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RealTaskResult {
    pub task_id: String,
    pub language: String,
    pub axes: AxisScores,
    pub weighted: f64,
    pub raw: RawMetrics,
}

/// Sweep result. Embeds a standard `Score` so existing tooling (badge
/// renderer, eval-report) can consume it without a parallel schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RealScore {
    pub base: Score,
    pub by_task: Vec<RealTaskResult>,
    pub by_axis_mean: AxisScores,
}

/// Run the full `score-real` sweep.
///
/// Today this is a wiring scaffold — it discovers tasks under
/// `cfg.tasks_dir`, loads each `grade.toml`, and would dispatch the
/// live driver path. The live dispatch is gated behind a TODO; the
/// scorer ([`score_task`]) and aggregator are fully functional and
/// covered by unit tests, so the parent process can wire the CLI
/// against this entrypoint without touching internals later.
///
/// # Errors
/// - `tasks_dir` doesn't exist.
/// - Any task's `grade.toml` fails to parse or validate.
pub async fn run_real_sweep(cfg: &RealConfig) -> Result<RealScore> {
    let tasks = discover_tasks(&cfg.tasks_dir)?;
    let mut to_run: Vec<DiscoveredTask> = tasks;
    if let Some(limit) = cfg.task_limit {
        to_run.truncate(limit);
    }

    // For each discovered task, validate its layout (grade.toml +
    // workspace + hidden-tests). This catches misconfigured tasks
    // *before* spending model budget on them. The live dispatch is
    // a TODO — see module-level docs — so the scaffold loop just
    // validates and returns an empty per-task list.
    for t in &to_run {
        verify_task_layout(&t.dir).with_context(|| format!("task `{}` failed layout check", t.id))?;
    }

    // TODO(score-real-live-dispatch): once the live driver is wired,
    // run each task through `tui_score::run_one_task` (adapted for
    // the real-task workspace overlay + hidden-tests overlay), tail
    // the AgentEvent stream from the session log, and feed
    // `score_task` per-result. For now, return the empty aggregate so
    // the wiring scaffold compiles and the CLI is exercisable in
    // dry-run mode.
    let per_task: Vec<RealTaskResult> = Vec::new();
    let aggregate = aggregate_score(&per_task, cfg, false);

    Ok(RealScore {
        base: aggregate,
        by_axis_mean: AxisScores::mean(&per_task),
        by_task: per_task,
    })
}

/// Result of scanning a tasks directory. One entry per
/// `grade.toml`-bearing subdir.
#[derive(Debug, Clone)]
struct DiscoveredTask {
    id: String,
    dir: PathBuf,
}

fn discover_tasks(tasks_dir: &Path) -> Result<Vec<DiscoveredTask>> {
    if !tasks_dir.exists() {
        return Err(anyhow!("tasks-real dir does not exist: {}", tasks_dir.display()));
    }
    let mut out = Vec::new();
    let entries = std::fs::read_dir(tasks_dir).with_context(|| format!("read tasks dir {}", tasks_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path.join("grade.toml").exists() {
            continue;
        }
        let id = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("bad task dir name: {}", path.display()))?
            .to_string();
        out.push(DiscoveredTask { id, dir: path });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn verify_task_layout(task_dir: &Path) -> Result<()> {
    for required in ["grade.toml", "workspace", "hidden-tests"] {
        let p = task_dir.join(required);
        if !p.exists() {
            return Err(anyhow!("missing `{required}` in task dir {}", task_dir.display()));
        }
    }
    // README is optional but recommended.
    Ok(())
}

/// Score a single task post-hoc, given:
/// - the workspace AFTER the agent ran (so we can diff against the
///   baseline workspace to count files/lines edited),
/// - the `grade.toml` config,
/// - the AgentEvent stream emitted during the run,
/// - the cost in USD reported by the cost sidecar.
///
/// The hidden-tests overlay + test run are the caller's
/// responsibility — by the time `score_task` is called, the caller
/// has already determined whether the hidden tests passed and how
/// many files/lines were edited (passed in via `raw_overrides`).
///
/// This function is pure — given the same inputs it returns the
/// same result. That makes it easy to test exhaustively.
///
/// # Errors
/// None today — this is a pure scoring function. Returns `Result` for
/// future-proofing.
pub fn score_task(grade: &GradeToml, events: &[AgentEvent], cost_usd: f64, raw_overrides: RawMetricInputs) -> Result<RealTaskResult> {
    let tool_pattern_hits = count_tool_pattern_hits(events, &grade.verify.expect_tool_invocations);
    let verify_invocations = count_verify_invocations(events, &grade.verify.expect_tool_invocations);

    let pass_axis = if raw_overrides.hidden_tests_passed { 1.0 } else { 0.0 };
    let edits_axis = edits_score(raw_overrides.files_edited, raw_overrides.lines_added, grade);
    let verify_axis = verify_score(verify_invocations, grade.verify.min_verify_invocations);
    let tools_axis = tools_score(tool_pattern_hits, grade.verify.expect_tool_invocations.len());
    let cost_axis = cost_score(cost_usd, grade.cost.budget_usd);

    let weighted = combine_axes(pass_axis, edits_axis, verify_axis, tools_axis, cost_axis, &grade.weights);

    Ok(RealTaskResult {
        task_id: grade.task.id.clone(),
        language: grade.task.language.clone(),
        axes: AxisScores {
            pass: pass_axis,
            edits: edits_axis,
            verify: verify_axis,
            tools: tools_axis,
            cost: cost_axis,
        },
        weighted,
        raw: RawMetrics {
            files_edited: raw_overrides.files_edited,
            lines_added: raw_overrides.lines_added,
            verify_invocations,
            tool_pattern_hits,
            cost_usd,
            hidden_tests_passed: raw_overrides.hidden_tests_passed,
        },
    })
}

/// Inputs to `score_task` that the LIVE driver path produces post-run:
/// the hidden-tests outcome and the diff stats. Kept in a struct so
/// the function signature stays readable.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawMetricInputs {
    pub hidden_tests_passed: bool,
    pub files_edited: u32,
    pub lines_added: u32,
}

/// Count how many of the configured `expect_tool_invocations` patterns
/// matched ANY tool call in the AgentEvent stream. Each pattern can
/// match at most once toward the cap (so spamming `cargo test` 50
/// times doesn't game the tools axis).
fn count_tool_pattern_hits(events: &[AgentEvent], patterns: &[String]) -> u32 {
    if patterns.is_empty() {
        return 0;
    }
    let mut hits = 0u32;
    for pat in patterns {
        let needle = pat.to_lowercase();
        let mut matched = false;
        for ev in events {
            if event_mentions(ev, &needle) {
                matched = true;
                break;
            }
        }
        if matched {
            hits = hits.saturating_add(1);
        }
    }
    hits
}

/// Count ALL invocations of verify-class tools — every match counts
/// (not deduped). Used for the "min verify invocations" check: a
/// real engineer runs the tests multiple times during debugging.
fn count_verify_invocations(events: &[AgentEvent], patterns: &[String]) -> u32 {
    if patterns.is_empty() {
        return 0;
    }
    let needles: Vec<String> = patterns.iter().map(|p| p.to_lowercase()).collect();
    let mut count = 0u32;
    for ev in events {
        for needle in &needles {
            if event_mentions(ev, needle) {
                count = count.saturating_add(1);
                break; // one verify hit per event, even if it mentions multiple patterns
            }
        }
    }
    count
}

/// Does this event "mention" `needle` (already lowercased)? Looks at
/// tool names + argument payloads for `ToolCallStart`/`Complete`. All
/// other event variants are ignored — they don't represent agent
/// actions.
fn event_mentions(ev: &AgentEvent, needle_lc: &str) -> bool {
    match ev {
        AgentEvent::ToolCallStart { tool_name, arguments, .. } => tool_name.to_lowercase().contains(needle_lc) || arguments.to_lowercase().contains(needle_lc),
        AgentEvent::ToolCallComplete { tool_name, result, .. } => tool_name.to_lowercase().contains(needle_lc) || result.to_lowercase().contains(needle_lc),
        _ => false,
    }
}

/// Edits axis: 1.0 if the agent stayed at or under the human baseline
/// on both files and lines. Linearly subtract `penalty_per_extra_file`
/// per file over baseline and `penalty_per_extra_100_lines` for each
/// 100 lines over baseline (fractional). Clamped to `[0.0, 1.0]`.
fn edits_score(files_edited: u32, lines_added: u32, grade: &GradeToml) -> f64 {
    let baseline_files = grade.task.human_baseline_edits;
    let baseline_lines = grade.task.human_baseline_lines;
    let extra_files = files_edited.saturating_sub(baseline_files);
    let extra_lines = lines_added.saturating_sub(baseline_lines);
    let file_penalty = f64::from(extra_files) * grade.edits.penalty_per_extra_file;
    #[allow(clippy::cast_precision_loss)]
    let line_penalty = (f64::from(extra_lines) / 100.0) * grade.edits.penalty_per_extra_100_lines;
    (1.0 - file_penalty - line_penalty).clamp(0.0, 1.0)
}

/// Verify axis: 1.0 at or above `min_verify_invocations`; linearly
/// decays to 0 at zero invocations. `min == 0` means we don't care
/// about this axis (always full credit).
fn verify_score(invocations: u32, min: u32) -> f64 {
    if min == 0 {
        return 1.0;
    }
    if invocations >= min {
        return 1.0;
    }
    f64::from(invocations) / f64::from(min)
}

/// Tools axis: fraction of `expect_tool_invocations` patterns the
/// agent hit at least once. If no patterns are configured, the axis
/// is full credit (no signal → don't penalize).
fn tools_score(hits: u32, total_patterns: usize) -> f64 {
    if total_patterns == 0 {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let total = total_patterns as f64;
    (f64::from(hits) / total).clamp(0.0, 1.0)
}

/// Cost axis: 1.0 at or under `budget_usd`; linearly decays to 0 at
/// `2 * budget_usd`; 0 beyond.
fn cost_score(cost_usd: f64, budget_usd: f64) -> f64 {
    if budget_usd <= 0.0 {
        // Defensive — validation should have caught this, but don't
        // panic if it slips through.
        return 0.0;
    }
    if cost_usd <= budget_usd {
        return 1.0;
    }
    if cost_usd >= 2.0 * budget_usd {
        return 0.0;
    }
    // Linear decay from 1.0 @ budget to 0.0 @ 2*budget.
    let ratio = (cost_usd - budget_usd) / budget_usd;
    (1.0 - ratio).clamp(0.0, 1.0)
}

fn aggregate_score(per_task: &[RealTaskResult], cfg: &RealConfig, budget_hit: bool) -> Score {
    let mut by_lang_counts: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut tasks_attempted: u32 = 0;
    let mut tasks_green: u32 = 0;
    let mut cost_total = 0.0_f64;
    let mut durations_ms: Vec<u64> = Vec::with_capacity(per_task.len());
    for r in per_task {
        let entry = by_lang_counts.entry(r.language.clone()).or_insert((0, 0));
        entry.0 += 1;
        tasks_attempted += 1;
        // For aggregate "pass" purposes treat the hidden test result
        // as authoritative. The weighted score is preserved per-task
        // in `RealTaskResult`.
        if r.raw.hidden_tests_passed {
            entry.1 += 1;
            tasks_green += 1;
        }
        cost_total += r.raw.cost_usd;
        // Per-task duration isn't tracked in RawMetrics yet (live
        // driver TODO); push 0 so the median doesn't blow up.
        durations_ms.push(0);
    }
    let overall_pass_rate = if tasks_attempted == 0 {
        0.0
    } else {
        f64::from(tasks_green) / f64::from(tasks_attempted)
    };
    let by_language: BTreeMap<String, LanguageScore> = by_lang_counts
        .into_iter()
        .map(|(lang, (att, green))| (lang, LanguageScore::from_counts(att, green)))
        .collect();
    Score {
        smooth_version: cfg.smooth_version.clone(),
        commit_sha: cfg.commit_sha.clone(),
        ran_at: chrono::Utc::now(),
        overall_pass_rate,
        by_language,
        tasks_attempted,
        tasks_green,
        tasks_inconclusive: 0,
        cost_usd: cost_total,
        median_task_ms: median_ms(&durations_ms),
        budget_usd_cap: cfg.budget_usd_cap,
        budget_usd_hit: budget_hit,
    }
}

/// Combine an `AxisWeights` with a set of per-axis scores into the
/// final weighted score. Thin wrapper around `combine_axes` exposed
/// for callers that already have an `AxisScores` in hand.
#[must_use]
pub fn weighted_from_axes(axes: AxisScores, weights: &AxisWeights) -> f64 {
    combine_axes(axes.pass, axes.edits, axes.verify, axes.tools, axes.cost, weights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smooth_operator::agent::AgentEvent;
    use tempfile::TempDir;

    const FIXTURE_GRADE: &str = r#"
[task]
id = "rust-ttl-cache"
language = "rust"
human_baseline_edits = 3
human_baseline_lines = 80

[verify]
test_cmd = "cargo test --quiet"
expect_tool_invocations = ["cargo test", "cargo check"]
min_verify_invocations = 2

[weights]
pass = 0.50
edits = 0.15
verify = 0.15
tools = 0.10
cost = 0.10

[cost]
budget_usd = 0.50

[edits]
penalty_per_extra_file = 0.10
penalty_per_extra_100_lines = 0.05
"#;

    fn fx_grade() -> GradeToml {
        GradeToml::parse(FIXTURE_GRADE).expect("fixture parses")
    }

    /// Tool call event helper.
    fn tool_start(tool: &str, args: &str) -> AgentEvent {
        AgentEvent::ToolCallStart {
            iteration: 1,
            tool_name: tool.into(),
            arguments: args.into(),
        }
    }

    /// Tool complete event helper.
    fn tool_done(tool: &str, result: &str) -> AgentEvent {
        AgentEvent::ToolCallComplete {
            iteration: 1,
            tool_name: tool.into(),
            is_error: false,
            result: result.into(),
            duration_ms: 0,
        }
    }

    #[test]
    fn combine_axes_weighted_correctly() {
        let grade = fx_grade();
        // Hidden tests pass, exactly baseline edits, enough verify
        // calls, all tool patterns hit, half budget spent → all axes
        // 1.0 → weighted should be exactly 1.0.
        let events = vec![
            tool_start("bash", "cargo test --quiet"),
            tool_done("bash", ""),
            tool_start("bash", "cargo check"),
            tool_done("bash", ""),
        ];
        let r = score_task(
            &grade,
            &events,
            0.25,
            RawMetricInputs {
                hidden_tests_passed: true,
                files_edited: 3,
                lines_added: 80,
            },
        )
        .unwrap();
        assert!((r.weighted - 1.0).abs() < 1e-9, "expected weighted=1.0 got {}", r.weighted);
        assert!((r.axes.pass - 1.0).abs() < 1e-9);
        assert!((r.axes.edits - 1.0).abs() < 1e-9);
        assert!((r.axes.verify - 1.0).abs() < 1e-9);
        assert!((r.axes.tools - 1.0).abs() < 1e-9);
        assert!((r.axes.cost - 1.0).abs() < 1e-9);
    }

    #[test]
    fn edits_penalty_clamped_to_zero() {
        let grade = fx_grade();
        // Way past baseline → edits axis must clamp at 0, not go
        // negative. Hidden tests still pass so pass axis is 1.0.
        let events = vec![
            tool_start("bash", "cargo test"),
            tool_done("bash", ""),
            tool_start("bash", "cargo check"),
            tool_done("bash", ""),
        ];
        let r = score_task(
            &grade,
            &events,
            0.10,
            RawMetricInputs {
                hidden_tests_passed: true,
                files_edited: 50,
                lines_added: 10_000,
            },
        )
        .unwrap();
        assert!(r.axes.edits >= 0.0, "edits axis went negative: {}", r.axes.edits);
        assert!((r.axes.edits - 0.0).abs() < 1e-9, "expected 0.0 got {}", r.axes.edits);
        // Other axes intact.
        assert!((r.axes.pass - 1.0).abs() < 1e-9);
        assert!((r.axes.verify - 1.0).abs() < 1e-9);
        assert!((r.axes.tools - 1.0).abs() < 1e-9);
        assert!((r.axes.cost - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cost_axis_linear_decay() {
        // budget is 0.50 in the fixture.
        // <= budget → 1.0
        assert!((cost_score(0.0, 0.5) - 1.0).abs() < 1e-9);
        assert!((cost_score(0.5, 0.5) - 1.0).abs() < 1e-9);
        // At 1.5x budget → halfway between 1 and 0 → 0.5
        assert!((cost_score(0.75, 0.5) - 0.5).abs() < 1e-9, "got {}", cost_score(0.75, 0.5));
        // At 2x budget → 0
        assert!(cost_score(1.0, 0.5).abs() < 1e-9);
        // Beyond 2x → still 0.
        assert!(cost_score(5.0, 0.5).abs() < 1e-9);
    }

    #[test]
    fn verify_axis_counts_grep_hits() {
        let grade = fx_grade();
        // min_verify_invocations = 2 in the fixture. Two cargo-test
        // invocations → full credit.
        let two_verifies = vec![
            tool_start("bash", "cargo test --lib"),
            tool_done("bash", ""),
            tool_start("bash", "cargo test --doc"),
            tool_done("bash", ""),
        ];
        let v2 = count_verify_invocations(&two_verifies, &grade.verify.expect_tool_invocations);
        assert_eq!(v2, 2);
        assert!((verify_score(v2, grade.verify.min_verify_invocations) - 1.0).abs() < 1e-9);

        // One verify call → 0.5.
        let one_verify = vec![tool_start("bash", "cargo test"), tool_done("bash", "")];
        let v1 = count_verify_invocations(&one_verify, &grade.verify.expect_tool_invocations);
        assert_eq!(v1, 1);
        assert!((verify_score(v1, grade.verify.min_verify_invocations) - 0.5).abs() < 1e-9);

        // Zero verify calls → 0.0.
        let zero_verify = vec![tool_start("bash", "ls"), tool_done("bash", "")];
        let v0 = count_verify_invocations(&zero_verify, &grade.verify.expect_tool_invocations);
        assert_eq!(v0, 0);
        assert!(verify_score(v0, grade.verify.min_verify_invocations).abs() < 1e-9);
    }

    #[test]
    fn tools_axis_counts_unique_patterns() {
        let grade = fx_grade();
        // Both expected patterns mentioned at least once → 1.0.
        let both = vec![
            tool_start("bash", "cargo test --quiet"),
            tool_done("bash", ""),
            tool_start("bash", "cargo check"),
            tool_done("bash", ""),
        ];
        let hits = count_tool_pattern_hits(&both, &grade.verify.expect_tool_invocations);
        assert_eq!(hits, 2);

        // Only one of the two patterns → 0.5.
        let one = vec![tool_start("bash", "cargo test --quiet"), tool_done("bash", "")];
        let hits = count_tool_pattern_hits(&one, &grade.verify.expect_tool_invocations);
        assert_eq!(hits, 1);
        assert!((tools_score(hits, grade.verify.expect_tool_invocations.len()) - 0.5).abs() < 1e-9);

        // Spamming the same pattern is not double-counted.
        let spam = vec![
            tool_start("bash", "cargo test"),
            tool_done("bash", ""),
            tool_start("bash", "cargo test"),
            tool_done("bash", ""),
            tool_start("bash", "cargo test"),
            tool_done("bash", ""),
        ];
        let hits = count_tool_pattern_hits(&spam, &grade.verify.expect_tool_invocations);
        assert_eq!(hits, 1, "spam should saturate at one hit per pattern");
    }

    #[test]
    fn load_task_from_fixture_dir() {
        // Build a minimal fake task layout in a tempdir, then
        // discover + verify it.
        let tmp = TempDir::new().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        let task_dir = tasks_dir.join("rust-fake");
        std::fs::create_dir_all(task_dir.join("workspace")).unwrap();
        std::fs::create_dir_all(task_dir.join("hidden-tests")).unwrap();
        std::fs::write(task_dir.join("grade.toml"), FIXTURE_GRADE).unwrap();
        std::fs::write(task_dir.join("README.md"), "# fake\n").unwrap();

        let discovered = discover_tasks(&tasks_dir).unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].id, "rust-fake");
        verify_task_layout(&discovered[0].dir).expect("valid layout");

        let grade = GradeToml::load(&discovered[0].dir.join("grade.toml")).unwrap();
        assert_eq!(grade.task.id, "rust-ttl-cache");
    }

    #[test]
    fn missing_hidden_tests_dir_errors() {
        let tmp = TempDir::new().unwrap();
        let task_dir = tmp.path().join("task");
        std::fs::create_dir_all(task_dir.join("workspace")).unwrap();
        std::fs::write(task_dir.join("grade.toml"), FIXTURE_GRADE).unwrap();
        // No hidden-tests dir.
        let err = verify_task_layout(&task_dir).expect_err("must error");
        assert!(format!("{err:#}").contains("hidden-tests"), "unexpected: {err:#}");
    }

    #[test]
    fn discover_tasks_skips_non_task_dirs() {
        let tmp = TempDir::new().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        // A non-task entry: just a stray dir, no grade.toml.
        std::fs::create_dir_all(tasks_dir.join("notes")).unwrap();
        // A stray file at the top level shouldn't blow up the scan.
        std::fs::write(tasks_dir.join("README.md"), "# stray\n").unwrap();
        // One real task.
        let task = tasks_dir.join("ts-fake");
        std::fs::create_dir_all(task.join("workspace")).unwrap();
        std::fs::create_dir_all(task.join("hidden-tests")).unwrap();
        std::fs::write(task.join("grade.toml"), FIXTURE_GRADE).unwrap();

        let discovered = discover_tasks(&tasks_dir).unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].id, "ts-fake");
    }

    #[test]
    fn discover_tasks_errors_on_missing_dir() {
        let err = discover_tasks(Path::new("/nonexistent/path/should/not/exist")).expect_err("must error");
        assert!(format!("{err:#}").contains("does not exist"));
    }

    #[test]
    fn axis_scores_mean_handles_empty() {
        let m = AxisScores::mean(&[]);
        assert!(m.pass.abs() < 1e-9 && m.edits.abs() < 1e-9);
    }

    #[test]
    fn axis_scores_mean_basic() {
        let mk = |p: f64| RealTaskResult {
            task_id: "x".into(),
            language: "rust".into(),
            axes: AxisScores {
                pass: p,
                edits: p,
                verify: p,
                tools: p,
                cost: p,
            },
            weighted: p,
            raw: RawMetrics {
                files_edited: 0,
                lines_added: 0,
                verify_invocations: 0,
                tool_pattern_hits: 0,
                cost_usd: 0.0,
                hidden_tests_passed: p > 0.5,
            },
        };
        let results = vec![mk(1.0), mk(0.5), mk(0.0)];
        let m = AxisScores::mean(&results);
        assert!((m.pass - 0.5).abs() < 1e-9);
        assert!((m.cost - 0.5).abs() < 1e-9);
    }

    #[test]
    fn weighted_from_axes_matches_combine() {
        let weights = AxisWeights {
            pass: 0.4,
            edits: 0.2,
            verify: 0.2,
            tools: 0.1,
            cost: 0.1,
        };
        let axes = AxisScores {
            pass: 1.0,
            edits: 0.5,
            verify: 1.0,
            tools: 0.5,
            cost: 0.0,
        };
        let w = weighted_from_axes(axes, &weights);
        let expected = 0.4 * 1.0 + 0.2 * 0.5 + 0.2 * 1.0 + 0.1 * 0.5 + 0.1 * 0.0;
        assert!((w - expected).abs() < 1e-9);
    }

    #[test]
    fn run_real_sweep_with_empty_dir_returns_empty_score() {
        // Smoke test the scaffolded entrypoint: pointed at an empty
        // tasks dir, it should return successfully with a zero-tasks
        // Score (no panic, no error).
        let tmp = TempDir::new().unwrap();
        let tasks_dir = tmp.path().join("tasks-real");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        let cfg = RealConfig {
            tasks_dir: tasks_dir.clone(),
            task_limit: None,
            under_test_model: "smooth-coding".into(),
            driver_persona: DriverPersona::default(),
            smooth_version: "0.0.0-test".into(),
            commit_sha: "test".into(),
            budget_usd_cap: 1.0,
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = rt.block_on(run_real_sweep(&cfg)).expect("sweep should not error on empty dir");
        assert_eq!(result.base.tasks_attempted, 0);
        assert_eq!(result.by_task.len(), 0);
    }
}
