//! `smooth-bench` — internal benchmark harness binary.
//!
//! Not shipped in the `th` CLI. Run via:
//!
//!     # single task against an already-running engine at --url
//!     cargo run -p smooai-smooth-bench -- aider-polyglot --task grade-school
//!
//!     # engine-parity sweep: boot each engine, score it, tear it down
//!     cargo run -p smooai-smooth-bench -- score --engine rust --engine go
//!
//! The `score` command is the engine-parity benchmark (pearl th-4c3e2d):
//! it runs the curated aider-polyglot suite through each of the five
//! smooth-operator LocalServer implementations and emits per-engine
//! (and per-model) scores.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use smooth_bench::curated::CuratedList;
use smooth_bench::engine::{run_engine_matrix, Engine, EngineEnv, EngineMatrixRun, Isolation, MicroVmBooter, ProcessBooter};
use smooth_bench::sweep::{current_commit_sha, StdoutObserver, SweepConfig, SweepGate};
use smooth_bench::{print_summary, run_aider_polyglot, BenchOpts, PolyglotLang};

#[derive(Parser)]
#[command(name = "smooth-bench", version, about = "Smooth engine-parity benchmark harness (internal)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a single Aider Polyglot task against an already-running
    /// engine at `--url`.
    AiderPolyglot {
        /// Task name (e.g. `grade-school`, `leap`, `forth`).
        #[arg(long)]
        task: String,
        /// Language subset. Default: python.
        #[arg(long, default_value = "python")]
        lang: String,
        /// Budget limit in USD for the LLM calls. Default: $5.00.
        #[arg(long, default_value_t = 5.00)]
        budget: f64,
        /// Override the routing model (passed through to the engine).
        #[arg(long)]
        model: Option<String>,
        /// Engine URL. Defaults to http://localhost:4400.
        #[arg(long, default_value = "http://localhost:4400")]
        url: String,
    },

    /// Engine-parity sweep: run the curated aider-polyglot suite through
    /// each selected smooth-operator engine, scoring per engine (and per
    /// model). Boots each engine's LocalServer the way
    /// `scripts/operator-serve.sh` does, runs the tasks, tears it down.
    /// Pearl th-4c3e2d.
    Score(ScoreArgs),
}

#[derive(Parser, Debug)]
struct ScoreArgs {
    /// Which engine(s) to benchmark. Repeatable. Default: all five
    /// (rust, go, ts, python, dotnet).
    #[arg(long = "engine", value_parser = parse_engine)]
    engines: Vec<Engine>,

    /// Which model(s) to run each engine under. Repeatable. Default:
    /// deepseek-v4-flash.
    #[arg(long = "model")]
    models: Vec<String>,

    /// Authoritative sample: every curated task × language. Mutually
    /// exclusive with `--pr`.
    #[arg(long, conflicts_with = "pr")]
    release: bool,

    /// CI-gate sample: `--tasks-per-language` tasks × 6 languages.
    /// Default when neither `--release` nor `--pr` is given.
    #[arg(long)]
    pr: bool,

    /// Tasks per language in the PR gate. Default: 3.
    #[arg(long, default_value_t = 3)]
    tasks_per_language: usize,

    /// Hard USD cap per engine×model cell. When the running cost total
    /// exceeds this, that cell's sweep aborts and emits a partial Score
    /// with `budget_usd_hit: true`.
    #[arg(long, default_value_t = 10.0)]
    budget_usd: f64,

    /// smooth-operator repo root (where the polyglot engine servers
    /// live). Default: $SMOOTH_OPERATOR_REPO or ~/dev/smooai/smooth-operator.
    #[arg(long)]
    repo: Option<PathBuf>,

    /// Per-engine boot timeout in seconds (wait for the port to listen).
    #[arg(long, default_value_t = 300)]
    boot_timeout_s: u64,

    /// Where the engine runs. `host` (default) spawns it as a host
    /// process. `microvm` boots the linux `smooth-daemon` inside a
    /// microsandbox microVM per task — default-deny egress except the
    /// LLM gateway, only the task's scratch dir mounted in. `microvm`
    /// requires `--engine rust`. Pearl th-a63c22.
    #[arg(long, default_value = "host", value_parser = parse_isolation)]
    isolation: Isolation,

    /// Output path. If given, JSON-lines records are written there and
    /// the summary table still prints to stdout; otherwise both go to
    /// stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

fn parse_engine(s: &str) -> Result<Engine, String> {
    Engine::from_name(s).ok_or_else(|| format!("unknown engine {s:?} (valid: rust, go, ts, python, dotnet)"))
}

fn parse_isolation(s: &str) -> Result<Isolation, String> {
    Isolation::from_name(s).ok_or_else(|| format!("unknown isolation {s:?} (valid: host, microvm)"))
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
    }
}

async fn run_score(args: ScoreArgs) -> Result<()> {
    let engines = if args.engines.is_empty() {
        Engine::ALL.to_vec()
    } else {
        args.engines.clone()
    };
    let models = if args.models.is_empty() {
        vec!["deepseek-v4-flash".to_string()]
    } else {
        args.models.clone()
    };

    let gate = if args.release {
        SweepGate::Release
    } else {
        if !args.pr {
            eprintln!(
                "neither --release nor --pr given; defaulting to --pr ({} tasks × 6 langs per engine)",
                args.tasks_per_language
            );
        }
        SweepGate::Pr {
            tasks_per_language: args.tasks_per_language,
        }
    };

    let repo = args
        .repo
        .or_else(|| std::env::var_os("SMOOTH_OPERATOR_REPO").map(PathBuf::from))
        .or_else(|| dirs_next::home_dir().map(|h| h.join("dev").join("smooai").join("smooth-operator")))
        .context("could not resolve smooth-operator repo root")?;

    let env = EngineEnv {
        gateway_url: std::env::var("SMOOAI_GATEWAY_URL").ok(),
        gateway_key: std::env::var("SMOOAI_GATEWAY_KEY").ok(),
        persona: std::env::var("SMOOTH_PERSONA").ok(),
    };
    if env.gateway_key.is_none() {
        eprintln!("warning: SMOOAI_GATEWAY_KEY is unset — engines will boot but turns will error; scores will be all-FAIL");
    }

    let curated = CuratedList::default_embedded().context("loading embedded curated task list")?;
    let cfg = SweepConfig {
        gate,
        budget_usd_cap: args.budget_usd,
        smooth_version: env!("CARGO_PKG_VERSION").to_string(),
        commit_sha: current_commit_sha(),
        task_opts: BenchOpts {
            big_smooth_url: String::new(), // filled per engine by the matrix runner
            budget_usd: Some(args.budget_usd),
            model: None,
        },
    };

    args.isolation.check_engines(&engines)?;

    let mut observer = StdoutObserver;
    let run = match args.isolation {
        Isolation::Host => {
            let mut booter = ProcessBooter::new(repo, env);
            booter.ready_timeout = Duration::from_secs(args.boot_timeout_s);
            run_engine_matrix(&curated, &booter, &engines, &models, &cfg, &mut observer).await?
        }
        Isolation::MicroVm => {
            // The microVM backend needs the SMOOTH repo (for
            // `scripts/msb-spike/`), not the smooth-operator one.
            let smooth_repo = std::env::var_os("SMOOTH_REPO")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(".."));
            let mut booter = MicroVmBooter::new(smooth_repo, env);
            booter.ready_timeout = Duration::from_secs(args.boot_timeout_s);
            run_engine_matrix(&curated, &booter, &engines, &models, &cfg, &mut observer).await?
        }
    };

    emit(&run, args.output.as_deref())?;

    // Non-zero exit if any cell hit its budget cap — CI will notice.
    if run.results.iter().any(|r| r.score.budget_usd_hit) {
        std::process::exit(2);
    }
    Ok(())
}

fn emit(run: &EngineMatrixRun, output: Option<&std::path::Path>) -> Result<()> {
    let jsonl = run.to_jsonl()?;
    match output {
        Some(path) => {
            std::fs::write(path, &jsonl).with_context(|| format!("writing JSON-lines to {}", path.display()))?;
            eprintln!("wrote {}", path.display());
            print!("{}", run.render_summary());
        }
        None => {
            print!("{jsonl}");
            println!();
            print!("{}", run.render_summary());
        }
    }
    Ok(())
}
