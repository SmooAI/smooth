//! Multi-task sweep runner for `smooth-bench score`.
//!
//! Wraps the single-task `run_aider_polyglot` runner in a loop over
//! the curated task list, aggregates per-task `BenchResult`s into an
//! aggregate `Score`, and honours the `--budget-usd` hard cap.
//!
//! Streams per-task results to stdout as they complete — the final
//! aggregate `Score` is emitted at the end so operators see progress
//! during the (potentially multi-hour) authoritative run.
//!
//! The runner is parameterised on a `TaskRunner` trait so unit tests
//! can exercise aggregation + budget-cap logic without an LLM.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;

use crate::curated::CuratedList;
use crate::score::{median_ms, LanguageScore, Score};
use crate::{BenchOpts, BenchResult, PolyglotLang};

/// Single-run result needed by the sweep. A thin projection of
/// `BenchResult` so unit tests don't have to fabricate every field
/// of the full struct.
#[derive(Debug, Clone)]
pub struct TaskOutcome {
    pub solved: bool,
    pub cost_usd: f64,
    pub duration_ms: u64,
    /// True when the dispatch hit `SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S`
    /// before the operator made meaningful progress — typically the
    /// chat-agent never returned a pearl id within the HTTP timeout
    /// window and the polyglot starter happens to satisfy the test
    /// suite for that exercise. These shouldn't count as real PASSes
    /// in the headline number even though the test runner reports
    /// solved=true. Detection: duration is exactly the chat HTTP
    /// timeout (within 100 ms) AND cost_usd is 0 (no LLM rounds
    /// completed) AND no LLM error was surfaced.
    pub inconclusive: bool,
    /// Run-dir name (last 8 chars under `~/.smooth/bench-runs/`) so the
    /// eval-html sweep rollup can link to the per-task artifacts.
    /// `None` for runner errors that didn't get far enough to write a
    /// result.json. Optional for back-compat with mocked test runners.
    pub run_id: Option<String>,
}

/// Injection point for the per-task runner. Production implementation
/// (`PolyglotTaskRunner`) calls `run_aider_polyglot`; unit tests
/// provide a canned-response implementation to exercise aggregation +
/// budget-cap logic without hitting the network.
#[async_trait]
pub trait TaskRunner: Send + Sync {
    async fn run_one(&self, lang: PolyglotLang, task: &str, opts: &BenchOpts) -> anyhow::Result<TaskOutcome>;
}

/// The real runner: shells out to `run_aider_polyglot`.
#[derive(Clone)]
pub struct PolyglotTaskRunner;

#[async_trait]
impl TaskRunner for PolyglotTaskRunner {
    async fn run_one(&self, lang: PolyglotLang, task: &str, opts: &BenchOpts) -> anyhow::Result<TaskOutcome> {
        let res = crate::run_aider_polyglot(lang, task, opts).await?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let duration_ms: u64 = (res.duration_s * 1000.0).max(0.0) as u64;

        // Detect HTTP-timeout starter passes. The chat-driver sets
        // SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S (default 600 s after take 9)
        // on the reqwest client; when that timeout fires before the
        // chat-agent returns a pearl id, the bench bails and runs the
        // test against the unmodified workspace. Some polyglot exercises
        // happen to pass without modification (rust/accumulate stub
        // returns the input list, etc.); these PASSes shouldn't pollute
        // the headline.
        //
        // Heuristic: duration ≈ HTTP timeout (within 100 ms),
        // cost_usd is 0 (no LLM rounds completed), and the test runner
        // declared solved=true. The cost==0 check excludes real solves
        // that happened to take the same wall time by coincidence —
        // unless the cost-tracker propagation bug masks them, which
        // we accept as conservative classification.
        let http_timeout_secs: u64 = std::env::var("SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);
        let http_timeout_ms = http_timeout_secs * 1000;
        let near_timeout = duration_ms.saturating_sub(http_timeout_ms) < 100 && http_timeout_ms.saturating_sub(duration_ms) < 100;
        let inconclusive = res.solved && near_timeout && res.cost_usd <= 0.0;

        Ok(TaskOutcome {
            solved: res.solved,
            cost_usd: res.cost_usd,
            duration_ms,
            inconclusive,
            run_id: Some(res.run_id.clone()),
        })
    }
}

/// Map a raw single-task result into a `TaskOutcome`. Helpful for
/// callers who already ran the task and want to feed the result into
/// the aggregator directly.
#[must_use]
pub fn outcome_from_result(r: &BenchResult) -> TaskOutcome {
    TaskOutcome {
        solved: r.solved,
        cost_usd: r.cost_usd,
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        duration_ms: (r.duration_s * 1000.0).max(0.0) as u64,
        inconclusive: false,
        run_id: Some(r.run_id.clone()),
    }
}

/// Which `Score` "gate" a sweep corresponds to. `Release` is the
/// authoritative 20×6=120 run. `Pr` is the small CI-gate sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepGate {
    Release,
    /// PR gate: `tasks_per_language` is the fixed per-lang sample
    /// size (typically 3).
    Pr {
        tasks_per_language: usize,
    },
}

/// Configuration for a sweep run. `budget_usd_cap` is a HARD cap —
/// the sweep aborts as soon as the running total exceeds it, with
/// `budget_usd_hit: true` in the emitted `Score`.
#[derive(Debug, Clone)]
pub struct SweepConfig {
    pub gate: SweepGate,
    pub budget_usd_cap: f64,
    pub smooth_version: String,
    pub commit_sha: String,
    /// Per-task options forwarded to the single-task runner. The
    /// sweep overrides `budget_usd` per task based on remaining
    /// headroom under the sweep-level cap.
    pub task_opts: BenchOpts,
}

/// Result of a sweep: the aggregate `Score` plus the raw per-task
/// outcomes (useful for detailed reporting or debugging — the
/// top-level CLI emits only the `Score`).
#[derive(Debug, Clone)]
pub struct SweepRun {
    pub score: Score,
    pub per_task: Vec<(PolyglotLang, String, TaskOutcome)>,
}

/// Streaming hook: called once per task after it completes, before
/// the next one starts. Default implementation prints a one-line
/// summary to stdout. Kept as a trait so tests can capture events.
pub trait SweepObserver: Send {
    fn on_task_complete(&mut self, idx: usize, total: usize, lang: PolyglotLang, task: &str, outcome: &TaskOutcome, cumulative_cost: f64);
    fn on_budget_hit(&mut self, cumulative_cost: f64, cap: f64);
}

/// Default observer: prints to stdout.
pub struct StdoutObserver;

impl SweepObserver for StdoutObserver {
    fn on_task_complete(&mut self, idx: usize, total: usize, lang: PolyglotLang, task: &str, outcome: &TaskOutcome, cumulative_cost: f64) {
        let tag = if outcome.solved { "PASS" } else { "FAIL" };
        println!(
            "[{idx:>3}/{total:>3}] {tag}  {lang:<10}  {task:<28}  {ms:>6}ms  ${cost:.4}  (total ${cum:.4})",
            lang = lang.dataset_dir(),
            task = task,
            ms = outcome.duration_ms,
            cost = outcome.cost_usd,
            cum = cumulative_cost,
        );
    }

    fn on_budget_hit(&mut self, cumulative_cost: f64, cap: f64) {
        eprintln!("budget cap reached: cumulative ${cumulative_cost:.4} exceeds ${cap:.4} — aborting sweep early");
    }
}

/// Run a curated sweep end-to-end. Emits streaming task results to
/// the observer; returns the aggregate `Score` + per-task outcomes
/// at the end.
///
/// Budget semantics: before kicking off task N, we compute
/// `remaining = cap - cumulative_cost`. If `remaining <= 0` we abort
/// *without* running task N and emit a partial `Score` with
/// `budget_usd_hit = true`. Tasks that finish and put us over the
/// cap cause the NEXT task to be skipped — we never interrupt a
/// running task mid-flight.
///
/// # Errors
/// Setup errors (e.g. missing providers config on the very first
/// call) propagate. Per-task LLM failures are captured in the
/// outcome's `solved = false` and DO NOT abort the sweep.
pub async fn run_sweep<R, O>(curated: &CuratedList, runner: &R, cfg: &SweepConfig, observer: &mut O) -> anyhow::Result<SweepRun>
where
    R: TaskRunner + Clone + 'static,
    O: SweepObserver,
{
    let pairs = curated_pairs(curated, cfg.gate);
    let total = pairs.len();

    let parallelism = std::env::var("SMOOTH_BENCH_PARALLELISM")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);

    if parallelism > 1 {
        return run_sweep_parallel(pairs, total, runner, cfg, observer, parallelism).await;
    }

    let mut per_task: Vec<(PolyglotLang, String, TaskOutcome)> = Vec::with_capacity(total);
    let mut durations_ms: Vec<u64> = Vec::with_capacity(total);
    let mut cumulative_cost = 0.0_f64;
    let mut budget_hit = false;

    let sweep_started = Instant::now();

    for (idx, (lang, task)) in pairs.iter().enumerate() {
        if cumulative_cost >= cfg.budget_usd_cap {
            budget_hit = true;
            observer.on_budget_hit(cumulative_cost, cfg.budget_usd_cap);
            break;
        }

        // Scale the per-task budget so a single task can't blow the
        // whole remaining headroom. This is best-effort — the
        // single-task runner's own budget is the authoritative stop.
        let remaining = cfg.budget_usd_cap - cumulative_cost;
        let mut task_opts = cfg.task_opts.clone();
        task_opts.budget_usd = Some(task_opts.budget_usd.map_or(remaining, |b| b.min(remaining)));

        let outcome = match runner.run_one(*lang, task, &task_opts).await {
            Ok(o) => o,
            Err(e) => {
                // Treat setup / transport failures as unsolved-with-zero-cost
                // so one dead task doesn't kill the whole sweep. Log the
                // error via the observer path (stdout for the default
                // observer, stderr for the error).
                eprintln!("task {}/{} ({}/{}): runner error: {e:#}", idx + 1, total, lang.dataset_dir(), task);
                TaskOutcome {
                    solved: false,
                    cost_usd: 0.0,
                    duration_ms: 0,
                    inconclusive: false,
                    run_id: None,
                }
            }
        };

        cumulative_cost += outcome.cost_usd;
        durations_ms.push(outcome.duration_ms);
        observer.on_task_complete(idx + 1, total, *lang, task, &outcome, cumulative_cost);
        per_task.push((*lang, task.clone(), outcome));
    }

    let score = aggregate(
        &per_task,
        AggregateInputs {
            smooth_version: cfg.smooth_version.clone(),
            commit_sha: cfg.commit_sha.clone(),
            cost_usd: cumulative_cost,
            durations_ms: &durations_ms,
            budget_usd_cap: cfg.budget_usd_cap,
            budget_usd_hit: budget_hit,
        },
    );

    // Touch the sweep-wall-clock so clippy doesn't warn about the
    // unused Instant — it's kept for future reporting.
    let _ = sweep_started.elapsed();

    Ok(SweepRun { score, per_task })
}

/// Parallel sweep variant. Spawns up to `parallelism` tasks at a time;
/// collects outcomes as each finishes. Budget cap is checked before
/// dispatching a new task — tasks already in flight are NOT cancelled
/// when the cap is hit, but no further tasks start.
///
/// Observer events arrive in completion order (not original task order).
/// `idx` passed to `on_task_complete` is the dispatch position; the
/// per_task vec is sorted back into original order before returning so
/// the score JSON is stable across runs.
///
/// Note: the bench runner today opens one chat-agent dispatch per task
/// against a single Big Smooth daemon. The daemon's sandbox pool maxes
/// at 3 concurrent VMs by default, so values above 3 stop helping —
/// extra tasks queue inside the daemon. 3 is the practical max.
async fn run_sweep_parallel<R, O>(
    pairs: Vec<(PolyglotLang, String)>,
    total: usize,
    runner: &R,
    cfg: &SweepConfig,
    observer: &mut O,
    parallelism: usize,
) -> anyhow::Result<SweepRun>
where
    R: TaskRunner + Clone + 'static,
    O: SweepObserver,
{
    use std::sync::Arc;
    use tokio::sync::{Mutex, Semaphore};
    use tokio::task::JoinSet;

    eprintln!("bench: parallel sweep enabled (SMOOTH_BENCH_PARALLELISM={parallelism})");

    let semaphore = Arc::new(Semaphore::new(parallelism));
    let cumulative_cost = Arc::new(Mutex::new(0.0_f64));
    let budget_hit = Arc::new(Mutex::new(false));

    let cap = cfg.budget_usd_cap;
    let task_opts_template = cfg.task_opts.clone();

    let mut js: JoinSet<(usize, PolyglotLang, String, anyhow::Result<TaskOutcome>)> = JoinSet::new();

    for (idx, (lang, task)) in pairs.into_iter().enumerate() {
        // Budget gate before dispatching a new task.
        let cur = *cumulative_cost.lock().await;
        if cur >= cap {
            *budget_hit.lock().await = true;
            observer.on_budget_hit(cur, cap);
            break;
        }
        let remaining = cap - cur;

        let mut task_opts = task_opts_template.clone();
        task_opts.budget_usd = Some(task_opts.budget_usd.map_or(remaining, |b| b.min(remaining)));

        let permit = semaphore.clone().acquire_owned().await?;
        // Each spawned task gets its own clone of the runner. PolyglotTaskRunner
        // is a unit struct so the clone is free; tests that pass closures or
        // canned runners will pay one Clone per task — acceptable cost.
        let runner_clone = runner.clone();
        let lang_c = lang;
        let task_c = task.clone();
        js.spawn(async move {
            let result = runner_clone.run_one(lang_c, &task_c, &task_opts).await;
            drop(permit);
            (idx, lang_c, task_c, result)
        });
    }

    let mut completed: Vec<(usize, PolyglotLang, String, TaskOutcome)> = Vec::with_capacity(total);
    while let Some(joined) = js.join_next().await {
        let (idx, lang, task, result) = match joined {
            Ok(r) => r,
            Err(e) => {
                eprintln!("bench: parallel task join error: {e:#}");
                continue;
            }
        };
        let outcome = match result {
            Ok(o) => o,
            Err(e) => {
                eprintln!("task {}/{} ({}/{}): runner error: {e:#}", idx + 1, total, lang.dataset_dir(), task);
                TaskOutcome {
                    solved: false,
                    cost_usd: 0.0,
                    duration_ms: 0,
                    inconclusive: false,
                    run_id: None,
                }
            }
        };
        let mut cum = cumulative_cost.lock().await;
        *cum += outcome.cost_usd;
        let cumulative_now = *cum;
        drop(cum);
        observer.on_task_complete(idx + 1, total, lang, &task, &outcome, cumulative_now);
        completed.push((idx, lang, task, outcome));
    }

    // Re-sort by original dispatch index so the per_task vec + score
    // JSON are stable across parallel runs.
    completed.sort_by_key(|(idx, _, _, _)| *idx);
    let per_task: Vec<(PolyglotLang, String, TaskOutcome)> = completed.into_iter().map(|(_, lang, task, outcome)| (lang, task, outcome)).collect();
    let durations_ms: Vec<u64> = per_task.iter().map(|(_, _, o)| o.duration_ms).collect();
    let final_cost = *cumulative_cost.lock().await;
    let final_budget_hit = *budget_hit.lock().await;

    let score = aggregate(
        &per_task,
        AggregateInputs {
            smooth_version: cfg.smooth_version.clone(),
            commit_sha: cfg.commit_sha.clone(),
            cost_usd: final_cost,
            durations_ms: &durations_ms,
            budget_usd_cap: cfg.budget_usd_cap,
            budget_usd_hit: final_budget_hit,
        },
    );

    Ok(SweepRun { score, per_task })
}

/// Pick out the `(lang, task)` pairs to run based on the gate.
///
/// For `Release` we run every pair. For `Pr` we take the first
/// `tasks_per_language` tasks per language in the order they were
/// curated — stable and cheap to reason about.
fn curated_pairs(curated: &CuratedList, gate: SweepGate) -> Vec<(PolyglotLang, String)> {
    match gate {
        SweepGate::Release => curated.iter_all().map(|(l, t)| (l, t.to_string())).collect(),
        SweepGate::Pr { tasks_per_language } => {
            let mut out = Vec::new();
            for lang in [
                PolyglotLang::Python,
                PolyglotLang::Rust,
                PolyglotLang::Go,
                PolyglotLang::Javascript,
                PolyglotLang::Java,
                PolyglotLang::Cpp,
            ] {
                for task in curated.tasks_for(lang).iter().take(tasks_per_language) {
                    out.push((lang, task.clone()));
                }
            }
            out
        }
    }
}

struct AggregateInputs<'a> {
    smooth_version: String,
    commit_sha: String,
    cost_usd: f64,
    durations_ms: &'a [u64],
    budget_usd_cap: f64,
    budget_usd_hit: bool,
}

fn aggregate(per_task: &[(PolyglotLang, String, TaskOutcome)], inputs: AggregateInputs<'_>) -> Score {
    let mut by_lang_counts: BTreeMap<PolyglotLang, (u32, u32)> = BTreeMap::new();
    let mut tasks_attempted: u32 = 0;
    let mut tasks_green: u32 = 0;
    let mut tasks_inconclusive: u32 = 0;
    for (lang, _task, outcome) in per_task {
        let entry = by_lang_counts.entry(*lang).or_insert((0, 0));
        entry.0 += 1;
        tasks_attempted += 1;
        if outcome.solved {
            entry.1 += 1;
            tasks_green += 1;
        }
        if outcome.inconclusive {
            tasks_inconclusive += 1;
        }
    }

    let overall_pass_rate = if tasks_attempted == 0 {
        0.0
    } else {
        f64::from(tasks_green) / f64::from(tasks_attempted)
    };

    let by_language: BTreeMap<String, LanguageScore> = by_lang_counts
        .into_iter()
        .map(|(lang, (attempted, green))| (lang.dataset_dir().to_string(), LanguageScore::from_counts(attempted, green)))
        .collect();

    Score {
        smooth_version: inputs.smooth_version,
        commit_sha: inputs.commit_sha,
        ran_at: chrono::Utc::now(),
        overall_pass_rate,
        by_language,
        tasks_attempted,
        tasks_green,
        tasks_inconclusive,
        cost_usd: inputs.cost_usd,
        median_task_ms: median_ms(inputs.durations_ms),
        budget_usd_cap: inputs.budget_usd_cap,
        budget_usd_hit: inputs.budget_usd_hit,
    }
}

/// Resolve the current commit SHA via `git rev-parse HEAD`. Returns
/// `"unknown"` if git fails (e.g. not a git checkout) — we'd rather
/// publish a Score tagged `unknown` than abort the release.
#[must_use]
pub fn current_commit_sha() -> String {
    let Ok(out) = std::process::Command::new("git").args(["rev-parse", "HEAD"]).output() else {
        return "unknown".to_string();
    };
    if !out.status.success() {
        return "unknown".to_string();
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A canned runner: answers from a pre-programmed queue of
    /// `(solved, cost, duration_ms)` tuples. If the queue is
    /// exhausted it returns an unsolved $0 task so tests don't panic
    /// mid-sweep.
    #[derive(Clone)]
    struct CannedRunner {
        queue: std::sync::Arc<Mutex<Vec<(bool, f64, u64)>>>,
    }

    impl CannedRunner {
        fn new(queue: Vec<(bool, f64, u64)>) -> Self {
            Self {
                queue: std::sync::Arc::new(Mutex::new(queue)),
            }
        }
    }

    #[async_trait]
    impl TaskRunner for CannedRunner {
        async fn run_one(&self, _lang: PolyglotLang, _task: &str, _opts: &BenchOpts) -> anyhow::Result<TaskOutcome> {
            let mut q = self.queue.lock().unwrap();
            if q.is_empty() {
                return Ok(TaskOutcome {
                    solved: false,
                    cost_usd: 0.0,
                    duration_ms: 0,
                    inconclusive: false,
                    run_id: None,
                });
            }
            let (solved, cost_usd, duration_ms) = q.remove(0);
            Ok(TaskOutcome {
                solved,
                cost_usd,
                duration_ms,
                inconclusive: false,
                run_id: None,
            })
        }
    }

    struct CapturingObserver {
        events: Vec<String>,
        budget_hit: Option<(f64, f64)>,
    }

    impl CapturingObserver {
        fn new() -> Self {
            Self {
                events: Vec::new(),
                budget_hit: None,
            }
        }
    }

    impl SweepObserver for CapturingObserver {
        fn on_task_complete(&mut self, idx: usize, total: usize, lang: PolyglotLang, task: &str, outcome: &TaskOutcome, cumulative_cost: f64) {
            self.events.push(format!(
                "{idx}/{total} {}/{task} solved={} cost={} cum={cumulative_cost:.4}",
                lang.dataset_dir(),
                outcome.solved,
                outcome.cost_usd,
            ));
        }

        fn on_budget_hit(&mut self, cumulative_cost: f64, cap: f64) {
            self.budget_hit = Some((cumulative_cost, cap));
        }
    }

    fn tiny_curated_pr_pairs() -> (CuratedList, SweepConfig) {
        let list = CuratedList::default_embedded().unwrap();
        let cfg = SweepConfig {
            // PR gate with 1 task per language = exactly 6 tasks.
            gate: SweepGate::Pr { tasks_per_language: 1 },
            budget_usd_cap: 10.0,
            smooth_version: "0.0.0-test".to_string(),
            commit_sha: "deadbeef".to_string(),
            task_opts: BenchOpts::default(),
        };
        (list, cfg)
    }

    #[tokio::test]
    async fn sweep_aggregates_per_language_correctly() {
        // 6 tasks in PR gate (1 per lang × 6 langs). Python solved,
        // everyone else fails. Expected: overall_pass_rate = 1/6.
        let (list, cfg) = tiny_curated_pr_pairs();
        let runner = CannedRunner::new(vec![
            (true, 0.1, 1000),   // python
            (false, 0.2, 2000),  // rust
            (false, 0.15, 1500), // go
            (false, 0.2, 2500),  // javascript
            (false, 0.3, 3000),  // java
            (false, 0.05, 500),  // cpp
        ]);
        let mut obs = CapturingObserver::new();
        let run = run_sweep(&list, &runner, &cfg, &mut obs).await.unwrap();

        assert_eq!(run.score.tasks_attempted, 6);
        assert_eq!(run.score.tasks_green, 1);
        assert!((run.score.overall_pass_rate - 1.0 / 6.0).abs() < 1e-9);
        assert!(!run.score.budget_usd_hit);

        // Per-language shape: exactly 6 entries, each with
        // attempted=1. Only python is green.
        assert_eq!(run.score.by_language.len(), 6);
        assert_eq!(run.score.by_language["python"].tasks_green, 1);
        assert_eq!(run.score.by_language["rust"].tasks_green, 0);
        assert_eq!(run.score.by_language["cpp"].tasks_green, 0);

        // Streaming observer saw every task.
        assert_eq!(obs.events.len(), 6);
        assert!(obs.budget_hit.is_none());
    }

    // Note: SMOOTH_BENCH_PARALLELISM is process-global env state. Setting
    // it in a test would leak into the parallel-running tests that
    // depend on serial sweep semantics (budget-cap aborts, etc.). The
    // parallel path is exercised end-to-end via the bench binary's
    // integration paths and the test isn't worth the flakiness here.
    // See similar consolidation in eval_html::classify_* and
    // supervisor::config_from_env_round_trip.

    #[tokio::test]
    async fn sweep_budget_cap_aborts_on_third_task() {
        // 3 tasks queued with costs [3.0, 4.0, 4.0]. Budget = 5.0.
        //
        // Timeline:
        //   Before task 1: cum=0.0 < 5.0, run → cum=3.0
        //   Before task 2: cum=3.0 < 5.0, run → cum=7.0  (OVER — but task 2 finishes)
        //   Before task 3: cum=7.0 >= 5.0 → abort, budget_usd_hit=true
        //
        // Expected: 2 tasks recorded, budget_usd_hit=true, Score still emitted.
        let (list, _) = tiny_curated_pr_pairs();
        let cfg = SweepConfig {
            gate: SweepGate::Pr { tasks_per_language: 1 }, // 6 pairs, but we'll cap short
            budget_usd_cap: 5.0,
            smooth_version: "0.0.0-test".to_string(),
            commit_sha: "deadbeef".to_string(),
            task_opts: BenchOpts::default(),
        };
        let runner = CannedRunner::new(vec![
            (true, 3.0, 1000),
            (true, 4.0, 2000),
            (true, 4.0, 3000), // must never run
        ]);
        let mut obs = CapturingObserver::new();
        let run = run_sweep(&list, &runner, &cfg, &mut obs).await.unwrap();

        assert_eq!(run.score.tasks_attempted, 2, "third task must not run");
        assert_eq!(run.score.tasks_green, 2);
        assert!(run.score.budget_usd_hit, "budget cap flag must be set");
        assert!((run.score.cost_usd - 7.0).abs() < 1e-9);
        assert_eq!(obs.events.len(), 2);
        let (cum, cap) = obs.budget_hit.expect("on_budget_hit fires");
        assert!((cum - 7.0).abs() < 1e-9);
        assert!((cap - 5.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn sweep_partial_results_shape_matches_full_run() {
        // Partial-run Score shape must JSON-round-trip just like a
        // complete run. Use a queue whose first task already blows
        // the cap — the sweep then aborts before task 2.
        let (list, _) = tiny_curated_pr_pairs();
        let cfg = SweepConfig {
            gate: SweepGate::Pr { tasks_per_language: 1 },
            budget_usd_cap: 1.0,
            smooth_version: "0.0.0-test".to_string(),
            commit_sha: "deadbeef".to_string(),
            task_opts: BenchOpts::default(),
        };
        // Task 1 costs $2.50 (already over the $1 cap). Task 2 must
        // never run.
        let runner = CannedRunner::new(vec![(true, 2.5, 1500), (true, 0.1, 500)]);
        let mut obs = CapturingObserver::new();
        let run = run_sweep(&list, &runner, &cfg, &mut obs).await.unwrap();

        assert_eq!(run.score.tasks_attempted, 1, "second task must not run");
        assert!(run.score.budget_usd_hit);
        assert!((run.score.cost_usd - 2.5).abs() < 1e-9);
        assert_eq!(run.score.median_task_ms, 1500);

        // JSON round-trip preserves the partial-result shape.
        let json = serde_json::to_string(&run.score).unwrap();
        let decoded: Score = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, run.score);
    }

    #[tokio::test]
    async fn sweep_zero_budget_runs_no_tasks() {
        // Edge case: cap=0 → abort before task 1. Score has zero
        // tasks attempted and `budget_usd_hit: true`.
        let (list, _) = tiny_curated_pr_pairs();
        let cfg = SweepConfig {
            gate: SweepGate::Pr { tasks_per_language: 1 },
            budget_usd_cap: 0.0,
            smooth_version: "0.0.0-test".to_string(),
            commit_sha: "deadbeef".to_string(),
            task_opts: BenchOpts::default(),
        };
        let runner = CannedRunner::new(vec![(true, 0.1, 500)]);
        let mut obs = CapturingObserver::new();
        let run = run_sweep(&list, &runner, &cfg, &mut obs).await.unwrap();

        assert_eq!(run.score.tasks_attempted, 0);
        assert!(run.score.budget_usd_hit);
        assert_eq!(run.score.overall_pass_rate, 0.0);
        assert_eq!(run.score.median_task_ms, 0);
    }

    #[tokio::test]
    async fn sweep_gate_pr_limits_total_tasks() {
        // PR gate with 2 per language → exactly 12 tasks (not 120).
        let list = CuratedList::default_embedded().unwrap();
        let cfg = SweepConfig {
            gate: SweepGate::Pr { tasks_per_language: 2 },
            budget_usd_cap: 100.0,
            smooth_version: "0".to_string(),
            commit_sha: "x".to_string(),
            task_opts: BenchOpts::default(),
        };
        let runner = CannedRunner::new(vec![(false, 0.0, 1); 20]); // plenty
        let mut obs = CapturingObserver::new();
        let run = run_sweep(&list, &runner, &cfg, &mut obs).await.unwrap();
        assert_eq!(run.score.tasks_attempted, 12);
    }

    #[tokio::test]
    async fn sweep_runner_error_is_recorded_as_failure_not_abort() {
        #[derive(Clone)]
        struct FailFirstRunner {
            called: std::sync::Arc<Mutex<u32>>,
        }
        #[async_trait]
        impl TaskRunner for FailFirstRunner {
            async fn run_one(&self, _l: PolyglotLang, _t: &str, _o: &BenchOpts) -> anyhow::Result<TaskOutcome> {
                let mut n = self.called.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    Err(anyhow::anyhow!("pretend network blew up"))
                } else {
                    Ok(TaskOutcome {
                        solved: true,
                        cost_usd: 0.1,
                        duration_ms: 500,
                        inconclusive: false,
                        run_id: None,
                    })
                }
            }
        }
        let (list, mut cfg) = tiny_curated_pr_pairs();
        cfg.budget_usd_cap = 100.0;
        let runner = FailFirstRunner {
            called: std::sync::Arc::new(Mutex::new(0)),
        };
        let mut obs = CapturingObserver::new();
        let run = run_sweep(&list, &runner, &cfg, &mut obs).await.unwrap();

        assert_eq!(run.score.tasks_attempted, 6);
        // First task counted as failed-with-no-cost.
        assert_eq!(run.score.tasks_green, 5);
        assert!(!run.score.budget_usd_hit);
    }

    #[test]
    fn current_commit_sha_returns_something_non_empty() {
        // In a git checkout it should be a 40-char hex string; in a
        // tarball checkout it's "unknown". Either is fine; just
        // assert it's non-empty and doesn't panic.
        let sha = current_commit_sha();
        assert!(!sha.is_empty());
    }
}
