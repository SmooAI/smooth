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

    /// Render a per-task HTML eval report from a bench-run dir.
    ///
    /// Reads `<run-dir>/result.json` + `<run-dir>/PROMPT.txt` plus the
    /// pearl's comments (heartbeats, [STEERING], [METRICS], [IDLE])
    /// from `~/.smooth/dolt/`, and writes `<run-dir>/eval.html`.
    EvalReport {
        /// Run id (last 8 chars under `~/.smooth/bench-runs/`) or full
        /// path to a run directory.
        run: String,
        /// Output path. Default: `<run-dir>/eval.html`.
        #[arg(long)]
        output: Option<PathBuf>,
    },
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
        Commands::EvalReport { run, output } => run_eval_report(&run, output.as_deref()),
    }
}

fn run_eval_report(run: &str, output: Option<&std::path::Path>) -> Result<()> {
    // Accept either a full path or a run-id (last 8 hex chars).
    let run_dir = if std::path::Path::new(run).is_dir() {
        PathBuf::from(run)
    } else {
        smooth_bench::runs_root()?.join(run)
    };
    if !run_dir.exists() {
        anyhow::bail!("run dir not found: {}", run_dir.display());
    }
    let html = smooth_bench::eval_html::render_run_html(&run_dir).with_context(|| format!("render eval HTML for {}", run_dir.display()))?;
    let out_path = output.map_or_else(|| run_dir.join("eval.html"), std::path::Path::to_path_buf);
    std::fs::write(&out_path, html).with_context(|| format!("write {}", out_path.display()))?;
    println!("wrote {}", out_path.display());
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
