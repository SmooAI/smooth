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
use smooth_bench::agentic::{default_scenarios, parse_scenarios, run_agentic, AgenticOpts, AgenticRun, Surface, Verdict};
use smooth_bench::convo::{default_convo_scenarios, gateway_from_providers_json, parse_convo_scenarios, run_convo, ConvoOpts, ConvoRun, ConvoStatus};
use smooth_bench::curated::CuratedList;
use smooth_bench::engine::{run_engine_matrix, Engine, EngineEnv, EngineMatrixRun, Isolation, MicroVmBooter, ProcessBooter, WorkspaceBooter};
use smooth_bench::leaderboard;
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

    /// Agentic / workflow benchmark: does the agent take the right
    /// ACTIONS through a multi-step tool workflow? Each scenario seeds a
    /// workspace, boots the engine rooted there, drives one turn with the
    /// user's goal, and scores the resulting state (deterministic
    /// assertions, or an LLM judge for open-ended goals).
    ///
    /// Scenarios never touch real services: the default microVM isolation
    /// denies all egress except the LLM gateway, and every "external
    /// system" is a JSON state file in the mounted workspace.
    /// Pearl th-300d7d.
    Agentic(AgenticArgs),

    /// Agentic CONVERSATION benchmark: an LLM plays a user across several
    /// turns against a live Big Smooth, and an LLM judge scores the whole
    /// thread — helpfulness, correctness, tool use, and consistency
    /// across turns.
    ///
    /// This is the suite that catches what a single-turn bench can't:
    /// contradictory answers, stale replies to a superseded question,
    /// confident claims with no tool call behind them.
    ///
    /// Slow, networked, and it costs money — deliberately NOT part of
    /// `cargo test`. Run it explicitly:
    ///
    ///     SMOOAI_GATEWAY_KEY=… cargo run -p smooai-smooth-bench -- convo
    ///
    /// Pearl th-f19853.
    Convo(ConvoArgs),
}

#[derive(Parser, Debug)]
struct ConvoArgs {
    /// Target an already-running Big Smooth instead of spawning one
    /// (e.g. `http://127.0.0.1:8788`).
    #[arg(long)]
    url: Option<String>,

    /// Auth token for `--url` (the daemon runs strict-auth). Defaults to
    /// $SMOOTH_LOCAL_TOKEN.
    #[arg(long)]
    token: Option<String>,

    /// Model(s) the spawned daemon runs under. Repeatable — each one
    /// gets its own daemon and the suite prints a leaderboard across
    /// them. Ignored with `--url` (that daemon owns its own routing);
    /// still recorded in the results. Default: deepseek-v4-flash.
    #[arg(long = "model")]
    models: Vec<String>,

    /// Model that plays the user.
    #[arg(long, default_value = "deepseek-v4-flash")]
    driver_model: String,

    /// Model that grades the conversation.
    #[arg(long, default_value = "deepseek-v4-flash")]
    judge_model: String,

    /// Scenario TOML to run instead of the embedded suite.
    #[arg(long)]
    scenarios: Option<PathBuf>,

    /// Run only the scenario(s) with these ids. Repeatable.
    #[arg(long = "only")]
    only: Vec<String>,

    /// Run each scenario N times — conversations are stochastic.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    trials: u32,

    /// Per-TURN deadline in seconds. Deliberately short: a turn that
    /// takes many minutes is itself the bug this suite hunts.
    #[arg(long, default_value_t = 180)]
    turn_timeout_s: u64,

    /// Boot timeout in seconds when spawning the daemon.
    #[arg(long, default_value_t = 300)]
    boot_timeout_s: u64,

    /// Write a publishable scoreboard (per-model pass %, cost, $/pass)
    /// as JSON here. Feed it to scripts/the-line/render-model-scores.sh
    /// to update docs/model-scores.json + the README badge.
    #[arg(long)]
    scoreboard: Option<PathBuf>,

    /// Write JSON-lines transcripts here; the table still prints to
    /// stdout. Defaults to `<run dir>/transcripts.jsonl`.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct AgenticArgs {
    /// Engine to benchmark. Default: rust (the only engine that ships
    /// tools, and the only one with a VM-bootable binary).
    #[arg(long, default_value = "rust", value_parser = parse_engine)]
    engine: Engine,

    /// Model(s) the agent runs under. Repeatable — pass it more than
    /// once and the suite runs per model and prints a leaderboard plus a
    /// scenario × model grid. Default: deepseek-v4-flash.
    #[arg(long = "model")]
    models: Vec<String>,

    /// Where the engine runs. Default `microvm` — scenarios mutate a
    /// workspace and must not be able to reach anything real.
    #[arg(long, default_value = "microvm", value_parser = parse_isolation)]
    isolation: Isolation,

    /// Which CLIENT drives the turn. `daemon` (default) speaks the
    /// canonical protocol directly, as the Big Smooth PWA does;
    /// `thcode` drives through smooth-code's own client, the codepath
    /// `th code --headless` runs. Same engine either way — the delta is
    /// attributable to `th code`'s layer. Pearl th-b3fe81.
    #[arg(long, default_value = "daemon", value_parser = parse_surface)]
    surface: Surface,

    /// Cheap model used to grade `kind = "judge"` scenarios.
    #[arg(long, default_value = "deepseek-v4-flash")]
    judge_model: String,

    /// Scenario TOML to run instead of the embedded suite.
    #[arg(long)]
    scenarios: Option<PathBuf>,

    /// Run only the scenario(s) with these ids. Repeatable.
    #[arg(long = "only")]
    only: Vec<String>,

    /// Run each scenario N times and report a pass RATE instead of a
    /// single anecdote — agent behaviour is stochastic. Trials run
    /// sequentially (one microVM + port at a time), each in its own
    /// freshly seeded work dir. Default 1.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    trials: u32,

    /// smooth-operator repo root (host isolation only — where the
    /// polyglot engine servers live).
    #[arg(long)]
    repo: Option<PathBuf>,

    /// Per-scenario boot timeout in seconds.
    #[arg(long, default_value_t = 300)]
    boot_timeout_s: u64,

    /// Write a publishable scoreboard (per-model pass %, cost, $/pass)
    /// as JSON here. Feed it to scripts/the-line/render-model-scores.sh
    /// to update docs/model-scores.json + the README badge.
    #[arg(long)]
    scoreboard: Option<PathBuf>,

    /// Write JSON-lines here; the table still prints to stdout.
    #[arg(long)]
    output: Option<PathBuf>,
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

fn parse_surface(s: &str) -> Result<Surface, String> {
    Surface::from_name(s).ok_or_else(|| format!("unknown surface {s:?} (valid: daemon, thcode)"))
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
        Commands::Agentic(args) => run_agentic_cmd(args).await,
        Commands::Convo(args) => run_convo_cmd(args).await,
    }
}

async fn run_convo_cmd(args: ConvoArgs) -> Result<()> {
    let mut scenarios = match &args.scenarios {
        Some(p) => {
            let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            parse_convo_scenarios(&text)?
        }
        None => default_convo_scenarios()?,
    };
    if !args.only.is_empty() {
        scenarios.retain(|s| args.only.contains(&s.id));
        anyhow::ensure!(!scenarios.is_empty(), "--only matched no scenarios (have: {:?})", args.only);
    }

    // The driver and the judge are LLM calls of our own — without a key
    // there is no conversation to hold and nothing to grade. Fall back to
    // ~/.smooth/providers.json, the same store the daemon reads, so the
    // suite runs without exporting anything by hand.
    let mut env = EngineEnv {
        gateway_url: std::env::var("SMOOAI_GATEWAY_URL").ok(),
        gateway_key: std::env::var("SMOOAI_GATEWAY_KEY").ok(),
        persona: std::env::var("SMOOTH_PERSONA").ok(),
    };
    if env.gateway_key.is_none() {
        let (url, key) = gateway_from_providers_json()
            .context("no SMOOAI_GATEWAY_KEY and no OpenAI-compatible provider in ~/.smooth/providers.json — the driver and judge cannot run")?;
        eprintln!("convo: using the {url} provider from ~/.smooth/providers.json");
        env.gateway_url.get_or_insert(url);
        env.gateway_key = Some(key);
    }

    let models = if args.models.is_empty() {
        vec!["deepseek-v4-flash".to_string()]
    } else {
        args.models.clone()
    };
    // `--url` targets a daemon that already exists and owns its own
    // routing — we cannot make it change model between runs, so several
    // models would silently produce N identical rows.
    anyhow::ensure!(
        models.len() == 1 || args.url.is_none(),
        "--url targets an existing daemon whose routing we don't control; pass a single --model, or drop --url so each model gets its own daemon"
    );

    // convo always drives the Rust daemon, spawned or via --url.
    eprintln!("{}", smooth_bench::provenance::Provenance::detect().render());

    let run_root = smooth_bench::runs_root()?.join(format!("convo-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
    std::fs::create_dir_all(&run_root).with_context(|| format!("mkdir {}", run_root.display()))?;
    eprintln!("convo: scratch at {}", run_root.display());

    let gateway_url = env.gateway_url.clone().unwrap_or_else(|| "https://llm.smoo.ai/v1".to_string());
    let gateway_key = env.gateway_key.clone();

    // Sample how fast the SHARED key is spending on traffic that isn't
    // ours, before anything of ours is in flight. Without this the cost
    // column happily ranked gpt-5.5 below deepseek-v4-flash.
    let noise = match gateway_key.as_deref() {
        Some(k) => {
            eprintln!("cost: sampling the key's background spend for 10s …");
            smooth_bench::spend::NoiseFloor::sample(&gateway_url, k, std::time::Duration::from_secs(10))
                .await
                .map_err(|e| eprintln!("cost: could not sample background spend — {e:#}"))
                .ok()
        }
        None => None,
    };

    let mut runs = Vec::with_capacity(models.len());
    for model in &models {
        if models.len() > 1 {
            eprintln!("\nconvo: === model {model} ===");
        }
        // A fresh workspace per model: the daemon writes memories and
        // session state into its root, and model B inheriting model A's
        // memories is not a comparison.
        let work = if models.len() > 1 {
            run_root.join(slugify(model)).join("work")
        } else {
            run_root.join("work")
        };
        std::fs::create_dir_all(&work).with_context(|| format!("mkdir {}", work.display()))?;

        // Either drive a daemon that's already up, or spawn one rooted at
        // the scratch workspace (guard kept alive so it's torn down after).
        let (url, token, _guard) = match &args.url {
            Some(u) => (u.clone(), args.token.clone().or_else(|| std::env::var("SMOOTH_LOCAL_TOKEN").ok()), None),
            None => {
                eprintln!("convo: spawning a Big Smooth daemon (workspace {}) …", work.display());
                let mut booter = ProcessBooter::new(smooth_repo_root(), env.clone());
                booter.ready_timeout = Duration::from_secs(args.boot_timeout_s);
                let booted = booter
                    .boot_workspace(Engine::Rust, model, &work, &run_root.join("log"))
                    .await
                    .context("spawning the Big Smooth daemon (is `th` on PATH?)")?;
                (booted.url.clone(), booted.token.clone(), Some(booted))
            }
        };
        eprintln!("convo: target {url}");

        let opts = ConvoOpts {
            url,
            token,
            model: model.clone(),
            driver_model: args.driver_model.clone(),
            judge_model: args.judge_model.clone(),
            gateway_url: gateway_url.clone(),
            gateway_key: gateway_key.clone(),
            trials: args.trials as usize,
            turn_deadline: Duration::from_secs(args.turn_timeout_s),
        };
        let (run, measured) = smooth_bench::spend::measure(&gateway_url, gateway_key.as_deref(), run_convo(&scenarios, &opts)).await;
        runs.push((run, measured));
        // `_guard` drops here — the daemon is torn down before the next
        // model boots, so they never contend for the port.
    }

    let out_path = args.output.unwrap_or_else(|| run_root.join("transcripts.jsonl"));
    let mut jsonl = String::new();
    for (run, _) in &runs {
        jsonl.push_str(&run.to_jsonl()?);
    }
    std::fs::write(&out_path, jsonl).with_context(|| format!("writing transcripts to {}", out_path.display()))?;
    for (run, _) in &runs {
        print!("{}", run.render_table());
    }
    let rows: Vec<_> = runs.iter().map(|(r, m)| convo_row(r, *m, noise)).collect();
    if runs.len() > 1 {
        print!("{}", leaderboard::render("convo", &rows));
    }
    write_scoreboard(args.scoreboard.as_deref(), "convo", args.trials as usize, &rows)?;
    print_cost_note(&runs.iter().map(|(_, m)| *m).collect::<Vec<_>>(), noise);
    eprintln!("convo: transcripts at {}", out_path.display());

    // Non-zero on FAIL, INCONCLUSIVE, or XPASS — an expected-fail
    // scenario that starts passing means the flag is now a lie.
    if runs.iter().any(|(r, _)| !r.suite_ok()) {
        std::process::exit(1);
    }
    Ok(())
}

/// Tool usage across a convo run: the judge's `tool_use` axis plus the
/// objective counts the transcript already carries.
///
/// The objective half matters because the judged half can be generous —
/// a model that reached the right answer after twelve calls and four
/// errors can still read as good tool use to a grader looking at the
/// outcome. Error rate and calls-per-turn do not flatter anyone.
fn convo_tool_usage(run: &ConvoRun) -> leaderboard::ToolUsage {
    let mut u = leaderboard::ToolUsage::default();
    let mut graded = Vec::new();
    for r in &run.results {
        if let Some(s) = &r.scores {
            graded.push(f64::from(s.tool_use));
        }
        for t in &r.turns {
            u.turns += 1;
            if t.tools.is_empty() {
                u.turns_without_tools += 1;
            }
            u.calls += t.tools.len();
            // Recorded as "name -> ok" / "name -> error".
            u.errors += t.tools.iter().filter(|c| c.ends_with("error")).count();
        }
    }
    #[allow(clippy::cast_precision_loss, reason = "trial counts are small")]
    if !graded.is_empty() {
        u.judge_score = Some(graded.iter().sum::<f64>() / graded.len() as f64);
    }
    u
}

/// Flatten one convo run into a leaderboard row.
///
/// `XFAIL` maps to a fail cell: the documented gap is genuinely not
/// solved, and a green suite shouldn't inflate a model's pass rate.
/// `XPASS` maps to a pass — that model really did clear the bar.
fn convo_row(run: &ConvoRun, measured: Option<smooth_bench::spend::Measured>, noise: Option<smooth_bench::spend::NoiseFloor>) -> leaderboard::ModelRow {
    let mut cells = Vec::with_capacity(run.results.len());
    let (mut passed, mut conclusive, mut inconclusive) = (0usize, 0usize, 0usize);
    for r in &run.results {
        let cell = match r.status {
            ConvoStatus::Pass | ConvoStatus::XPass => {
                passed += 1;
                conclusive += 1;
                leaderboard::Cell::Pass
            }
            ConvoStatus::Fail => {
                conclusive += 1;
                leaderboard::Cell::Fail
            }
            ConvoStatus::XFail => {
                conclusive += 1;
                leaderboard::Cell::KnownGap
            }
            ConvoStatus::Inconclusive => {
                inconclusive += 1;
                leaderboard::Cell::Inconclusive
            }
        };
        cells.push((r.id.clone(), cell));
    }
    #[allow(clippy::cast_precision_loss, reason = "scenario counts are single digits")]
    let pass_rate = if conclusive == 0 { 0.0 } else { passed as f64 / conclusive as f64 };
    let duration_ms = run.results.iter().map(|r| r.duration_ms).sum();
    let tools = convo_tool_usage(run);
    leaderboard::ModelRow {
        model: run.model.clone(),
        pass_rate,
        tools,
        passed,
        conclusive,
        inconclusive,
        cost_usd: measured.map_or_else(|| run.results.iter().map(|r| r.cost_usd).sum(), smooth_bench::spend::Measured::agent),
        cost_resolvable: measured.is_some_and(|m| m.is_resolvable(noise, duration_ms)),
        duration_ms,
        cells,
    }
}

/// Resolve the smooth repo root (holds `scripts/msb-spike/`) for the
/// microVM booter. Same resolution `score --isolation microvm` uses.
fn smooth_repo_root() -> PathBuf {
    std::env::var_os("SMOOTH_REPO").map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(".."), PathBuf::from)
}

async fn run_agentic_cmd(args: AgenticArgs) -> Result<()> {
    args.isolation.check_engines(&[args.engine])?;

    let mut scenarios = match &args.scenarios {
        Some(p) => {
            let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            parse_scenarios(&text)?
        }
        None => default_scenarios()?,
    };
    if !args.only.is_empty() {
        scenarios.retain(|s| args.only.contains(&s.id));
        anyhow::ensure!(!scenarios.is_empty(), "--only matched no scenarios (have: {:?})", args.only);
    }

    let env = EngineEnv {
        gateway_url: std::env::var("SMOOAI_GATEWAY_URL").ok(),
        gateway_key: std::env::var("SMOOAI_GATEWAY_KEY").ok(),
        persona: std::env::var("SMOOTH_PERSONA").ok(),
    };
    if env.gateway_key.is_none() {
        eprintln!("warning: SMOOAI_GATEWAY_KEY is unset — the agent will boot but every turn errors; scenarios will be INCONCLUSIVE");
    }

    // Say which binary is about to be benchmarked. The Rust engine boots
    // `th daemon` from PATH and nothing rebuilds it, so a run can silently
    // score a months-old build (th-cdf7b8).
    if args.engine == Engine::Rust {
        let prov = smooth_bench::provenance::Provenance::detect();
        eprintln!("{}", prov.render());
    }

    let run_root = smooth_bench::runs_root()?.join(format!("agentic-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
    std::fs::create_dir_all(&run_root).with_context(|| format!("mkdir {}", run_root.display()))?;
    eprintln!("agentic: scratch at {}", run_root.display());

    let gateway_url = env.gateway_url.clone().unwrap_or_else(|| "https://llm.smoo.ai/v1".to_string());
    let gateway_key = env.gateway_key.clone();

    let models = if args.models.is_empty() {
        vec!["deepseek-v4-flash".to_string()]
    } else {
        args.models.clone()
    };

    let booter: Box<dyn WorkspaceBooter> = match args.isolation {
        Isolation::Host => {
            let repo = args
                .repo
                .clone()
                .or_else(|| std::env::var_os("SMOOTH_OPERATOR_REPO").map(PathBuf::from))
                .or_else(|| dirs_next::home_dir().map(|h| h.join("dev").join("smooai").join("smooth-operator")))
                .context("could not resolve smooth-operator repo root")?;
            let mut b = ProcessBooter::new(repo, env);
            b.ready_timeout = Duration::from_secs(args.boot_timeout_s);
            Box::new(b)
        }
        Isolation::MicroVm => {
            let mut b = MicroVmBooter::new(smooth_repo_root(), env);
            b.ready_timeout = Duration::from_secs(args.boot_timeout_s);
            Box::new(b)
        }
    };

    // Sample how fast the SHARED key is spending on traffic that isn't
    // ours, before anything of ours is in flight. Without this the cost
    // column happily ranked gpt-5.5 below deepseek-v4-flash.
    let noise = match gateway_key.as_deref() {
        Some(k) => {
            eprintln!("cost: sampling the key's background spend for 10s …");
            smooth_bench::spend::NoiseFloor::sample(&gateway_url, k, std::time::Duration::from_secs(10))
                .await
                .map_err(|e| eprintln!("cost: could not sample background spend — {e:#}"))
                .ok()
        }
        None => None,
    };

    // One suite run per model, sequentially: every trial boots an engine
    // and grabs a port, so overlapping models would fight over both.
    let mut runs = Vec::with_capacity(models.len());
    for model in &models {
        if models.len() > 1 {
            eprintln!("\nagentic: === model {model} ===");
        }
        let opts = AgenticOpts {
            engine: args.engine,
            // Each model gets its own scratch subtree — otherwise model B
            // would score against model A's leftover workspace.
            runs_root: if models.len() > 1 { run_root.join(slugify(model)) } else { run_root.clone() },
            model: model.clone(),
            surface: args.surface,
            isolation: args.isolation,
            judge_model: args.judge_model.clone(),
            gateway_url: gateway_url.clone(),
            gateway_key: gateway_key.clone(),
            trials: args.trials as usize,
        };
        let (run, measured) = smooth_bench::spend::measure(&gateway_url, gateway_key.as_deref(), run_agentic(&scenarios, booter.as_ref(), &opts)).await;
        runs.push((run?, measured));
    }

    let mut jsonl = String::new();
    for (run, _) in &runs {
        jsonl.push_str(&run.to_jsonl()?);
    }
    if let Some(path) = args.output.as_deref() {
        std::fs::write(path, &jsonl).with_context(|| format!("writing JSON-lines to {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    } else {
        print!("{jsonl}");
    }
    println!();
    for (run, _) in &runs {
        print!("{}", run.render_table());
    }
    let rows: Vec<_> = runs.iter().map(|(r, m)| agentic_row(r, *m, noise)).collect();
    if runs.len() > 1 {
        print!("{}", leaderboard::render("agentic", &rows));
    }
    write_scoreboard(args.scoreboard.as_deref(), "agentic", args.trials as usize, &rows)?;
    print_cost_note(&runs.iter().map(|(_, m)| *m).collect::<Vec<_>>(), noise);

    // Non-zero when any scenario didn't pass every conclusive trial — CI
    // (and a human) should notice an INCONCLUSIVE or a FLAKY just as much
    // as a FAIL. With several models, any model falling short fails the run.
    if runs.iter().any(|(r, _)| r.passed() < r.scenario_count()) {
        std::process::exit(1);
    }
    Ok(())
}

/// Write the publishable scoreboard, if `--scoreboard` asked for one.
///
/// Separate from the JSONL transcripts on purpose: those are the raw
/// record, this is the artefact other things parse (the docs table, the
/// README badge). Keeping it small and pre-rounded means the badge, the
/// table and the JSON can never disagree with each other.
fn write_scoreboard(path: Option<&std::path::Path>, suite: &str, trials: usize, rows: &[leaderboard::ModelRow]) -> Result<()> {
    let Some(path) = path else { return Ok(()) };
    let board = leaderboard::Scoreboard::from_rows(suite, trials, rows);
    let json = serde_json::to_string_pretty(&board).context("serialising the scoreboard")?;
    std::fs::write(path, json + "\n").with_context(|| format!("writing {}", path.display()))?;
    eprintln!("scoreboard: wrote {}", path.display());
    Ok(())
}

/// Flatten one agentic run into a leaderboard row.
fn agentic_row(run: &AgenticRun, measured: Option<smooth_bench::spend::Measured>, noise: Option<smooth_bench::spend::NoiseFloor>) -> leaderboard::ModelRow {
    let cells = run
        .aggregates()
        .iter()
        .map(|a| {
            let cell = match a.verdict {
                Verdict::Pass => leaderboard::Cell::Pass,
                Verdict::Fail => leaderboard::Cell::Fail,
                Verdict::Inconclusive => leaderboard::Cell::Inconclusive,
            };
            (a.id.clone(), cell)
        })
        .collect();
    let duration_ms = run.results.iter().map(|r| r.duration_ms).sum();
    leaderboard::ModelRow {
        model: run.model.clone(),
        pass_rate: run.pass_rate(),
        passed: run.passed(),
        conclusive: run.conclusive(),
        inconclusive: run.inconclusive(),
        // Prefer the gateway-measured figure: the protocol reports $0
        // until th-11f9bb lands, and the polyglot engines never report
        // cost at all.
        cost_usd: measured.map_or_else(|| run.total_cost_usd(), smooth_bench::spend::Measured::agent),
        cost_resolvable: measured.is_some_and(|m| m.is_resolvable(noise, duration_ms)),
        duration_ms,
        // The agentic runner records tool NAMES per trial, not per-call
        // outcomes, so an error rate is unavailable here — reported as 0
        // rather than guessed. One turn per trial.
        tools: leaderboard::ToolUsage {
            calls: run.results.iter().map(|r| r.tools.len()).sum(),
            errors: 0,
            judge_score: None,
            turns_without_tools: run.results.iter().filter(|r| r.tools.is_empty()).count(),
            turns: run.results.len(),
        },
        cells,
    }
}

/// Say where the cost numbers came from — or that there are none.
fn print_cost_note(measured: &[Option<smooth_bench::spend::Measured>], noise: Option<smooth_bench::spend::NoiseFloor>) {
    use smooth_bench::spend::Measured;
    if measured.iter().all(Option::is_none) {
        println!("\n  cost: not measured (no gateway key, or /key/info unreachable).");
        return;
    }
    let harness: f64 = measured.iter().flatten().map(|m| m.harness).sum();
    println!("\n  cost: {}", Measured::caveat());
    println!("  harness (driver + judge) excluded from the per-model figures: ${harness:.4}");
    if let Some(n) = noise {
        println!(
            "  background spend on this key: ${:.6}/s — a 60s run carries ~${:.4} that is not ours.",
            n.usd_per_second,
            n.over(60_000)
        );
        println!("  figures shown as `<noise` are below twice that and are NOT a measurement; use a dedicated key, or a longer run.");
    }
}

/// Filesystem-safe form of a model id, for per-model scratch subtrees.
/// Model ids carry `/` and `:` (`anthropic/claude-opus-4-8`), which would
/// otherwise silently create nested directories.
fn slugify(model: &str) -> String {
    model
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '-' })
        .collect()
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
            let mut booter = MicroVmBooter::new(smooth_repo_root(), env);
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
