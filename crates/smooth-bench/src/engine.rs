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
// Unix-only: used for `process_group` below. Windows has no equivalent and the
// bench only ever runs on unix (the engines need unix toolchains), but the crate
// must still COMPILE on windows for CI. (th-4c3e2d)
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Serialize;

use crate::curated::CuratedList;
use crate::score::Score;
use crate::sweep::{run_sweep, SweepConfig, SweepObserver, SweepRun, TaskOutcome, TaskRunner};

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
    /// at the smooth-operator `repo`, with its workspace pointed at
    /// `workspace` (the task's scratch dir). Pure data — mirrors the
    /// `serve()` cases in `scripts/operator-serve.sh` so the mapping is
    /// unit-testable without spawning anything. Gateway / persona / model
    /// env (and the rust daemon's auth token) is layered on at spawn time
    /// (see [`spawn_engine`]), not here.
    ///
    /// **Workspace wiring** — the agent must edit files in the task's
    /// scratch dir, so the engine's file tools have to be rooted there:
    /// - **rust** reads `SMOOTH_WORKSPACE`.
    /// - **go / ts / python / dotnet** have no workspace env; their file
    ///   tools key off the process cwd, so we launch them with
    ///   `cwd = workspace`. Because that means the process is no longer
    ///   started from its own project dir, the launcher args carry an
    ///   **absolute** path back to the server (the go package, the ts
    ///   bundle, `--project` for python/dotnet) so the toolchain still
    ///   resolves the module while the child runs in `workspace`.
    pub fn boot_command(self, repo: &Path, port: u16, workspace: &Path) -> BootCommand {
        let bind = format!("127.0.0.1:{port}");
        let ws = workspace.to_path_buf();
        match self {
            // The one runnable Rust LocalServer is the daemon itself; it
            // confines its fs/shell tools to SMOOTH_WORKSPACE.
            Self::Rust => BootCommand {
                program: "th".into(),
                args: vec!["daemon".into()],
                cwd: None,
                env: vec![("SMOOTH_ADDR".into(), bind), ("SMOOTH_WORKSPACE".into(), ws.display().to_string())],
            },
            Self::Go => BootCommand {
                // `go run` resolves modules from the *cwd*, not the package
                // path, so it can't launch from `workspace`. Instead we run a
                // binary prebuilt by `prepare_engine` (see [`go_serve_bin`])
                // with cwd = workspace so the file tools root there.
                program: go_serve_bin().display().to_string(),
                args: vec![],
                cwd: Some(ws),
                env: vec![("SMOOTH_OPERATOR_BIND".into(), bind)],
            },
            Self::Ts => BootCommand {
                program: "node".into(),
                // node resolves imports/node_modules from the bundle's own
                // dir, so an absolute path to dist/main.js runs fine with
                // cwd = workspace.
                args: vec![repo.join("typescript").join("server").join("dist").join("main.js").display().to_string()],
                cwd: Some(ws),
                env: vec![
                    ("SMOOTH_OPERATOR_HOST".into(), "127.0.0.1".into()),
                    ("SMOOTH_OPERATOR_PORT".into(), port.to_string()),
                ],
            },
            Self::Python => BootCommand {
                program: "uv".into(),
                // --project pins the server's pyproject/venv; cwd = workspace
                // roots the file tools. Bind is hardcoded 127.0.0.1:8787.
                args: vec![
                    "run".into(),
                    "--project".into(),
                    repo.join("python").join("server").display().to_string(),
                    "python".into(),
                    "-m".into(),
                    "smooth_operator_server".into(),
                ],
                cwd: Some(ws),
                env: vec![],
            },
            Self::Dotnet => BootCommand {
                program: "dotnet".into(),
                // --project builds/runs the host project from anywhere; cwd =
                // workspace roots the file tools.
                args: vec![
                    "run".into(),
                    "--project".into(),
                    repo.join("dotnet").join("server").join("host").display().to_string(),
                ],
                cwd: Some(ws),
                env: vec![("ASPNETCORE_URLS".into(), format!("http://{bind}"))],
            },
        }
    }
}

/// Where `prepare_engine` builds the Go server binary. Outside both repos
/// (a stable temp path) so building it never dirties the smooth-operator
/// checkout; deterministic within a process so [`Engine::boot_command`]
/// and the unit test agree.
fn go_serve_bin() -> PathBuf {
    std::env::temp_dir().join("smooth-bench-go-operator-serve")
}

/// A fully-resolved spawn recipe. `program` + `args` run in `cwd` with
/// `env` (bind + workspace, plus the shared gateway/persona/model env and
/// the rust daemon's auth token layered on at spawn time) in the
/// environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
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
        // The OS process is booted PER TASK (with its workspace pointed at
        // that task's scratch dir) inside the returned runner — not here.
        // `boot` just hands back a runner carrying the boot config.
        Ok(BootedEngine {
            runner: Box::new(EngineTaskRunner {
                engine,
                model: model.to_string(),
                repo: self.repo.clone(),
                env: self.env.clone(),
                ready_timeout: self.ready_timeout,
            }),
            url: String::new(),
            _guard: Box::new(()),
        })
    }
}

/// The production per-task runner: for each task it prepares the scratch
/// dir, boots the engine with `workspace = work_dir`, drives one canonical
/// turn, scores, and tears the engine down before the next task.
pub struct EngineTaskRunner {
    pub engine: Engine,
    pub model: String,
    pub repo: PathBuf,
    pub env: EngineEnv,
    pub ready_timeout: Duration,
}

#[async_trait]
impl TaskRunner for EngineTaskRunner {
    async fn run_one(&self, lang: crate::PolyglotLang, task: &str, opts: &crate::BenchOpts) -> Result<TaskOutcome> {
        let setup = crate::prepare_task(lang, task)?;
        let port = self.engine.default_port();
        let (url, token, _guard) = spawn_engine(self.engine, &self.model, &setup.work_dir, &self.repo, &self.env, self.ready_timeout, port)?;
        let result = crate::run_prepared(lang, task, &setup, &url, token.as_deref(), opts).await?;
        // `_guard` drops here → the engine (and its children) are reaped
        // before the next task boots on the same port.
        Ok(crate::sweep::outcome_from_result(&result))
    }
}

/// Boot one engine LocalServer with its workspace pointed at `workspace`,
/// waiting for its port to accept TCP. Returns the base URL, the auth
/// token (the rust daemon runs strict-auth; `None` for the anonymous
/// polyglot servers), and a teardown guard that kills the process (and
/// its children) on drop.
///
/// # Errors
/// Errors on prep failure, spawn failure, or if the port never listens
/// within `ready_timeout`.
#[allow(clippy::too_many_arguments)]
fn spawn_engine(
    engine: Engine,
    model: &str,
    workspace: &Path,
    repo: &Path,
    env: &EngineEnv,
    ready_timeout: Duration,
    port: u16,
) -> Result<(String, Option<String>, KillOnDrop)> {
    prepare_engine(engine, repo)?;
    let cmd = engine.boot_command(repo, port, workspace);

    let mut command = Command::new(&cmd.program);
    command.args(&cmd.args);
    if let Some(cwd) = &cmd.cwd {
        command.current_dir(cwd);
    }
    for (k, v) in &cmd.env {
        command.env(k, v);
    }
    command.env("SMOOAI_MODEL", model);
    if let Some(u) = &env.gateway_url {
        command.env("SMOOAI_GATEWAY_URL", u);
    }
    if let Some(k) = &env.gateway_key {
        command.env("SMOOAI_GATEWAY_KEY", k);
    }
    if let Some(p) = &env.persona {
        command.env("SMOOTH_PERSONA", p);
    }
    // The rust daemon runs strict-auth; hand it a known token so the driver
    // can authenticate. The polyglot servers are anonymous (token = None).
    let token = if engine == Engine::Rust {
        let t = uuid::Uuid::new_v4().simple().to_string();
        command.env("SMOOTH_LOCAL_TOKEN", &t);
        Some(t)
    } else {
        None
    };
    // New process group so we can reap `go run` / `dotnet run` children.
    // Unix-only; windows has no equivalent (see the cfg'd import above).
    #[cfg(unix)]
    command.process_group(0);
    command.stdout(Stdio::null()).stderr(Stdio::null());

    let child = command
        .spawn()
        .with_context(|| format!("spawning {} engine ({} {:?})", engine.as_str(), cmd.program, cmd.args))?;
    let guard = KillOnDrop(child);

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().context("engine bind addr")?;
    wait_for_port(addr, ready_timeout).with_context(|| format!("{} engine never opened {addr}", engine.as_str()))?;

    Ok((format!("http://127.0.0.1:{port}"), token, guard))
}

/// Best-effort pre-spawn prep the shell script does inline: the TS
/// server needs a `dist/` build, the Python server a synced venv. Keyed
/// off the engine's project dir under `repo` (independent of where the
/// server is later launched from).
fn prepare_engine(engine: Engine, repo: &Path) -> Result<()> {
    match engine {
        Engine::Go => {
            // Build the server binary once (incremental after the first —
            // Go's build cache keys off content, not cwd) so it can be
            // launched from the task workspace. `go run` can't, because it
            // resolves the module from cwd.
            let dir = repo.join("go").join("server");
            let bin = go_serve_bin();
            run_prep(&dir, "go", &["build", "-o", &bin.display().to_string(), "./cmd/serve"])?;
        }
        Engine::Ts => {
            let dir = repo.join("typescript").join("server");
            if !dir.join("dist").join("main.js").exists() {
                run_prep(&dir, "pnpm", &["install", "--silent"])?;
                run_prep(&dir, "pnpm", &["build"])?;
            }
        }
        Engine::Python => {
            run_prep(&repo.join("python").join("server"), "uv", &["sync", "--quiet"])?;
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

    use crate::sweep::SweepGate;
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

    // Unix-only: asserts exact unix-style path strings (`repo.join(..)` yields
    // backslashes on windows). The bench only ever RUNS on unix — the engines
    // need unix toolchains — but the crate must still compile+test on windows
    // for CI, so skip this mapping assertion there. (th-4c3e2d)
    #[cfg(unix)]
    #[test]
    fn boot_command_mapping_matches_operator_serve() {
        let repo = Path::new("/repo");
        let ws = Path::new("/scratch/task-work");

        // rust: workspace via env, no cwd override (daemon confines to SMOOTH_WORKSPACE).
        let rust = Engine::Rust.boot_command(repo, 8791, ws);
        assert_eq!(rust.program, "th");
        assert_eq!(rust.args, ["daemon"]);
        assert_eq!(rust.cwd, None);
        assert_eq!(
            rust.env,
            [
                ("SMOOTH_ADDR".to_string(), "127.0.0.1:8791".to_string()),
                ("SMOOTH_WORKSPACE".to_string(), "/scratch/task-work".to_string()),
            ]
        );

        // go/ts/python/dotnet: cwd = workspace. Go runs a prebuilt binary
        // (go run can't launch from a foreign cwd); the rest use an absolute
        // path back to the server.
        let go = Engine::Go.boot_command(repo, 8792, ws);
        assert_eq!(go.program, go_serve_bin().display().to_string());
        assert!(go.args.is_empty());
        assert_eq!(go.cwd, Some(PathBuf::from("/scratch/task-work")));
        assert_eq!(go.env, [("SMOOTH_OPERATOR_BIND".to_string(), "127.0.0.1:8792".to_string())]);

        let ts = Engine::Ts.boot_command(repo, 8793, ws);
        assert_eq!(ts.program, "node");
        assert_eq!(ts.args, ["/repo/typescript/server/dist/main.js"]);
        assert_eq!(ts.cwd, Some(PathBuf::from("/scratch/task-work")));
        assert_eq!(
            ts.env,
            [
                ("SMOOTH_OPERATOR_HOST".to_string(), "127.0.0.1".to_string()),
                ("SMOOTH_OPERATOR_PORT".to_string(), "8793".to_string()),
            ]
        );

        let py = Engine::Python.boot_command(repo, 9999, ws);
        assert_eq!(py.program, "uv");
        assert_eq!(py.args, ["run", "--project", "/repo/python/server", "python", "-m", "smooth_operator_server"]);
        assert_eq!(py.cwd, Some(PathBuf::from("/scratch/task-work")));
        assert!(py.env.is_empty(), "python bind is hardcoded upstream");
        // Python's port is fixed regardless of what's requested.
        assert_eq!(Engine::Python.default_port(), 8787);

        let net = Engine::Dotnet.boot_command(repo, 8795, ws);
        assert_eq!(net.program, "dotnet");
        assert_eq!(net.args, ["run", "--project", "/repo/dotnet/server/host"]);
        assert_eq!(net.cwd, Some(PathBuf::from("/scratch/task-work")));
        assert_eq!(net.env, [("ASPNETCORE_URLS".to_string(), "http://127.0.0.1:8795".to_string())]);
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
