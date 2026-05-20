//! `score-tui` — drive `th code` (the actual user-facing TUI)
//! through aider-polyglot tasks via tmux + an LLM-as-human loop, then
//! emit a `Score` of the same shape as `score --pr` / `score --release`.
//!
//! This exercises the surface a real user touches:
//! - The TUI's prompt + input handling.
//! - The model-alias → upstream display.
//! - Tool-call rendering in the pane.
//! - Session lifecycle on a real `th` binary.
//!
//! …rather than the WebSocket path the existing `score` command uses
//! (which bypasses the TUI entirely and dispatches directly into Big
//! Smooth's chat-agent).
//!
//! See the module-level doc on [`run_polyglot_task_via_tui`] for the
//! per-task flow.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::curated::CuratedList;
use crate::human_driver::{run_human_loop, DriverModel, LoopConfig, LoopExit};
use crate::score::{median_ms, LanguageScore, Score};
use crate::sweep::{SweepGate, SweepObserver, TaskOutcome};
use crate::tmux_driver::TmuxDriver;
use crate::{finalize_and_score, prepare_task, BenchOpts, PolyglotLang};

/// Per-task config for the TUI path.
#[derive(Debug, Clone)]
pub struct TuiTaskConfig {
    /// Path to the `th` binary to spawn. Defaults to "th" (assumes
    /// it's on PATH). Pearl operators on a dev machine may want to
    /// point at a release-build binary in the worktree.
    pub th_binary: String,
    /// tmux session name. Each task uses `{prefix}-{lang}-{task}` so
    /// concurrent runs don't collide.
    pub tmux_session_prefix: String,
    /// How long to wait for `th code` to render its first frame.
    pub boot_timeout: Duration,
    /// LLM-as-human loop knobs.
    pub loop_cfg: LoopConfig,
    /// Outer per-task wall-clock cap. Independent of the per-turn
    /// idle timeout — bounds total time spent on a task.
    pub task_timeout: Duration,
    /// When `true`, write a per-task pane-debug log to the run dir
    /// at `<run_dir>/<lang>-<task>.pane.log`. Each `send` and each
    /// `wait_for_idle` boundary appends a timestamped record so a
    /// failed bench can be inspected post-hoc.
    pub debug_pane_log: bool,
    /// When `true`, a TUI task that exits with `Stuck` / `TurnCap` /
    /// `IdleTimeout` on turn 1 (i.e. driver bailed before any real
    /// interaction) is forced to `solved=false` regardless of the
    /// raw test result. This stops the harness from reporting a
    /// passing score on a workspace the agent never touched — see
    /// pearl th-f46efa.
    pub stuck_means_failed: bool,
}

impl Default for TuiTaskConfig {
    fn default() -> Self {
        Self {
            th_binary: "th".into(),
            tmux_session_prefix: "smooth-bench-tui".into(),
            // `th code` boots an entire microVM cast (wonk, goalie,
            // narc, scribe, archivist, diver, groove) plus the
            // operator-runner pool before reaching the input prompt.
            // Empirically this takes 30-60s on a warm machine; 15s
            // (the old default) was way under, which is what made the
            // first-render gate fire prematurely on every PR run. 120s
            // gives generous headroom for a cold sandbox image pull.
            boot_timeout: Duration::from_secs(120),
            loop_cfg: LoopConfig::default(),
            task_timeout: Duration::from_secs(900),
            debug_pane_log: false,
            stuck_means_failed: true,
        }
    }
}

/// Outcome of a single TUI-driven task. Mirrors `TaskOutcome` but
/// captures the extra signal score-tui needs for downstream analysis
/// (turn count, loop-exit reason, tool-call count, final pane).
#[derive(Debug, Clone)]
pub struct TuiTaskOutcome {
    pub solved: bool,
    pub cost_usd: f64,
    pub duration_ms: u64,
    pub turns: usize,
    pub tool_calls: usize,
    pub exit: LoopExit,
}

impl TuiTaskOutcome {
    /// Lossy down-conversion to the existing `TaskOutcome` so the
    /// score-tui path can feed into the same `Score` aggregator the
    /// WebSocket path uses.
    #[must_use]
    pub fn into_task_outcome(self) -> TaskOutcome {
        TaskOutcome {
            solved: self.solved,
            cost_usd: self.cost_usd,
            duration_ms: self.duration_ms,
            // The TUI path does not have a separate "inconclusive"
            // detector — the LLM-as-human loop either drives the
            // task to a sentinel/turn-cap or the test result is what
            // it is.
            inconclusive: false,
        }
    }
}

/// Run one aider-polyglot task through the TUI. Sets up the scratch
/// dir, spawns `th code` in tmux, drives the human loop, then runs
/// the task's tests and returns the outcome.
///
/// Flow:
///   1. `prepare_task` — clone dataset, copy task files, build prompt.
///   2. Spawn `th code` in tmux pointed at the scratch dir.
///   3. Send `PROMPT.txt` as the first user turn.
///   4. Run the LLM-as-human loop until sentinel/turn cap/idle timeout.
///   5. `finalize_and_score` — strip agent-added test files, run tests.
///
/// Cost is reported as 0.0 from the TUI surface — this is a TODO
/// because the TUI doesn't expose the underlying coding model's spend
/// in a way the harness can scrape today. Pearl follow-up to wire
/// `[METRICS]`-style hooks into `th code`'s output for the score-tui
/// path; see field comment on `TuiTaskOutcome::cost_usd`.
///
/// # Errors
/// Tmux + driver-LLM errors propagate. Test-side errors (failing
/// tests) are reflected in `solved=false`, not errors.
pub async fn run_polyglot_task_via_tui<D: DriverModel>(lang: PolyglotLang, task: &str, model: &D, cfg: &TuiTaskConfig) -> Result<TuiTaskOutcome> {
    let setup = prepare_task(lang, task).context("prepare polyglot task")?;
    let t0 = Instant::now();

    let session = format!("{}-{}-{}", cfg.tmux_session_prefix, lang.dataset_dir(), task);
    let shell_cmd = format!("{} code", shell_escape(&cfg.th_binary));

    // Build the optional per-task pane-debug log BEFORE spawning so
    // the boot screen + a `start_command` failure both end up in the
    // log. Path mirrors the result file layout — sibling to
    // PROMPT.txt under the run dir — so an op looking at a failed
    // task has every artifact in one place.
    let debug_log = if cfg.debug_pane_log {
        let log_path = setup.run_dir.join(format!("{}-{}.pane.log", lang.dataset_dir(), task));
        match crate::tmux_driver::PaneDebugLog::create(&log_path) {
            Ok(dbg) => {
                eprintln!("score-tui: debug pane log → {}", log_path.display());
                Some(dbg)
            }
            Err(e) => {
                eprintln!("score-tui: warning — could not open debug log {}: {e:#}", log_path.display());
                None
            }
        }
    } else {
        None
    };

    let driver = TmuxDriver::start_command_with_debug(&session, &setup.work_dir, &shell_cmd, cfg.boot_timeout, debug_log).context("spawn th code in tmux")?;

    // Outer timeout: hard cap on per-task time. Implemented by
    // racing the human-loop future against a sleep; the tmux session
    // gets killed when `driver` drops, which happens on either path.
    let loop_fut = run_human_loop(&driver, model, &setup.prompt, &setup.prompt, &cfg.loop_cfg);
    let loop_result = match tokio::time::timeout(cfg.task_timeout, loop_fut).await {
        Ok(r) => r.context("run human loop")?,
        Err(_) => crate::human_driver::LoopResult {
            turns: 0,
            exit: LoopExit::IdleTimeout,
            final_pane: driver.capture().unwrap_or_default(),
        },
    };

    // Tool-call signal: best-effort scrape from the final pane.
    // Counts lines that look like a tool-call header — the TUI
    // prefixes tool calls with `▶ <tool_name>` / `tool: ` etc. This
    // is intentionally lenient because the exact format may drift;
    // the count is for trend tracking, not a load-bearing assertion.
    let tool_calls = count_tool_call_lines(&loop_result.final_pane);

    // Drop the driver early so the tmux session goes away BEFORE we
    // run the test command. Tests run in the same scratch dir and
    // we don't want `th code` still holding a file watcher on it.
    drop(driver);

    let (_test_stdout, counts) = finalize_and_score(lang, &setup).await.context("score work dir")?;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let duration_ms: u64 = (t0.elapsed().as_secs_f64() * 1000.0).max(0.0) as u64;

    // Stuck-means-failed guard. If the LLM-as-human loop bailed
    // before any real interaction (turn==1 and exit != Complete) we
    // refuse to count a passing test result. Rationale: aider-polyglot
    // fixtures should not pass un-edited (the failing tests are the
    // point), so a "passing" result here is almost certainly the
    // harness scoring the wrong directory or a runner that prints
    // ok-on-empty. See pearl th-f46efa for the regression that
    // surfaced this — the un-fixed PR reported 2/3 Rust passes on
    // runs where the agent never typed anything.
    let solved_raw = counts.solved();
    let driver_bailed_immediately = loop_result.turns <= 1 && !matches!(loop_result.exit, LoopExit::Complete);
    let solved = if cfg.stuck_means_failed && driver_bailed_immediately {
        if solved_raw {
            eprintln!(
                "score-tui: WARNING — {}/{} reported solved=true but driver bailed on turn {} ({:?}); forcing solved=false (pearl th-f46efa)",
                lang.dataset_dir(),
                task,
                loop_result.turns,
                loop_result.exit,
            );
        }
        false
    } else {
        solved_raw
    };

    Ok(TuiTaskOutcome {
        solved,
        // Cost from the TUI path is not yet plumbed — see module doc.
        cost_usd: 0.0,
        duration_ms,
        turns: loop_result.turns,
        tool_calls,
        exit: loop_result.exit,
    })
}

/// Minimal shell escape for `th_binary` so a path with a space (e.g.
/// `~/dev/my smooth/th`) still passes correctly through `sh -c`.
fn shell_escape(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.')) {
        s.to_string()
    } else {
        // Replace ' with '\'' then wrap whole string in single quotes.
        let escaped = s.replace('\'', "'\\''");
        format!("'{escaped}'")
    }
}

/// Count lines that look like tool-call headers. Lenient — the
/// downstream consumer is a trend chart, not a correctness check.
fn count_tool_call_lines(pane: &str) -> usize {
    pane.lines()
        .filter(|l| {
            let t = l.trim_start();
            // Common TUI shapes for tool-call headers. We're matching
            // against rendered text post-ANSI-strip (tmux capture-pane
            // returns plain text), so no color codes are present.
            t.starts_with("▶ ") || t.starts_with("> ") && (t.contains("tool:") || t.contains("→")) || t.starts_with("tool:") || t.starts_with("[tool ")
        })
        .count()
}

/// Configuration for a full `score-tui` sweep. Mirrors `SweepConfig`
/// but adds TUI-specific knobs.
#[derive(Debug, Clone)]
pub struct TuiSweepConfig {
    pub gate: SweepGate,
    pub budget_usd_cap: f64,
    pub smooth_version: String,
    pub commit_sha: String,
    /// Forwarded to per-task setup. The TUI path ignores
    /// `task_opts.budget_usd` for cost accounting (see TODO above)
    /// but keeps the field around for parity with the WebSocket
    /// sweep shape.
    pub task_opts: BenchOpts,
    pub tui_cfg: TuiTaskConfig,
    /// When `Some(n)`, run at most `n` tasks before stopping. Useful
    /// for harness debug runs (`--task-limit 1` to inspect a single
    /// pane log). `None` = run all tasks selected by `gate`.
    pub task_limit: Option<usize>,
}

/// The same shape as the WebSocket `SweepRun` plus a `via` marker so
/// downstream tooling can distinguish "tui" vs "websocket" runs.
#[derive(Debug, Clone)]
pub struct TuiSweepRun {
    pub score: Score,
    pub per_task: Vec<(PolyglotLang, String, TuiTaskOutcome)>,
    /// Path discriminator. Always "tui" for runs produced by this
    /// module. The WebSocket sweep should set the same field to
    /// "websocket" on its own SweepRun shape — pearl follow-up.
    pub via: &'static str,
}

/// Resolve which `(lang, task)` pairs to run based on the gate.
/// Copied from sweep.rs because `curated_pairs` is private there;
/// kept here as a thin selector so the gate semantics stay
/// authoritative in one place at the type level (`SweepGate`).
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

/// Run the TUI sweep end-to-end. Streams per-task summaries via
/// `observer` (same trait as the WebSocket sweep) and emits a `Score`
/// at the end.
///
/// Budget semantics match `sweep::run_sweep`: the cap is checked
/// before each task; an over-cap value aborts before the NEXT task
/// (never mid-task). Since TUI cost is 0.0 today, the cap effectively
/// never fires — left in for parity so the same flag surface works.
///
/// # Errors
/// Setup errors propagate (e.g. tmux missing). Per-task LLM errors
/// degrade to `solved=false` outcomes (logged to stderr, sweep
/// continues).
pub async fn run_tui_sweep<D: DriverModel, O: SweepObserver>(curated: &CuratedList, model: &D, cfg: &TuiSweepConfig, observer: &mut O) -> Result<TuiSweepRun> {
    let mut pairs = curated_pairs(curated, cfg.gate);
    if let Some(limit) = cfg.task_limit {
        pairs.truncate(limit);
    }
    let total = pairs.len();
    let mut per_task: Vec<(PolyglotLang, String, TuiTaskOutcome)> = Vec::with_capacity(total);
    let mut durations_ms: Vec<u64> = Vec::with_capacity(total);
    let mut cumulative_cost = 0.0_f64;
    let mut budget_hit = false;

    for (idx, (lang, task)) in pairs.iter().enumerate() {
        if cumulative_cost >= cfg.budget_usd_cap {
            budget_hit = true;
            observer.on_budget_hit(cumulative_cost, cfg.budget_usd_cap);
            break;
        }

        let outcome = match run_polyglot_task_via_tui(*lang, task, model, &cfg.tui_cfg).await {
            Ok(o) => o,
            Err(e) => {
                eprintln!("score-tui: task {}/{} ({}/{}): runner error: {e:#}", idx + 1, total, lang.dataset_dir(), task);
                TuiTaskOutcome {
                    solved: false,
                    cost_usd: 0.0,
                    duration_ms: 0,
                    turns: 0,
                    tool_calls: 0,
                    exit: LoopExit::IdleTimeout,
                }
            }
        };

        cumulative_cost += outcome.cost_usd;
        durations_ms.push(outcome.duration_ms);
        let task_outcome = TaskOutcome {
            solved: outcome.solved,
            cost_usd: outcome.cost_usd,
            duration_ms: outcome.duration_ms,
            inconclusive: false,
        };
        observer.on_task_complete(idx + 1, total, *lang, task, &task_outcome, cumulative_cost);
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

    // Sanity-check the result: a real sweep should report > $0 of
    // model spend. If every task reports $0.00, the cost surface
    // wasn't wired (a real risk on the TUI path — see module doc on
    // `run_polyglot_task_via_tui` re: TUI cost not yet plumbed) and
    // any pass-rate claim should be treated with extreme suspicion.
    if score.tasks_attempted > 0 && score.cost_usd == 0.0 {
        eprintln!(
            "score-tui: WARNING — cost reported as $0.00 across {} task(s). TUI cost surfacing is not yet wired (pearl follow-up); pass-rate may not reflect real model behaviour.",
            score.tasks_attempted
        );
    }

    Ok(TuiSweepRun { score, per_task, via: "tui" })
}

struct AggregateInputs<'a> {
    smooth_version: String,
    commit_sha: String,
    cost_usd: f64,
    durations_ms: &'a [u64],
    budget_usd_cap: f64,
    budget_usd_hit: bool,
}

fn aggregate(per_task: &[(PolyglotLang, String, TuiTaskOutcome)], inputs: AggregateInputs<'_>) -> Score {
    use std::collections::BTreeMap;
    let mut by_lang_counts: BTreeMap<PolyglotLang, (u32, u32)> = BTreeMap::new();
    let mut tasks_attempted: u32 = 0;
    let mut tasks_green: u32 = 0;
    for (lang, _task, outcome) in per_task {
        let entry = by_lang_counts.entry(*lang).or_insert((0, 0));
        entry.0 += 1;
        tasks_attempted += 1;
        if outcome.solved {
            entry.1 += 1;
            tasks_green += 1;
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
        tasks_inconclusive: 0,
        cost_usd: inputs.cost_usd,
        median_task_ms: median_ms(inputs.durations_ms),
        budget_usd_cap: inputs.budget_usd_cap,
        budget_usd_hit: inputs.budget_usd_hit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_leaves_plain_path_alone() {
        assert_eq!(shell_escape("th"), "th");
        assert_eq!(shell_escape("/usr/local/bin/th"), "/usr/local/bin/th");
        assert_eq!(shell_escape("./th"), "./th");
    }

    #[test]
    fn shell_escape_quotes_spaces() {
        assert_eq!(shell_escape("/tmp/path with space/th"), "'/tmp/path with space/th'");
    }

    #[test]
    fn shell_escape_handles_embedded_single_quote() {
        assert_eq!(shell_escape("it's/th"), "'it'\\''s/th'");
    }

    #[test]
    fn count_tool_call_lines_picks_up_arrow_prefix() {
        let pane = "\
some preamble
▶ read_file path=foo.py
text in between
▶ write_file path=bar.py
final line
";
        assert_eq!(count_tool_call_lines(pane), 2);
    }

    #[test]
    fn count_tool_call_lines_zero_on_plain_text() {
        assert_eq!(count_tool_call_lines("hello world\nno tools here\n"), 0);
    }

    #[test]
    fn into_task_outcome_drops_extra_fields() {
        let t = TuiTaskOutcome {
            solved: true,
            cost_usd: 0.42,
            duration_ms: 1234,
            turns: 7,
            tool_calls: 3,
            exit: LoopExit::Complete,
        };
        let o = t.into_task_outcome();
        assert!(o.solved);
        assert!((o.cost_usd - 0.42).abs() < 1e-9);
        assert_eq!(o.duration_ms, 1234);
        assert!(!o.inconclusive);
    }

    #[test]
    fn curated_pairs_pr_gate_size_matches_lang_count() {
        let list = CuratedList::default_embedded().unwrap();
        let pairs = curated_pairs(&list, SweepGate::Pr { tasks_per_language: 2 });
        // 6 langs × 2 tasks each = 12
        assert_eq!(pairs.len(), 12);
    }

    #[test]
    fn curated_pairs_release_gate_is_full_list() {
        let list = CuratedList::default_embedded().unwrap();
        let pairs = curated_pairs(&list, SweepGate::Release);
        assert_eq!(pairs.len(), 120);
    }

    #[test]
    fn aggregate_handles_empty_per_task() {
        let score = aggregate(
            &[],
            AggregateInputs {
                smooth_version: "0.0.0-test".into(),
                commit_sha: "abc".into(),
                cost_usd: 0.0,
                durations_ms: &[],
                budget_usd_cap: 10.0,
                budget_usd_hit: false,
            },
        );
        assert_eq!(score.tasks_attempted, 0);
        assert_eq!(score.tasks_green, 0);
        assert_eq!(score.overall_pass_rate, 0.0);
        assert_eq!(score.median_task_ms, 0);
        assert!(score.by_language.is_empty());
    }

    #[test]
    fn aggregate_produces_per_language_breakdown() {
        let per_task = vec![
            (
                PolyglotLang::Python,
                "p1".into(),
                TuiTaskOutcome {
                    solved: true,
                    cost_usd: 0.0,
                    duration_ms: 1000,
                    turns: 3,
                    tool_calls: 2,
                    exit: LoopExit::Complete,
                },
            ),
            (
                PolyglotLang::Python,
                "p2".into(),
                TuiTaskOutcome {
                    solved: false,
                    cost_usd: 0.0,
                    duration_ms: 2000,
                    turns: 15,
                    tool_calls: 5,
                    exit: LoopExit::TurnCap,
                },
            ),
            (
                PolyglotLang::Rust,
                "r1".into(),
                TuiTaskOutcome {
                    solved: true,
                    cost_usd: 0.0,
                    duration_ms: 3000,
                    turns: 4,
                    tool_calls: 3,
                    exit: LoopExit::Complete,
                },
            ),
        ];
        let score = aggregate(
            &per_task,
            AggregateInputs {
                smooth_version: "0.0.0-test".into(),
                commit_sha: "abc".into(),
                cost_usd: 0.0,
                durations_ms: &[1000, 2000, 3000],
                budget_usd_cap: 10.0,
                budget_usd_hit: false,
            },
        );
        assert_eq!(score.tasks_attempted, 3);
        assert_eq!(score.tasks_green, 2);
        assert!((score.overall_pass_rate - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(score.by_language["python"].tasks_attempted, 2);
        assert_eq!(score.by_language["python"].tasks_green, 1);
        assert_eq!(score.by_language["rust"].tasks_attempted, 1);
        assert_eq!(score.by_language["rust"].tasks_green, 1);
    }
}
