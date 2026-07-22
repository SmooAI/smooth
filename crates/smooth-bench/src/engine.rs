//! Engine-parity axis for the aider-polyglot sweep.
//!
//! There are five smooth-operator `LocalServer` implementations —
//! Rust, Go, TypeScript, Python, .NET — all speaking the same canonical
//! WebSocket protocol (the one [`crate::chat_driver`] drives). This
//! module boots each engine the way `scripts/operator-serve.sh` does
//! (pearl th-3f46fd), runs the curated aider-polyglot sweep against it,
//! tears it down, and tags the resulting [`Score`] with the engine +
//! model it was produced under.
//!
//! The matrix runner is parameterised on an [`EngineBooter`] trait so
//! the engine×model aggregation can be unit-tested with a fake booter +
//! canned [`TaskRunner`] — no live LLM, no real servers. The production
//! [`ProcessBooter`] spawns the real engine and waits for its port.

use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Serialize;

use crate::curated::CuratedList;
use crate::score::Score;
use crate::sweep::{run_sweep, PolyglotTaskRunner, SweepConfig, SweepObserver, SweepRun, TaskRunner};

/// The five polyglot smooth-operator engine implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Rust,
    Go,
    Ts,
    Python,
    Dotnet,
}

impl Engine {
    /// All engines, in the canonical order `operator-serve.sh smoke` uses.
    pub const ALL: [Self; 5] = [Self::Rust, Self::Go, Self::Ts, Self::Python, Self::Dotnet];

    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "rust" | "rs" => Some(Self::Rust),
            "go" | "golang" => Some(Self::Go),
            "ts" | "typescript" | "node" => Some(Self::Ts),
            "python" | "py" => Some(Self::Python),
            "dotnet" | ".net" | "csharp" | "cs" => Some(Self::Dotnet),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Ts => "ts",
            Self::Python => "python",
            Self::Dotnet => "dotnet",
        }
    }

    /// Default bind port. Python's bind is hardcoded to 8787 upstream —
    /// `operator-serve.sh` ignores the port argument for it — so we pin
    /// the same value here. The others get distinct ports so a matrix
    /// run doesn't collide when engines are booted back-to-back.
    pub fn default_port(self) -> u16 {
        match self {
            Self::Rust => 8791,
            Self::Go => 8792,
            Self::Ts => 8793,
            Self::Python => 8787,
            Self::Dotnet => 8795,
        }
    }

    /// The spawn recipe for this engine's LocalServer at `port`, rooted
    /// at the smooth-operator `repo`. Pure data — mirrors the `serve()`
    /// cases in `scripts/operator-serve.sh` so the mapping is unit-
    /// testable without spawning anything. Gateway / persona / model
    /// env is layered on at spawn time (see [`ProcessBooter`]), not here.
    pub fn boot_command(self, repo: &Path, port: u16) -> BootCommand {
        let bind = format!("127.0.0.1:{port}");
        match self {
            // The one runnable Rust LocalServer is the daemon itself.
            Self::Rust => BootCommand {
                program: "th".into(),
                args: vec!["daemon".into()],
                cwd: None,
                bind_env: vec![("SMOOTH_ADDR".into(), bind)],
            },
            Self::Go => BootCommand {
                program: "go".into(),
                args: vec!["run".into(), "./cmd/serve".into()],
                cwd: Some(repo.join("go").join("server")),
                bind_env: vec![("SMOOTH_OPERATOR_BIND".into(), bind)],
            },
            Self::Ts => BootCommand {
                program: "node".into(),
                args: vec!["dist/main.js".into()],
                cwd: Some(repo.join("typescript").join("server")),
                bind_env: vec![
                    ("SMOOTH_OPERATOR_HOST".into(), "127.0.0.1".into()),
                    ("SMOOTH_OPERATOR_PORT".into(), port.to_string()),
                ],
            },
            Self::Python => BootCommand {
                program: "uv".into(),
                args: vec!["run".into(), "python".into(), "-m".into(), "smooth_operator_server".into()],
                cwd: Some(repo.join("python").join("server")),
                // Bind is hardcoded 127.0.0.1:8787 upstream — no bind env.
                bind_env: vec![],
            },
            Self::Dotnet => BootCommand {
                program: "dotnet".into(),
                args: vec!["run".into()],
                cwd: Some(repo.join("dotnet").join("server").join("host")),
                bind_env: vec![("ASPNETCORE_URLS".into(), format!("http://{bind}"))],
            },
        }
    }
}

/// A fully-resolved spawn recipe. `program` + `args` run in `cwd` with
/// `bind_env` (plus the shared gateway/persona/model env) in the
/// environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub bind_env: Vec<(String, String)>,
}

/// The LLM-gateway + persona env every engine reads, per the uniform
/// contract in `operator-serve.sh`. `SMOOAI_MODEL` is passed per-boot
/// (it varies across the matrix), not here.
#[derive(Debug, Clone, Default)]
pub struct EngineEnv {
    pub gateway_url: Option<String>,
    pub gateway_key: Option<String>,
    pub persona: Option<String>,
}

/// A booted engine: a [`TaskRunner`] pointed at it, its base URL, and a
/// teardown guard that kills the process (and its children) on drop.
pub struct BootedEngine {
    pub runner: Box<dyn TaskRunner>,
    pub url: String,
    // Dropped after the sweep → tears the engine down. `()` for fakes.
    _guard: Box<dyn Send + Sync>,
}

/// Injection point for booting an engine. Production ([`ProcessBooter`])
/// spawns the real server; tests provide a fake that returns a canned
/// runner so the matrix aggregation is exercised without a live LLM.
#[async_trait]
pub trait EngineBooter: Send + Sync {
    async fn boot(&self, engine: Engine, model: &str) -> Result<BootedEngine>;
}

/// Kills a spawned engine (and its child processes) when dropped.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // `go run` / `dotnet run` spawn a child that holds the socket —
        // kill the whole tree, matching `operator-serve.sh`'s cleanup.
        let pid = self.0.id();
        let _ = Command::new("pkill").arg("-P").arg(pid.to_string()).status();
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Production booter: spawns the real engine LocalServer and waits for
/// its port to accept TCP (a listening socket = booted; a strict-auth
/// 401 on GET still counts as listening).
pub struct ProcessBooter {
    pub repo: PathBuf,
    pub env: EngineEnv,
    pub ready_timeout: Duration,
}

impl ProcessBooter {
    pub fn new(repo: PathBuf, env: EngineEnv) -> Self {
        Self {
            repo,
            env,
            ready_timeout: Duration::from_secs(300),
        }
    }
}

#[async_trait]
impl EngineBooter for ProcessBooter {
    async fn boot(&self, engine: Engine, model: &str) -> Result<BootedEngine> {
        let port = engine.default_port();
        let cmd = engine.boot_command(&self.repo, port);
        prepare_engine(engine, &cmd)?;

        let mut command = Command::new(&cmd.program);
        command.args(&cmd.args);
        if let Some(cwd) = &cmd.cwd {
            command.current_dir(cwd);
        }
        for (k, v) in &cmd.bind_env {
            command.env(k, v);
        }
        command.env("SMOOAI_MODEL", model);
        if let Some(u) = &self.env.gateway_url {
            command.env("SMOOAI_GATEWAY_URL", u);
        }
        if let Some(k) = &self.env.gateway_key {
            command.env("SMOOAI_GATEWAY_KEY", k);
        }
        if let Some(p) = &self.env.persona {
            command.env("SMOOTH_PERSONA", p);
        }
        // New process group so we can reap `go run` / `dotnet run` children.
        command.process_group(0);
        command.stdout(Stdio::null()).stderr(Stdio::null());

        let child = command
            .spawn()
            .with_context(|| format!("spawning {} engine ({} {:?})", engine.as_str(), cmd.program, cmd.args))?;
        let guard = KillOnDrop(child);

        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().context("engine bind addr")?;
        wait_for_port(addr, self.ready_timeout).with_context(|| format!("{} engine never opened {addr}", engine.as_str()))?;

        Ok(BootedEngine {
            runner: Box::new(PolyglotTaskRunner),
            url: format!("http://127.0.0.1:{port}"),
            _guard: Box::new(guard),
        })
    }
}

/// Best-effort pre-spawn prep the shell script does inline: the TS
/// server needs a `dist/` build, the Python server a synced venv.
fn prepare_engine(engine: Engine, cmd: &BootCommand) -> Result<()> {
    let Some(cwd) = &cmd.cwd else { return Ok(()) };
    match engine {
        Engine::Ts if !cwd.join("dist").join("main.js").exists() => {
            run_prep(cwd, "pnpm", &["install", "--silent"])?;
            run_prep(cwd, "pnpm", &["build"])?;
        }
        Engine::Python => {
            run_prep(cwd, "uv", &["sync", "--quiet"])?;
        }
        _ => {}
    }
    Ok(())
}

fn run_prep(cwd: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("running prep `{program} {}`", args.join(" ")))?;
    anyhow::ensure!(status.success(), "prep `{program} {}` failed", args.join(" "));
    Ok(())
}

/// Poll a TCP connect until it succeeds or `timeout` elapses.
fn wait_for_port(addr: SocketAddr, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    anyhow::bail!("timed out after {timeout:?} waiting for {addr}")
}

/// One engine×model sweep result — the [`Score`] flattened onto its
/// engine + model provenance. Serialised as one JSON-lines record.
#[derive(Debug, Clone, Serialize)]
pub struct EngineScore {
    pub engine: String,
    pub model: String,
    #[serde(flatten)]
    pub score: Score,
}

/// The full engine×model matrix run.
#[derive(Debug, Clone, Serialize)]
pub struct EngineMatrixRun {
    pub results: Vec<EngineScore>,
}

impl EngineMatrixRun {
    /// One JSON object per line (JSON-lines) — the streaming record format.
    ///
    /// # Errors
    /// Propagates a `serde_json` failure if a record can't be serialised.
    pub fn to_jsonl(&self) -> Result<String> {
        let mut out = String::new();
        for r in &self.results {
            out.push_str(&serde_json::to_string(r).context("serialising EngineScore")?);
            out.push('\n');
        }
        Ok(out)
    }

    /// Human summary: one row per engine×model with the headline numbers.
    pub fn render_summary(&self) -> String {
        let mut out = String::from("engine   model                pass    green/att   $cost\n");
        for r in &self.results {
            out.push_str(&format!(
                "{engine:<8} {model:<20} {rate:>5.1}%  {green:>3}/{att:<3}    ${cost:.4}\n",
                engine = r.engine,
                model = r.model,
                rate = r.score.overall_pass_rate * 100.0,
                green = r.score.tasks_green,
                att = r.score.tasks_attempted,
                cost = r.score.cost_usd,
            ));
        }
        out
    }
}

/// Run the curated aider-polyglot sweep against every `engine` × `model`
/// combination. Each cell boots the engine via `booter`, points the
/// sweep's `task_opts.big_smooth_url` (and model) at it, runs the sweep,
/// and tears the engine down before the next cell. A boot failure for
/// one cell is recorded and skipped — one dead engine doesn't abort the
/// matrix.
///
/// # Errors
/// Propagates only if the observer or aggregation itself fails; per-cell
/// boot/transport failures are surfaced via `eprintln!` and skipped.
pub async fn run_engine_matrix<B, O>(
    curated: &CuratedList,
    booter: &B,
    engines: &[Engine],
    models: &[String],
    sweep_cfg: &SweepConfig,
    observer: &mut O,
) -> Result<EngineMatrixRun>
where
    B: EngineBooter,
    O: SweepObserver,
{
    let mut results = Vec::with_capacity(engines.len() * models.len());
    for &engine in engines {
        for model in models {
            let booted = match booter.boot(engine, model).await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("engine {} model {model}: boot failed, skipping: {e:#}", engine.as_str());
                    continue;
                }
            };
            let mut cfg = sweep_cfg.clone();
            cfg.task_opts.big_smooth_url = booted.url.clone();
            cfg.task_opts.model = Some(model.clone());
            let SweepRun { score, per_task: _ } = run_sweep(curated, booted.runner.as_ref(), &cfg, observer).await?;
            results.push(EngineScore {
                engine: engine.as_str().to_string(),
                model: model.clone(),
                score,
            });
            // `booted` drops here → engine torn down before the next cell.
        }
    }
    Ok(EngineMatrixRun { results })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::sweep::{SweepGate, TaskOutcome};
    use crate::{BenchOpts, PolyglotLang};

    #[test]
    fn engine_from_name_and_as_str_round_trip() {
        for e in Engine::ALL {
            assert_eq!(Engine::from_name(e.as_str()), Some(e));
        }
        assert_eq!(Engine::from_name("golang"), Some(Engine::Go));
        assert_eq!(Engine::from_name("typescript"), Some(Engine::Ts));
        assert_eq!(Engine::from_name(".net"), Some(Engine::Dotnet));
        assert_eq!(Engine::from_name("nonsense"), None);
    }

    #[test]
    fn boot_command_mapping_matches_operator_serve() {
        let repo = Path::new("/repo");

        let rust = Engine::Rust.boot_command(repo, 8791);
        assert_eq!(rust.program, "th");
        assert_eq!(rust.args, ["daemon"]);
        assert_eq!(rust.cwd, None);
        assert_eq!(rust.bind_env, [("SMOOTH_ADDR".to_string(), "127.0.0.1:8791".to_string())]);

        let go = Engine::Go.boot_command(repo, 8792);
        assert_eq!(go.program, "go");
        assert_eq!(go.args, ["run", "./cmd/serve"]);
        assert_eq!(go.cwd, Some(PathBuf::from("/repo/go/server")));
        assert_eq!(go.bind_env, [("SMOOTH_OPERATOR_BIND".to_string(), "127.0.0.1:8792".to_string())]);

        let ts = Engine::Ts.boot_command(repo, 8793);
        assert_eq!(ts.program, "node");
        assert_eq!(ts.args, ["dist/main.js"]);
        assert_eq!(ts.cwd, Some(PathBuf::from("/repo/typescript/server")));
        assert_eq!(
            ts.bind_env,
            [
                ("SMOOTH_OPERATOR_HOST".to_string(), "127.0.0.1".to_string()),
                ("SMOOTH_OPERATOR_PORT".to_string(), "8793".to_string()),
            ]
        );

        let py = Engine::Python.boot_command(repo, 9999);
        assert_eq!(py.program, "uv");
        assert_eq!(py.args, ["run", "python", "-m", "smooth_operator_server"]);
        assert_eq!(py.cwd, Some(PathBuf::from("/repo/python/server")));
        assert!(py.bind_env.is_empty(), "python bind is hardcoded upstream");
        // Python's port is fixed regardless of what's requested.
        assert_eq!(Engine::Python.default_port(), 8787);

        let net = Engine::Dotnet.boot_command(repo, 8795);
        assert_eq!(net.program, "dotnet");
        assert_eq!(net.args, ["run"]);
        assert_eq!(net.cwd, Some(PathBuf::from("/repo/dotnet/server/host")));
        assert_eq!(net.bind_env, [("ASPNETCORE_URLS".to_string(), "http://127.0.0.1:8795".to_string())]);
    }

    #[test]
    fn default_ports_are_distinct_except_pinned_python() {
        let mut ports: Vec<u16> = Engine::ALL.iter().map(|e| e.default_port()).collect();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(ports.len(), 5, "each engine gets its own port");
    }

    /// A canned runner: solved/cost/duration keyed by language, so the
    /// same fake yields deterministic per-engine scores without a live
    /// LLM. Every call for a given language returns the same outcome.
    struct CannedRunner {
        solved: bool,
        cost: f64,
    }

    #[async_trait]
    impl TaskRunner for CannedRunner {
        async fn run_one(&self, _lang: PolyglotLang, _task: &str, _opts: &BenchOpts) -> Result<TaskOutcome> {
            Ok(TaskOutcome {
                solved: self.solved,
                cost_usd: self.cost,
                duration_ms: 1000,
                inconclusive: false,
            })
        }
    }

    /// Fake booter: never spawns a process. Returns a canned runner whose
    /// pass/fail is a function of the engine, so the matrix has variety.
    struct FakeBooter {
        booted: Mutex<Vec<(Engine, String)>>,
    }

    #[async_trait]
    impl EngineBooter for FakeBooter {
        async fn boot(&self, engine: Engine, model: &str) -> Result<BootedEngine> {
            self.booted.lock().unwrap().push((engine, model.to_string()));
            // Rust + Go "solve"; the rest "fail" — arbitrary but deterministic.
            let solved = matches!(engine, Engine::Rust | Engine::Go);
            Ok(BootedEngine {
                runner: Box::new(CannedRunner { solved, cost: 0.05 }),
                url: format!("http://127.0.0.1:{}", engine.default_port()),
                _guard: Box::new(()),
            })
        }
    }

    fn pr_one_per_lang() -> SweepConfig {
        SweepConfig {
            gate: SweepGate::Pr { tasks_per_language: 1 }, // 6 tasks per cell
            budget_usd_cap: 100.0,
            smooth_version: "0.0.0-test".to_string(),
            commit_sha: "deadbeef".to_string(),
            task_opts: BenchOpts::default(),
        }
    }

    #[tokio::test]
    async fn matrix_covers_every_engine_and_carries_dimensions() {
        let curated = CuratedList::default_embedded().unwrap();
        let booter = FakeBooter {
            booted: Mutex::new(Vec::new()),
        };
        let engines = [Engine::Rust, Engine::Go, Engine::Ts];
        let models = vec!["deepseek-v4-flash".to_string()];
        let cfg = pr_one_per_lang();
        let mut obs = crate::sweep::StdoutObserver;

        let run = run_engine_matrix(&curated, &booter, &engines, &models, &cfg, &mut obs).await.unwrap();

        // One result per engine×model cell, each tagged.
        assert_eq!(run.results.len(), 3);
        let tags: Vec<(&str, &str)> = run.results.iter().map(|r| (r.engine.as_str(), r.model.as_str())).collect();
        assert_eq!(tags, [("rust", "deepseek-v4-flash"), ("go", "deepseek-v4-flash"), ("ts", "deepseek-v4-flash")]);

        // Rust + Go solved all 6; ts solved 0 — proves per-cell scoring.
        let by_engine = |name: &str| run.results.iter().find(|r| r.engine == name).unwrap();
        assert_eq!(by_engine("rust").score.tasks_green, 6);
        assert_eq!(by_engine("go").score.tasks_green, 6);
        assert_eq!(by_engine("ts").score.tasks_green, 0);
        assert!((by_engine("rust").score.overall_pass_rate - 1.0).abs() < 1e-9);
        assert!((by_engine("ts").score.overall_pass_rate).abs() < 1e-9);

        // Every cell was actually booted.
        assert_eq!(booter.booted.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn matrix_multiplies_engines_by_models() {
        let curated = CuratedList::default_embedded().unwrap();
        let booter = FakeBooter {
            booted: Mutex::new(Vec::new()),
        };
        let engines = [Engine::Rust, Engine::Python];
        let models = vec!["deepseek-v4-flash".to_string(), "groq-gpt-oss-120b".to_string()];
        let cfg = pr_one_per_lang();
        let mut obs = crate::sweep::StdoutObserver;

        let run = run_engine_matrix(&curated, &booter, &engines, &models, &cfg, &mut obs).await.unwrap();

        // 2 engines × 2 models = 4 cells.
        assert_eq!(run.results.len(), 4);
        // JSON-lines: one line per cell, each carrying engine + model.
        let jsonl = run.to_jsonl().unwrap();
        assert_eq!(jsonl.lines().count(), 4);
        for line in jsonl.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v.get("engine").is_some(), "record carries engine");
            assert!(v.get("model").is_some(), "record carries model");
            assert!(v.get("overall_pass_rate").is_some(), "flattened Score present");
        }
        // Summary renders one row per cell (+ header).
        assert_eq!(run.render_summary().lines().count(), 5);
    }
}
