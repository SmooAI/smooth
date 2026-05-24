//! `smooth-bench` — internal benchmark harness binary.
//!
//! Not shipped in the `th` CLI. Run via:
//!
//!     cargo run -p smooai-smooth-bench -- aider-polyglot --task grade-school
//!     cargo run -p smooai-smooth-bench -- score --pr
//!
//! or the top-level wrapper `scripts/bench.sh`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use smooth_bench::curated::CuratedList;
use smooth_bench::sweep::{current_commit_sha, run_sweep, PolyglotTaskRunner, StdoutObserver, SweepConfig, SweepGate, SweepRun};
use smooth_bench::tui_score::{run_tui_sweep, TuiSweepConfig, TuiTaskConfig};
use smooth_bench::{print_summary, run_aider_polyglot, BenchOpts, PolyglotLang};

#[derive(Parser)]
#[command(name = "smooth-bench", version, about = "Smooth benchmark harness (internal)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a single Aider Polyglot task.
    AiderPolyglot {
        /// Task name (e.g. `grade-school`, `leap`, `forth`).
        #[arg(long)]
        task: String,
        /// Language subset. Default: python.
        #[arg(long, default_value = "python")]
        lang: String,
        /// Budget limit in USD for the LLM calls. Default: $5.00.
        /// Bench tasks are meant to be exercised end-to-end; we
        /// push through plateaus (up to 20 outer iterations) and
        /// need enough headroom that the cap isn't the limiter.
        #[arg(long, default_value_t = 5.00)]
        budget: f64,
        /// Override the routing (passed through to Big Smooth).
        #[arg(long)]
        model: Option<String>,
        /// Big Smooth URL. Defaults to http://localhost:4400.
        #[arg(long, default_value = "http://localhost:4400")]
        url: String,
    },

    /// Run the curated aider-polyglot sweep and emit an aggregate Score.
    ///
    /// This is the "The Line" authoritative benchmark — the single
    /// pass-rate Smoo AI publishes with every release.
    Score(ScoreArgs),

    /// Render `eval.html` per-task report from existing `result.json`
    /// files. Pearl th-2be27b. Pure offline operation: no LLM, no
    /// network, no agent dispatch — just HTML rendering for human
    /// review of completed runs.
    EvalReport {
        /// Directory containing one or more `result.json` files. May
        /// be a single run dir or a sweep dir (subdirs each with a
        /// `result.json`). Default: `~/.smooth/bench-runs`.
        #[arg(long)]
        run_dir: Option<PathBuf>,
    },

    /// Run the curated aider-polyglot sweep through `th code` (the
    /// real TUI), driving the agent with an LLM-as-human loop over
    /// tmux. Emits the same Score shape as `score`. Pearl th-399196.
    ///
    /// Same flag surface as `score` plus `--tmux-session` /
    /// `--driver-model`. The TUI path requires `tmux` on PATH and
    /// `th` (this binary's sibling) reachable via `--th-binary`.
    ScoreTui(ScoreTuiArgs),
}

#[derive(Parser, Debug)]
struct ScoreArgs {
    /// Authoritative sample: 20 tasks × 6 languages = 120 runs.
    /// Mutually exclusive with `--pr`.
    #[arg(long, conflicts_with = "pr")]
    release: bool,

    /// CI-gate sample: 3 tasks × 6 languages = 18 runs. Mutually
    /// exclusive with `--release`. If neither is given, defaults
    /// to `--pr` (safer — cheaper, faster) and prints a note.
    #[arg(long)]
    pr: bool,

    /// Hard USD cap. When the running cost total exceeds this, the
    /// sweep aborts between tasks and emits a partial Score with
    /// `budget_usd_hit: true`.
    #[arg(long, default_value_t = 10.0)]
    budget_usd: f64,

    /// Output path. If the path ends in `.json`, only JSON is written;
    /// otherwise a human table is rendered and the JSON is still
    /// available in the per-run dir. Default: stdout (human table).
    #[arg(long)]
    output: Option<PathBuf>,

    /// Which routing slot to hit. V1 only `smooth-coding` is
    /// exercised; other slots are stubbed.
    #[arg(long, default_value_t = Slot::SmoothCoding, value_enum)]
    slot: Slot,

    /// Big Smooth URL.
    #[arg(long, default_value = "http://localhost:4400")]
    url: String,
}

#[derive(Parser, Debug)]
struct ScoreTuiArgs {
    /// Authoritative sample (120 runs). Mutually exclusive with `--pr`.
    #[arg(long, conflicts_with = "pr")]
    release: bool,

    /// CI-gate sample (18 runs). Mutually exclusive with `--release`.
    /// If neither is set, defaults to `--pr`.
    #[arg(long)]
    pr: bool,

    /// Hard USD cap. (TUI cost reporting isn't yet wired — see
    /// `tui_score::run_polyglot_task_via_tui` doc — so this cap
    /// effectively never fires in v1. Kept for flag parity with
    /// `score` so wrapper scripts work unchanged.)
    #[arg(long, default_value_t = 10.0)]
    budget_usd: f64,

    /// Output path. If the path ends in `.json`, only JSON is written;
    /// otherwise a human table is rendered. Default: stdout.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Big Smooth URL. `th code` connects to this — must be running.
    #[arg(long, default_value = "http://localhost:4400")]
    url: String,

    /// tmux session name prefix. Each task uses
    /// `{prefix}-{lang}-{task}` so parallel runs don't collide.
    #[arg(long, default_value = "smooth-bench-tui")]
    tmux_session: String,

    /// Path to the `th` binary to drive. Defaults to "th" (relying
    /// on PATH). Override when bench-testing a worktree build that
    /// isn't installed.
    #[arg(long, default_value = "th")]
    th_binary: String,

    /// Routing slot the driver LLM uses to compose user messages.
    /// `Summarize` is the default — cheap, fast, doesn't burn the
    /// model under test's budget.
    #[arg(long, default_value_t = DriverModelSlot::Summarize, value_enum)]
    driver_model: DriverModelSlot,

    /// Maximum LLM-as-human turns per task before bailing as
    /// "turn cap hit" and scoring the workspace as-is.
    #[arg(long, default_value_t = 15)]
    max_turns: usize,

    /// Per-task wall-clock cap. Hard ceiling on time spent driving a
    /// single task — independent of the per-turn idle timeout.
    #[arg(long, default_value_t = 900)]
    task_timeout_s: u64,

    /// Write a per-task pane-debug log to the run dir. Each `send`
    /// and each `wait_for_idle` boundary appends a timestamped record
    /// so a failed bench task can be inspected post-hoc. Heavyweight
    /// — leave off in CI gates.
    #[arg(long, default_value_t = false)]
    debug: bool,

    /// Cap the number of tasks attempted (post-gate-selection). 0 =
    /// no cap (the default). Used for harness debug runs like
    /// `--task-limit 1` to inspect a single pane log.
    #[arg(long, default_value_t = 0)]
    task_limit: usize,

    /// Pause (seconds) between tasks. Lets the upstream Anthropic
    /// per-minute TPM bucket roll over before the next task starts.
    /// On a 30K input-TPM tier, three back-to-back coding tasks
    /// easily starve the 4th of budget — 60s gives one full bucket
    /// refill. 0 disables (the historical behaviour). Pearl
    /// th-4dd874.
    #[arg(long, default_value_t = 0)]
    inter_task_sleep_s: u64,

    /// Allow a task to be marked solved=true even when the
    /// LLM-as-human driver bailed on turn 1. Default is to refuse:
    /// aider-polyglot fixtures should not pass un-edited, so a
    /// passing result without any agent interaction is almost
    /// always a harness bug. Pearl th-f46efa.
    #[arg(long, default_value_t = false)]
    allow_stuck_passes: bool,

    /// Allow a task to be marked solved=true even when the agent
    /// made ZERO edits to editable source files. Default is to
    /// refuse: a passing test result on a workspace where the agent
    /// changed nothing is almost certainly a tooling artefact (e.g.
    /// cargo's shared target cache reusing a previously-compiled
    /// binary). Pearl th-a5ca18 Bug 3. Set this only for paranoid
    /// debugging.
    #[arg(long, default_value_t = false)]
    allow_no_edit_passes: bool,

    /// Pass `--model NAME` through to `th code` so the under-test
    /// agent uses a specific Smoo alias / model id instead of the
    /// `smooth-coding` default. Useful for proving the harness end-
    /// to-end against a tool-call-friendly model (e.g.
    /// `smooth-coding-claude`) when the default primary is still
    /// emitting pseudo-XML. Pearl th-67e338.
    #[arg(long)]
    under_test_model: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum DriverModelSlot {
    Summarize,
    Fast,
    Judge,
}

impl DriverModelSlot {
    fn to_activity(self) -> smooth_operator::providers::Activity {
        match self {
            Self::Summarize => smooth_operator::providers::Activity::Summarize,
            Self::Fast => smooth_operator::providers::Activity::Fast,
            Self::Judge => smooth_operator::providers::Activity::Judge,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // the "Smooth" prefix matches provider-registry slot names
enum Slot {
    SmoothCoding,
    SmoothReasoning,
    SmoothFast,
    SmoothCheap,
    SmoothJudge,
}

impl Slot {
    // Kept as an instance method so future per-slot routing (v2) has a
    // natural home. For v1 all slots fall through to whatever the
    // local routing config picks — the caller warns for non-coding
    // slots so operators know.
    #[allow(clippy::unused_self)]
    fn model_override(self) -> Option<String> {
        None
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Commands::AiderPolyglot {
            task,
            lang,
            budget,
            model,
            url,
        } => {
            let lang_enum =
                PolyglotLang::from_name(&lang).ok_or_else(|| anyhow::anyhow!("unknown language: {lang} (try python, rust, go, javascript, java, cpp)"))?;
            let opts = BenchOpts {
                big_smooth_url: url,
                budget_usd: Some(budget),
                model,
            };
            println!("Running aider-polyglot/{}/{task} …", lang_enum.dataset_dir());
            let result = run_aider_polyglot(lang_enum, &task, &opts).await?;
            print_summary(&result);
            if result.solved {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        Commands::Score(args) => run_score(args).await,
        Commands::EvalReport { run_dir } => run_eval_report(run_dir),
        Commands::ScoreTui(args) => run_score_tui(args).await,
    }
}

fn run_eval_report(run_dir: Option<PathBuf>) -> Result<()> {
    let dir = run_dir
        .or_else(|| dirs_next::home_dir().map(|h| h.join(".smooth").join("bench-runs")))
        .ok_or_else(|| anyhow::anyhow!("could not resolve default --run-dir; pass --run-dir explicitly"))?;
    let outcome = smooth_bench::eval_report::render_dir(&dir)?;
    for path in &outcome.eval_paths {
        println!("wrote {}", path.display());
    }
    if let Some(index) = &outcome.index_path {
        println!("wrote {} (index)", index.display());
    }
    Ok(())
}

async fn run_score(args: ScoreArgs) -> Result<()> {
    if args.slot != Slot::SmoothCoding {
        eprintln!("note: slot {:?} not yet wired end-to-end; falling back to local routing", args.slot);
    }

    // Neither flag set → default to --pr and say so.
    let gate = if args.release {
        SweepGate::Release
    } else {
        if !args.pr {
            eprintln!("neither --release nor --pr given; defaulting to --pr (3 tasks × 6 langs = 18 runs)");
        }
        SweepGate::Pr { tasks_per_language: 3 }
    };

    let curated = CuratedList::default_embedded().context("loading embedded curated task list")?;
    let cfg = SweepConfig {
        gate,
        budget_usd_cap: args.budget_usd,
        smooth_version: env!("CARGO_PKG_VERSION").to_string(),
        commit_sha: current_commit_sha(),
        task_opts: BenchOpts {
            big_smooth_url: args.url.clone(),
            budget_usd: Some(args.budget_usd),
            model: args.slot.model_override(),
        },
    };

    let runner = PolyglotTaskRunner;
    let mut observer = StdoutObserver;
    let SweepRun { score, per_task: _ } = run_sweep(&curated, &runner, &cfg, &mut observer).await?;

    emit_score(&score, args.output.as_deref())?;

    // Non-zero exit when budget was hit — CI gate will notice.
    if score.budget_usd_hit {
        std::process::exit(2);
    }
    Ok(())
}

async fn run_score_tui(args: ScoreTuiArgs) -> Result<()> {
    // Neither flag set → default to --pr.
    let gate = if args.release {
        SweepGate::Release
    } else {
        if !args.pr {
            eprintln!("neither --release nor --pr given; defaulting to --pr (3 tasks × 6 langs = 18 runs)");
        }
        SweepGate::Pr { tasks_per_language: 3 }
    };

    let curated = CuratedList::default_embedded().context("loading embedded curated task list")?;
    let driver_model = smooth_bench::human_driver::LlmDriverModel::from_activity(args.driver_model.to_activity())
        .context("loading driver model from providers.json — is the slot configured?")?;

    let loop_cfg = smooth_bench::human_driver::LoopConfig {
        max_turns: args.max_turns,
        ..smooth_bench::human_driver::LoopConfig::default()
    };

    let tui_cfg = TuiTaskConfig {
        th_binary: args.th_binary.clone(),
        tmux_session_prefix: args.tmux_session.clone(),
        // Pull boot_timeout from the default — `th code` needs > 15s
        // to bring up the Safehouse microVM + cast. See
        // `TuiTaskConfig::default` for the rationale.
        boot_timeout: TuiTaskConfig::default().boot_timeout,
        loop_cfg,
        task_timeout: std::time::Duration::from_secs(args.task_timeout_s),
        debug_pane_log: args.debug,
        stuck_means_failed: !args.allow_stuck_passes,
        require_edits_for_pass: !args.allow_no_edit_passes,
        under_test_model: args.under_test_model.clone(),
    };

    let cfg = TuiSweepConfig {
        gate,
        budget_usd_cap: args.budget_usd,
        smooth_version: env!("CARGO_PKG_VERSION").to_string(),
        commit_sha: current_commit_sha(),
        task_opts: BenchOpts {
            big_smooth_url: args.url.clone(),
            budget_usd: Some(args.budget_usd),
            model: None,
        },
        tui_cfg,
        task_limit: if args.task_limit == 0 { None } else { Some(args.task_limit) },
        inter_task_sleep_s: args.inter_task_sleep_s,
    };

    let mut observer = StdoutObserver;
    let run = run_tui_sweep(&curated, &driver_model, &cfg, &mut observer).await?;
    eprintln!("score-tui: via={}", run.via);

    emit_score(&run.score, args.output.as_deref())?;
    if run.score.budget_usd_hit {
        std::process::exit(2);
    }
    Ok(())
}

fn emit_score(score: &smooth_bench::score::Score, output: Option<&std::path::Path>) -> Result<()> {
    let json = serde_json::to_string_pretty(score).context("serialising Score")?;

    match output {
        Some(path) if path.extension().and_then(|e| e.to_str()) == Some("json") => {
            std::fs::write(path, json).with_context(|| format!("writing score JSON to {}", path.display()))?;
            eprintln!("wrote {}", path.display());
        }
        Some(path) => {
            std::fs::write(path, score.render_table()).with_context(|| format!("writing score table to {}", path.display()))?;
            eprintln!("wrote {}", path.display());
        }
        None => {
            // stdout = human table; JSON is always recoverable by
            // re-running with `--output score.json`.
            println!("{}", score.render_table());
        }
    }
    Ok(())
}
