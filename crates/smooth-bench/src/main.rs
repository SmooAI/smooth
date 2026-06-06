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

    /// SWE-bench (Princeton / SWE-bench Verified or Lite). Real
    /// GitHub issues from popular Python repos with held-out test
    /// suites. Industry-comparable score. Pearl th-swe-bench.
    ScoreSweBench(ScoreSweBenchArgs),

    /// Multi-axis benchmark on our stack (Rust + Python + TS), curated
    /// mini-projects with hidden test suites + grade.toml weights.
    /// Scores not just pass/fail but edit efficiency, verify
    /// discipline, tool-use quality, and cost. Pearl th-score-real.
    ScoreReal(ScoreRealArgs),

    /// Auto-harvest tasks from real merged PRs. Each task = a PR's
    /// title + body as prompt, parent-commit workspace, score by
    /// whether the agent makes the same test(s) pass that the human
    /// PR did. Pearl th-score-replay.
    ScoreReplay(ScoreReplayArgs),

    /// Operational-competence tasks (NOT coding). Each task
    /// materializes a polluted filesystem (Docker layer cruft,
    /// __pycache__ debris, orphaned node_modules) and scores the
    /// agent on bytes freed, files preserved, and whether it asked
    /// before deleting. Pearl th-85e3c5.
    ScoreCleanup(ScoreCleanupArgs),
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

    /// Driver persona. `user` is the historical non-technical-end-user
    /// driver (default — preserves comparability with prior matrices).
    /// `coach` is the senior pair-programmer persona from pearl
    /// th-e17b1a that probes for actual test runs before firing
    /// `TASK_COMPLETE` and suggests concrete debugging steps without
    /// giving the answer. The driver model is identical between the
    /// two; only the system prompt + per-turn template change.
    #[arg(long, default_value_t = DriverPersonaArg::User, value_enum)]
    driver_persona: DriverPersonaArg,

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

/// Clap-friendly mirror of [`smooth_bench::human_driver::DriverPersona`].
/// We keep a separate enum here so the `--driver-persona` flag's value
/// names (`user`, `coach`) stay independent of any future rename in the
/// library type. Pearl th-e17b1a.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum DriverPersonaArg {
    User,
    Coach,
}

impl std::fmt::Display for DriverPersonaArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::User => "user",
            Self::Coach => "coach",
        })
    }
}

impl DriverPersonaArg {
    fn to_persona(self) -> smooth_bench::human_driver::DriverPersona {
        match self {
            Self::User => smooth_bench::human_driver::DriverPersona::User,
            Self::Coach => smooth_bench::human_driver::DriverPersona::Coach,
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

#[derive(Parser, Debug)]
struct ScoreSweBenchArgs {
    /// Variant — `verified` (default) or `lite`.
    #[arg(long, default_value = "verified")]
    variant: String,

    /// Cap on number of instances. 0 = run every instance the dataset
    /// yields (~500 for verified).
    #[arg(long, default_value_t = 0)]
    task_limit: usize,

    /// Routing alias / concrete model id forwarded to `th code --model`.
    #[arg(long)]
    under_test_model: Option<String>,

    /// Driver persona.
    #[arg(long, default_value_t = DriverPersonaArg::User, value_enum)]
    driver_persona: DriverPersonaArg,

    /// Output path. If ends in `.json`, only JSON is written; otherwise
    /// stdout gets a human table.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Path to the `th` binary to drive. Defaults to "th" on PATH.
    #[arg(long, default_value = "th")]
    th_binary: String,

    /// Per-task wall-clock cap (seconds).
    #[arg(long, default_value_t = 900)]
    task_timeout_s: u64,
}

#[derive(Parser, Debug)]
struct ScoreRealArgs {
    /// Cap on number of tasks. 0 = run every task in `--tasks-dir`.
    #[arg(long, default_value_t = 0)]
    task_limit: usize,

    /// Routing alias / concrete model id forwarded to `th code --model`.
    #[arg(long)]
    under_test_model: Option<String>,

    /// Driver persona.
    #[arg(long, default_value_t = DriverPersonaArg::User, value_enum)]
    driver_persona: DriverPersonaArg,

    /// Directory containing one subdir per curated task. Defaults to
    /// the in-repo `crates/smooth-bench/tasks-real/`.
    #[arg(long)]
    tasks_dir: Option<PathBuf>,

    /// Output path. `.json` → JSON only; otherwise human table.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Hard USD cap on cumulative cost across the sweep.
    #[arg(long, default_value_t = 10.0)]
    budget_usd: f64,
}

#[derive(Parser, Debug)]
struct ScoreCleanupArgs {
    /// Cap on number of tasks. 0 = run every task in `--tasks-dir`.
    #[arg(long, default_value_t = 0)]
    task_limit: usize,

    /// Directory containing one subdir per cleanup-* task. Defaults
    /// to the in-repo `crates/smooth-bench/tasks-real/`.
    #[arg(long)]
    tasks_dir: Option<PathBuf>,

    /// Output path. `.json` → JSON only; otherwise human table.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Path to a mock-agent script. The script is invoked with
    /// `WORKSPACE` env set to the polluted dir and is expected to
    /// perform the cleanup. Provides a deterministic baseline for
    /// the scoring pipeline. Use this path:
    /// `crates/smooth-bench/tasks-real/_mock-agents/perfect-pycache.sh`
    /// for a known-good cleanup. Implies `--driver=mock`.
    #[arg(long)]
    mock_agent: Option<PathBuf>,

    /// Agent backend to dispatch each task against. Pearl th-e5b773
    /// (multi-driver harness). `mock` requires `--mock-agent`;
    /// `opencode` requires `opencode` on PATH; `smooth` and
    /// `claude-code` are TODO (pearls th-754512, th-36145e).
    #[arg(long, default_value_t = AgentDriverKind::Mock, value_enum)]
    driver: AgentDriverKind,

    /// Model id forwarded to the chosen driver. For `opencode` use the
    /// model name as configured in your `~/.config/opencode/opencode.json`
    /// (e.g. `deepseek-v4-flash` if that's the alias your llm.smoo.ai
    /// provider exposes). Ignored by `mock`.
    #[arg(long)]
    model: Option<String>,

    /// Per-task wall-clock timeout in seconds.
    #[arg(long, default_value_t = 600)]
    task_timeout_s: u64,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum AgentDriverKind {
    Mock,
    Opencode,
    /// Smooth's own `th code`. Pearl th-754512.
    Smooth,
    /// Earendil's `pi` coding agent. Pearl th-491e0c.
    Pi,
    /// Claude Code's `claude -p`. Pearl th-36145e (canceled per user
    /// directive 2026-06-03 — Pi takes its slot).
    ClaudeCode,
}

#[derive(Parser, Debug)]
struct ScoreReplayArgs {
    /// Target repo in `owner/repo` form. PRs are harvested via `gh`.
    #[arg(long)]
    repo: String,

    /// Only consider PRs merged on or after this date (`YYYY-MM-DD`).
    #[arg(long)]
    since: String,

    /// Cap on the number of PRs to replay.
    #[arg(long, default_value_t = 20)]
    task_limit: usize,

    /// Routing alias / concrete model id forwarded to `th code --model`.
    #[arg(long)]
    under_test_model: Option<String>,

    /// Driver persona.
    #[arg(long, default_value_t = DriverPersonaArg::User, value_enum)]
    driver_persona: DriverPersonaArg,

    /// Output path. `.json` → JSON only.
    #[arg(long)]
    output: Option<PathBuf>,
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
        Commands::ScoreSweBench(args) => run_score_swe_bench(args).await,
        Commands::ScoreReal(args) => run_score_real(args).await,
        Commands::ScoreReplay(args) => run_score_replay(args).await,
        Commands::ScoreCleanup(args) => run_score_cleanup(args).await,
    }
}

async fn run_score_swe_bench(args: ScoreSweBenchArgs) -> Result<()> {
    use smooth_bench::score_swe_bench::{run_swe_bench_sweep, SweBenchConfig};
    use smooth_bench::swe_bench_dataset::{cache_dir, SweBenchVariant};
    use smooth_bench::tui_score::TuiTaskConfig;

    let variant = match args.variant.to_lowercase().as_str() {
        "verified" => SweBenchVariant::Verified,
        "lite" => SweBenchVariant::Lite,
        other => anyhow::bail!("unknown --variant {other:?}; valid values: verified, lite"),
    };

    let home = dirs_next::home_dir().ok_or_else(|| anyhow::anyhow!("home dir unknown"))?;
    let work_root = home.join(".smooth").join("bench-runs").join(format!("swe-bench-{}", random_run_id()));
    std::fs::create_dir_all(&work_root).context("create work_root")?;

    let tui_cfg = TuiTaskConfig {
        th_binary: args.th_binary.clone(),
        task_timeout: std::time::Duration::from_secs(args.task_timeout_s),
        ..Default::default()
    };

    let cfg = SweBenchConfig {
        variant,
        task_limit: (args.task_limit > 0).then_some(args.task_limit),
        under_test_model: args.under_test_model.unwrap_or_default(),
        driver_persona: args.driver_persona.to_persona(),
        cache_dir: cache_dir(variant)?,
        work_root,
        smooth_version: env!("CARGO_PKG_VERSION").to_string(),
        commit_sha: smooth_bench::sweep::current_commit_sha(),
        budget_usd_cap: 100.0,
        tui_cfg,
    };

    let run = run_swe_bench_sweep(&cfg).await?;
    emit_score_json_or_table(&run.score, args.output.as_deref())
}

async fn run_score_real(args: ScoreRealArgs) -> Result<()> {
    use smooth_bench::score_real::{run_real_sweep, RealConfig};

    let tasks_dir = args.tasks_dir.unwrap_or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("smooth-bench")
            .join("tasks-real")
    });

    let cfg = RealConfig {
        task_limit: (args.task_limit > 0).then_some(args.task_limit),
        under_test_model: args.under_test_model.unwrap_or_default(),
        driver_persona: args.driver_persona.to_persona(),
        tasks_dir,
        smooth_version: env!("CARGO_PKG_VERSION").to_string(),
        commit_sha: smooth_bench::sweep::current_commit_sha(),
        budget_usd_cap: args.budget_usd,
    };

    let run = run_real_sweep(&cfg).await?;
    emit_score_json_or_table(&run.base, args.output.as_deref())
}

async fn run_score_replay(args: ScoreReplayArgs) -> Result<()> {
    use smooth_bench::score_replay::{run_replay_sweep, ReplayConfig};

    let since = chrono::NaiveDate::parse_from_str(&args.since, "%Y-%m-%d").with_context(|| format!("--since must be YYYY-MM-DD, got {:?}", args.since))?;

    let home = dirs_next::home_dir().ok_or_else(|| anyhow::anyhow!("home dir unknown"))?;
    let work_root = home.join(".smooth").join("bench-runs").join(format!("replay-{}", random_run_id()));
    std::fs::create_dir_all(&work_root).context("create work_root")?;

    let cfg = ReplayConfig {
        repo: args.repo,
        since,
        task_limit: args.task_limit,
        under_test_model: args.under_test_model.unwrap_or_default(),
        driver_persona: args.driver_persona.to_persona(),
        work_root,
    };

    let score = run_replay_sweep(&cfg).await?;
    emit_score_json_or_table(&score, args.output.as_deref())
}

async fn run_score_cleanup(args: ScoreCleanupArgs) -> Result<()> {
    use smooth_bench::agent_driver::{AgentDriver, DispatchRequest, MockAgentDriver, OpenCodeDriver, PiDriver, SmoothDriver};
    use smooth_bench::score_cleanup::{
        aggregate, destroyed_paths, discover_tasks, load_manifest, measure_bytes, run_setup, score_one_task, sweep_passed, AgentRunArtifacts,
    };

    let tasks_dir = args.tasks_dir.unwrap_or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("smooth-bench")
            .join("tasks-real")
    });

    let mut to_run = discover_tasks(&tasks_dir)?;
    if args.task_limit > 0 {
        to_run.truncate(args.task_limit);
    }
    if to_run.is_empty() {
        anyhow::bail!("no cleanup-* tasks with manifest.toml found under {}", tasks_dir.display());
    }

    let home = dirs_next::home_dir().ok_or_else(|| anyhow::anyhow!("home dir unknown"))?;
    let run_root = home.join(".smooth").join("bench-runs").join(format!("cleanup-{}", random_run_id()));
    std::fs::create_dir_all(&run_root).context("create run_root")?;
    eprintln!("score-cleanup: work root = {}", run_root.display());

    // Resolve the driver up-front so a misconfig fails fast (e.g.
    // --driver=mock without --mock-agent) before we materialize any
    // task workspaces. `--mock-agent <path>` implies `--driver=mock`
    // for back-compat with the original CLI shape.
    let driver_kind = if args.mock_agent.is_some() { AgentDriverKind::Mock } else { args.driver };
    let driver: Box<dyn AgentDriver> = match driver_kind {
        AgentDriverKind::Mock => {
            let script = args
                .mock_agent
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--driver=mock requires --mock-agent <path>"))?;
            Box::new(MockAgentDriver::new(script))
        }
        AgentDriverKind::Opencode => Box::new(OpenCodeDriver::from_path()),
        AgentDriverKind::Smooth => Box::new(SmoothDriver::from_path()),
        AgentDriverKind::Pi => Box::new(PiDriver::from_path()),
        AgentDriverKind::ClaudeCode => {
            anyhow::bail!("--driver=claude-code canceled per user direction 2026-06-03 (pearl th-36145e); use --driver=pi instead (th-491e0c)")
        }
    };
    eprintln!("score-cleanup: driver = {}", driver.name());
    if let Some(m) = args.model.as_deref() {
        eprintln!("score-cleanup: model  = {m}");
    }

    let mut per_task = Vec::new();
    for task_dir in &to_run {
        let manifest = load_manifest(task_dir)?;
        let work_dir = run_root.join(&manifest.task.id);
        eprintln!("\n=== {} ===", manifest.task.id);
        eprintln!("desc: {}", manifest.task.description);

        run_setup(task_dir, &manifest.setup.script, manifest.setup.timeout_s, &work_dir).with_context(|| format!("setup {}", manifest.task.id))?;
        let bytes_before = measure_bytes(&work_dir)?;
        eprintln!("bytes before: {bytes_before}");

        // Build the agent-facing prompt from the task's README. Mock
        // drivers ignore it; live drivers feed it straight to the LLM.
        let prompt = std::fs::read_to_string(task_dir.join("README.md")).unwrap_or_default();
        let artifacts: AgentRunArtifacts = driver
            .dispatch(DispatchRequest {
                task_id: &manifest.task.id,
                workspace: &work_dir,
                prompt: &prompt,
                model: args.model.as_deref(),
                timeout: std::time::Duration::from_secs(args.task_timeout_s),
                coach: manifest.coach.mode,
            })
            .await
            .with_context(|| format!("dispatch {} via {}", manifest.task.id, driver.name()))?;

        let bytes_after = measure_bytes(&work_dir)?;
        eprintln!("bytes after:  {bytes_after}  (freed {})", bytes_before.saturating_sub(bytes_after));
        let destroyed = destroyed_paths(&work_dir, &manifest.expect.must_preserve);
        if !destroyed.is_empty() {
            eprintln!("DESTROYED preserved files: {destroyed:?}");
        }

        let result = score_one_task(
            &manifest.task,
            &manifest.expect,
            &manifest.weights,
            bytes_before,
            bytes_after,
            destroyed,
            &artifacts,
        );
        eprintln!("weighted score: {:.3}", result.weighted_score);
        per_task.push(result);
    }

    let passed = sweep_passed(&per_task);
    let aggregate_score = aggregate(&per_task, env!("CARGO_PKG_VERSION").to_string(), smooth_bench::sweep::current_commit_sha());
    eprintln!("\n=== AGGREGATE ===");
    eprintln!("mean weighted: {:.3}  passed={passed}", aggregate_score.overall_pass_rate);

    // Emit JSON sidecar so downstream tools can diff runs.
    if let Some(out) = args.output.as_deref() {
        if out.extension().and_then(|s| s.to_str()) == Some("json") {
            let payload = serde_json::json!({
                "base": aggregate_score,
                "by_task": per_task,
            });
            std::fs::write(out, serde_json::to_string_pretty(&payload)?)?;
            eprintln!("wrote {}", out.display());
        }
    }

    if passed {
        Ok(())
    } else {
        anyhow::bail!("score-cleanup sweep did not pass — mean {:.3}", aggregate_score.overall_pass_rate);
    }
}

fn random_run_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{nanos:x}")
}

fn emit_score_json_or_table(score: &smooth_bench::score::Score, output: Option<&std::path::Path>) -> Result<()> {
    match output {
        Some(p) if p.extension().and_then(|s| s.to_str()) == Some("json") => {
            std::fs::write(p, serde_json::to_string_pretty(score)?).context("write output json")?;
            eprintln!("wrote {}", p.display());
        }
        Some(p) => {
            std::fs::write(p, score.render_table()).context("write output table")?;
            eprintln!("wrote {}", p.display());
        }
        None => {
            println!("{}", score.render_table());
        }
    }
    Ok(())
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
    let driver_model =
        smooth_bench::human_driver::LlmDriverModel::from_activity_with_persona(args.driver_model.to_activity(), args.driver_persona.to_persona())
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
