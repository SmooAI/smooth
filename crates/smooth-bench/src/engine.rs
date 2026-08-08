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
        let (url, token, _guard) = spawn_engine(self.engine, &self.model, &setup.work_dir, &self.repo, &self.env, self.ready_timeout, port, None)?;
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
/// Apply the model + gateway environment a spawned engine needs.
///
/// Split out of [`spawn_engine`] purely so it can be tested: the model
/// pin here was wrong for months in a way nothing could catch.
fn apply_engine_env(command: &mut Command, model: &str, workspace: &Path, env: &EngineEnv) {
    command.env("SMOOAI_MODEL", model);
    // EVERY host reads SMOOTH_WORKSPACE and falls back to cwd — go's
    // main.go:43, typescript's main.ts:112, python's server.py:332, and
    // .NET's Program.cs:140. Setting cwd alone is not enough: `dotnet
    // run --project <dir>` runs the app from the PROJECT directory, so
    // the .NET host confined its coding tools to the engine checkout
    // instead of the scenario workspace and never wrote the file the
    // scenario asserted on (th-93112a). Exporting it explicitly removes
    // the cwd dependency for all five.
    command.env("SMOOTH_WORKSPACE", workspace);
    // BOTH model vars, for the same reason the microVM path sets both:
    // the daemon's `resolve_gateway_config` only reads
    // SMOOTH_AGENT_MODEL. With `SMOOAI_MODEL` alone the daemon silently
    // ran its own default, so `--model` was decorative — every row of a
    // model matrix was the SAME model, and the differences between them
    // were run-to-run variance being read as model quality.
    command.env("SMOOTH_AGENT_MODEL", model);
    if let Some(u) = &env.gateway_url {
        command.env("SMOOAI_GATEWAY_URL", u);
    }
    if let Some(k) = &env.gateway_key {
        command.env("SMOOAI_GATEWAY_KEY", k);
    }
    if let Some(p) = &env.persona {
        command.env("SMOOTH_PERSONA", p);
    }
}

/// How long to let a port free up before calling it someone else's.
///
/// Sized for the sequential case: one scenario's engine is killed and
/// the next boots immediately, so the socket is typically released
/// within a second.
const PORT_RELEASE_TIMEOUT: Duration = Duration::from_secs(20);

/// Poll until nothing is listening on `addr`, or `timeout` elapses.
///
/// Returns whether the port came free.
fn wait_for_port_free(addr: SocketAddr, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Open `<log_dir>/<engine>.log` twice (stdout + stderr both append to
/// it, interleaved as the process emits them).
///
/// Returns `None` on any filesystem failure: losing the log is worth a
/// degraded run, never a failed one — the bench's job is to score the
/// engine, not to insist on observing it.
fn open_engine_log(log_dir: &Path, engine: Engine) -> Option<(std::fs::File, std::fs::File)> {
    std::fs::create_dir_all(log_dir).ok()?;
    let path = log_dir.join(format!("{}.log", engine.as_str()));
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok()?;
    let err = out.try_clone().ok()?;
    eprintln!("engine log: {}", path.display());
    Some((out, err))
}

/// # Errors
/// Errors on prep failure, spawn failure, or if the port never listens
/// within `ready_timeout`.
#[allow(
    clippy::too_many_arguments,
    reason = "a spawn takes what a spawn takes; splitting it into a struct would only move the arity"
)]
fn spawn_engine(
    engine: Engine,
    model: &str,
    workspace: &Path,
    repo: &Path,
    env: &EngineEnv,
    ready_timeout: Duration,
    port: u16,
    log_dir: Option<&Path>,
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
    apply_engine_env(&mut command, model, workspace, env);
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
    // Capture the engine's output when we have somewhere to put it.
    // Discarding it unconditionally is what made "the .NET engine returns
    // INTERNAL_ERROR" undiagnosable — the stack trace went to /dev/null
    // (th-901bdc). A dead engine with no log is a dead end.
    match log_dir.and_then(|d| open_engine_log(d, engine)) {
        Some((out, err)) => {
            command.stdout(Stdio::from(out)).stderr(Stdio::from(err));
        }
        None => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().context("engine bind addr")?;

    // Refuse to start if the port is already taken. Ports are fixed per
    // engine, so a second concurrent run — or a daemon left over from a
    // previous one — is already listening there. `wait_for_port` below
    // only waits for SOMETHING to accept TCP: it cannot tell our engine
    // from a stranger's, so without this check the bench would attach to
    // the wrong process and score it. That is the same class of failure
    // as benchmarking a stale binary, and just as invisible.
    // WAIT for it, then refuse. The suite boots a fresh engine per
    // scenario and the previous one's port is still in TIME_WAIT for a
    // moment after its guard kills it — failing fast here turned 23 of
    // 28 scenarios into "engine boot failed" in the first run after this
    // check landed. A few seconds of patience distinguishes "the last
    // scenario is still letting go" from "someone else owns this".
    if !wait_for_port_free(addr, PORT_RELEASE_TIMEOUT) {
        anyhow::bail!(
            "{addr} is still in use after {PORT_RELEASE_TIMEOUT:?}, so the {} engine cannot start \
             there. Another bench run (or a leftover daemon) owns it — the bench refuses to attach \
             to a process it did not spawn, because it would score that one instead. Stop the \
             other run, or wait for it.",
            engine.as_str()
        );
    }

    let child = command
        .spawn()
        .with_context(|| format!("spawning {} engine ({} {:?})", engine.as_str(), cmd.program, cmd.args))?;
    let guard = KillOnDrop(child);

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
            // ALWAYS install + build (tsc is incremental: ~7s cold, ~2s warm),
            // exactly like Go's unconditional `go build` and Python's `uv sync`.
            // Pearl th-11284c: keying off `dist/main.js` merely EXISTING meant a
            // `dist/` built before the coding toolset landed was silently reused —
            // the server booted and turns completed, but with ZERO tools
            // registered, so the bench scored it as a model-quality FAIL rather
            // than a stale bundle. `dist/` is gitignored, so it can be
            // arbitrarily older than the checkout; existence proves nothing.
            // A stale `node_modules` bites the same way (the engine dep had been
            // bumped 0.1.1 → 1.7.1 without a reinstall), hence install too.
            let dir = repo.join("typescript").join("server");
            run_prep(&dir, "pnpm", &["install", "--silent"])?;
            run_prep(&dir, "pnpm", &["build"])?;
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

// ---------------------------------------------------------------------------
// microVM isolation backend (pearl th-a63c22)
// ---------------------------------------------------------------------------

/// Where a scored task's engine runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Isolation {
    /// Engine runs as a host process (today's behaviour).
    #[default]
    Host,
    /// Engine runs inside a microsandbox microVM: default-deny egress,
    /// only the task workspace bind-mounted in.
    MicroVm,
}

impl Isolation {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "host" => Some(Self::Host),
            "microvm" | "micro-vm" | "vm" => Some(Self::MicroVm),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::MicroVm => "microvm",
        }
    }

    /// microVM isolation boots the linux `smooth-daemon` — the only
    /// engine with a VM-bootable binary. The polyglot servers ship no
    /// tools anyway (pearl th-82ad57), so combining them with `microvm`
    /// is always a mistake worth erroring on.
    ///
    /// # Errors
    /// Errors when `microvm` is paired with any non-rust engine.
    pub fn check_engines(self, engines: &[Engine]) -> Result<()> {
        if self == Self::MicroVm {
            if let Some(bad) = engines.iter().find(|e| **e != Engine::Rust) {
                anyhow::bail!(
                    "--isolation microvm supports only --engine rust (got {}); the polyglot engines have no VM-bootable binary and ship no tools (pearl th-82ad57)",
                    bad.as_str()
                );
            }
        }
        Ok(())
    }
}

/// Everything that varies per microVM task boot. Split out from
/// [`msb_run_args`] so the invocation shape is asserted in tests without
/// spawning anything.
#[derive(Debug, Clone)]
pub struct MsbSpec<'a> {
    /// Unique sandbox name — `msb stop`/`remove` key at teardown.
    pub name: &'a str,
    /// Host dir holding the linux `smooth-daemon`, mounted at `/opt`.
    pub bin_dir: &'a Path,
    /// The task's scratch dir, mounted at `/work` (the daemon's workspace).
    pub workspace: &'a Path,
    /// Host dir mounted at `/var/log/smooth`; the daemon's stdout/stderr
    /// is redirected there because attached `msb run` pipes no guest
    /// output (pearl th-64fd98).
    pub log_dir: &'a Path,
    pub host_port: u16,
    pub guest_port: u16,
    pub token: &'a str,
    pub model: &'a str,
    pub gateway_url: &'a str,
    pub gateway_key: Option<&'a str>,
    /// The daemon is a glibc binary — `debian`, never alpine.
    pub image: &'a str,
}

/// Host of the LLM gateway, i.e. the one hole punched in the VM's
/// default-deny egress policy.
fn gateway_host(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let hostport = after_scheme.split('/').next().unwrap_or(after_scheme);
    // ponytail: no url crate — gateway URLs here are always `scheme://host[:port]/…`.
    hostport.rsplit_once(':').map_or(hostport, |(h, _)| h)
}

/// The full `msb` argv for one task's microVM. Verified shape (msb 0.4.6,
/// libkrun, macOS arm64) — see `scripts/msb-spike/run-daemon-vm.sh`.
///
/// Deliberately **attached**: `-d/--detach` silently ignores the command
/// after `--` and boots the image entrypoint instead, and `msb exec` into
/// a detached sandbox hangs in 0.4.6. The caller backgrounds this child
/// and reaps it via [`MsbGuard`].
pub fn msb_run_args(spec: &MsbSpec<'_>) -> Vec<String> {
    let host = gateway_host(spec.gateway_url);
    let mut a: Vec<String> = vec![
        "run".into(),
        "--name".into(),
        spec.name.into(),
        "-p".into(),
        format!("{}:{}", spec.host_port, spec.guest_port),
        "-v".into(),
        format!("{}:/opt", spec.bin_dir.display()),
        "-v".into(),
        format!("{}:/work", spec.workspace.display()),
        "-v".into(),
        format!("{}:/var/log/smooth", spec.log_dir.display()),
        "-e".into(),
        format!("SMOOTH_ADDR=0.0.0.0:{}", spec.guest_port),
        "-e".into(),
        "SMOOTH_WORKSPACE=/work".into(),
        "-e".into(),
        format!("SMOOTH_LOCAL_TOKEN={}", spec.token),
        "-e".into(),
        format!("SMOOAI_GATEWAY_URL={}", spec.gateway_url),
        "-e".into(),
        format!("SMOOAI_MODEL={}", spec.model),
        // The daemon's own model pin. `SMOOAI_MODEL` alone is NOT enough:
        // `resolve_gateway_config` only honours `SMOOTH_AGENT_MODEL`, so
        // without this the VM silently ran the upstream default
        // (claude-haiku-4-5) instead of `--model`.
        "-e".into(),
        format!("SMOOTH_AGENT_MODEL={}", spec.model),
        "-e".into(),
        "SMOOTH_OPERATOR_DB=/tmp/operator-storage.db".into(),
        "-e".into(),
        "SMOOTH_TAILSCALE_SERVE=0".into(),
        "-e".into(),
        "HOME=/root".into(),
        "-e".into(),
        format!("RUST_LOG={}", std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into())),
        "--net-default-egress".into(),
        "deny".into(),
        "--net-rule".into(),
        format!("allow@{host}:tcp:443"),
    ];
    if let Some(key) = spec.gateway_key {
        a.push("--secret".into());
        a.push(format!("SMOOAI_GATEWAY_KEY={key}@{host}"));
    }
    a.push(spec.image.into());
    a.push("--".into());
    a.push("/bin/sh".into());
    a.push("-c".into());
    a.push("exec /opt/smooth-daemon >> /var/log/smooth/daemon.log 2>&1".into());
    a
}

/// Kills the attached `msb run` child and removes the sandbox, so a
/// sweep can't leak VMs across tasks.
struct MsbGuard {
    name: String,
    child: Child,
}

impl Drop for MsbGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new("msb")
            .args(["remove", "-f", "-q", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// microVM booter: every task boots the LINUX `smooth-daemon` inside its
/// own microsandbox microVM with the task's scratch dir bind-mounted at
/// `/work` and egress denied except the LLM gateway.
pub struct MicroVmBooter {
    /// smooth repo root (holds `scripts/msb-spike/`).
    pub repo_root: PathBuf,
    pub env: EngineEnv,
    pub ready_timeout: Duration,
}

impl MicroVmBooter {
    pub fn new(repo_root: PathBuf, env: EngineEnv) -> Self {
        Self {
            repo_root,
            env,
            ready_timeout: Duration::from_secs(300),
        }
    }
}

#[async_trait]
impl EngineBooter for MicroVmBooter {
    async fn boot(&self, engine: Engine, model: &str) -> Result<BootedEngine> {
        Isolation::MicroVm.check_engines(&[engine])?;
        // Build once, up front — never per task (~163s cold, cached after).
        let bin_dir = ensure_linux_daemon(&self.repo_root)?;
        Ok(BootedEngine {
            runner: Box::new(MicroVmTaskRunner {
                model: model.to_string(),
                bin_dir,
                env: self.env.clone(),
                ready_timeout: self.ready_timeout,
            }),
            url: String::new(),
            _guard: Box::new(()),
        })
    }
}

/// Ensure `scripts/msb-spike/vmbin/smooth-daemon` exists, building it in
/// the container builder if not. Returns the dir to mount at `/opt`.
/// Cached by the binary's own existence — the build never touches the
/// host `~/.cargo` or `./target`, so it can't poison macOS builds.
fn ensure_linux_daemon(repo_root: &Path) -> Result<PathBuf> {
    let spike = repo_root.join("scripts").join("msb-spike");
    let bin_dir = spike.join("vmbin");
    if bin_dir.join("smooth-daemon").exists() {
        return Ok(bin_dir);
    }
    eprintln!("microvm: building the linux smooth-daemon (first run, ~3min) …");
    let status = Command::new(spike.join("build-linux-daemon.sh"))
        .current_dir(&spike)
        .status()
        .context("running scripts/msb-spike/build-linux-daemon.sh (is docker running?)")?;
    anyhow::ensure!(status.success(), "build-linux-daemon.sh failed");
    anyhow::ensure!(bin_dir.join("smooth-daemon").exists(), "build-linux-daemon.sh produced no vmbin/smooth-daemon");
    Ok(bin_dir)
}

/// Poll until `addr` answers an HTTP request (any status — the daemon's
/// strict-auth 401 counts) or `timeout` elapses. A plain TCP connect is
/// NOT a readiness signal through msb's port forwarder: it accepts as
/// soon as the VM starts, before the guest has bound anything.
fn wait_for_http(addr: SocketAddr, timeout: Duration) -> Result<()> {
    use std::io::{Read, Write};
    let start = Instant::now();
    let mut last = "no connection".to_string();
    while start.elapsed() < timeout {
        if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
            if s.write_all(b"GET / HTTP/1.0\r\n\r\n").is_ok() {
                let mut buf = [0u8; 16];
                match s.read(&mut buf) {
                    Ok(n) if buf[..n].starts_with(b"HTTP/") => return Ok(()),
                    Ok(n) => last = format!("{n} non-HTTP bytes"),
                    Err(e) => last = e.to_string(),
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    anyhow::bail!("timed out after {timeout:?} waiting for an HTTP response from {addr} (last: {last})")
}

/// Ask the OS for a free loopback port.
// ponytail: classic bind-and-release race; the window is microseconds and
// each sandbox owns a fresh port. Switch to a reserved-range allocator if
// parallel sweeps ever collide.
fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").context("allocating a free host port")?;
    Ok(l.local_addr()?.port())
}

/// Per-task runner for microVM isolation.
struct MicroVmTaskRunner {
    model: String,
    bin_dir: PathBuf,
    env: EngineEnv,
    ready_timeout: Duration,
}

/// Boot one microVM with `workspace` bind-mounted at `/work` and the VM's
/// stdout/stderr redirected into `log_dir`. Shared by the polyglot sweep
/// (per task) and the agentic bench (per scenario) so both get identical
/// isolation — default-deny egress, only the gateway allowed.
///
/// # Errors
/// Errors when `msb` can't be spawned or the guest never serves HTTP.
fn boot_microvm(bin_dir: &Path, workspace: &Path, log_dir: &Path, model: &str, env: &EngineEnv, ready_timeout: Duration) -> Result<BootedServer> {
    let host_port = free_port()?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("smooth-bench-{}", &token[..8]);
    std::fs::create_dir_all(log_dir).with_context(|| format!("mkdir {}", log_dir.display()))?;

    let gateway_url = env.gateway_url.clone().unwrap_or_else(|| "https://llm.smoo.ai/v1".to_string());
    let args = msb_run_args(&MsbSpec {
        name: &name,
        bin_dir,
        workspace,
        log_dir,
        host_port,
        guest_port: 8791,
        token: &token,
        model,
        gateway_url: &gateway_url,
        gateway_key: env.gateway_key.as_deref(),
        image: "debian",
    });

    let msb_log = std::fs::File::create(log_dir.join("msb.log")).context("creating msb.log")?;
    let mut cmd = Command::new("msb");
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(msb_log.try_clone()?))
        .stderr(Stdio::from(msb_log));
    // New process group so the guard can reap msb's children on drop. Unix-only
    // (microsandbox is unix-only anyway; the crate must still COMPILE on windows).
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn().context("spawning `msb run` (is microsandbox installed?)")?;
    let guard = MsbGuard { name: name.clone(), child };

    let addr: SocketAddr = format!("127.0.0.1:{host_port}").parse().context("microvm host addr")?;
    // NOT `wait_for_port`: msb's host-side forwarder accepts TCP the
    // moment the VM starts, long before the guest binds — a TCP probe
    // returns instantly and the driver then dies with "Handshake not
    // finished". Probe at the HTTP layer instead.
    wait_for_http(addr, ready_timeout).with_context(|| format!("microVM {name} never served HTTP on {addr}; see {}", log_dir.display()))?;

    Ok(BootedServer {
        url: format!("http://127.0.0.1:{host_port}"),
        token: Some(token),
        _guard: Box::new(guard),
    })
}

#[async_trait]
impl TaskRunner for MicroVmTaskRunner {
    async fn run_one(&self, lang: crate::PolyglotLang, task: &str, opts: &crate::BenchOpts) -> Result<TaskOutcome> {
        let setup = crate::prepare_task(lang, task)?;
        // Log dir sits beside the workspace (not in it) so the VM's logs
        // never land in the scored task dir.
        let log_dir = setup.run_dir.join("vmlog");
        let booted = boot_microvm(&self.bin_dir, &setup.work_dir, &log_dir, &self.model, &self.env, self.ready_timeout)?;
        let result = crate::run_prepared(lang, task, &setup, &booted.url, booted.token.as_deref(), opts).await?;
        // `booted` drops here → child killed + `msb remove -f` → no leak.
        Ok(crate::sweep::outcome_from_result(&result))
    }
}

// ---------------------------------------------------------------------------
// Workspace-rooted boot seam (pearl th-300d7d — the agentic bench)
// ---------------------------------------------------------------------------

/// A booted engine serving at `url`, with its file/shell tools rooted at
/// a caller-chosen workspace dir. The teardown guard reaps the process
/// (host) or the sandbox (microVM) on drop.
pub struct BootedServer {
    pub url: String,
    /// Auth token for the driver — `Some` for the rust daemon (strict
    /// auth), `None` for the anonymous polyglot servers.
    pub token: Option<String>,
    _guard: Box<dyn Send + Sync>,
}

impl BootedServer {
    /// Construct a `BootedServer` around an arbitrary teardown guard.
    /// Exists so tests (and future booters outside this module) can
    /// stand one up without spawning anything.
    pub fn new(url: String, token: Option<String>, guard: Box<dyn Send + Sync>) -> Self {
        Self { url, token, _guard: guard }
    }
}

/// Boot an engine whose tools are rooted at an arbitrary workspace.
///
/// The polyglot sweep boots per *task* behind the polyglot-shaped
/// [`TaskRunner`]; the agentic bench boots per *scenario* and needs the
/// workspace seam directly. Both production booters implement this over
/// the exact same spawn paths the sweep uses.
#[async_trait]
pub trait WorkspaceBooter: Send + Sync {
    /// Boot `engine` under `model` with `workspace` as its root.
    /// `log_dir` receives engine/VM logs and must sit OUTSIDE `workspace`
    /// so it can't pollute the scored state.
    async fn boot_workspace(&self, engine: Engine, model: &str, workspace: &Path, log_dir: &Path) -> Result<BootedServer>;
}

#[async_trait]
impl WorkspaceBooter for ProcessBooter {
    async fn boot_workspace(&self, engine: Engine, model: &str, workspace: &Path, log_dir: &Path) -> Result<BootedServer> {
        let port = engine.default_port();
        let (url, token, guard) = spawn_engine(engine, model, workspace, &self.repo, &self.env, self.ready_timeout, port, Some(log_dir))?;
        Ok(BootedServer {
            url,
            token,
            _guard: Box::new(guard),
        })
    }
}

#[async_trait]
impl WorkspaceBooter for MicroVmBooter {
    async fn boot_workspace(&self, engine: Engine, model: &str, workspace: &Path, log_dir: &Path) -> Result<BootedServer> {
        Isolation::MicroVm.check_engines(&[engine])?;
        let bin_dir = ensure_linux_daemon(&self.repo_root)?;
        boot_microvm(&bin_dir, workspace, log_dir, model, &self.env, self.ready_timeout)
    }
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

    fn spec_args(key: Option<&str>) -> Vec<String> {
        msb_run_args(&MsbSpec {
            name: "smooth-bench-abcd1234",
            bin_dir: Path::new("/repo/scripts/msb-spike/vmbin"),
            workspace: Path::new("/scratch/run/leap"),
            log_dir: Path::new("/scratch/run/vmlog"),
            host_port: 51234,
            guest_port: 8791,
            token: "tok123",
            model: "deepseek-v4-flash",
            gateway_url: "https://llm.smoo.ai/v1",
            gateway_key: key,
            image: "debian",
        })
    }

    /// Assert `flag` is immediately followed by `value` somewhere in `args`.
    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn msb_invocation_carries_mounts_ports_egress_and_secret() {
        let a = spec_args(Some("sk-live-xyz"));

        assert_eq!(a[0], "run");
        assert!(has_pair(&a, "--name", "smooth-bench-abcd1234"));
        // Host port is the allocated one; guest port is what the daemon binds.
        assert!(has_pair(&a, "-p", "51234:8791"));
        assert!(has_pair(&a, "-e", "SMOOTH_ADDR=0.0.0.0:8791"));

        // Three mounts: binary at /opt, task scratch at /work, logs out.
        assert!(has_pair(&a, "-v", "/repo/scripts/msb-spike/vmbin:/opt"));
        assert!(has_pair(&a, "-v", "/scratch/run/leap:/work"));
        assert!(has_pair(&a, "-v", "/scratch/run/vmlog:/var/log/smooth"));
        assert!(has_pair(&a, "-e", "SMOOTH_WORKSPACE=/work"));

        // Daemon needs these or it won't come up / won't authenticate.
        assert!(has_pair(&a, "-e", "SMOOTH_LOCAL_TOKEN=tok123"));
        assert!(has_pair(&a, "-e", "SMOOAI_GATEWAY_URL=https://llm.smoo.ai/v1"));
        // BOTH model vars: the daemon's `resolve_gateway_config` only reads
        // SMOOTH_AGENT_MODEL, so SMOOAI_MODEL alone silently ran the
        // upstream default model instead of `--model`.
        assert!(has_pair(&a, "-e", "SMOOAI_MODEL=deepseek-v4-flash"));
        assert!(has_pair(&a, "-e", "SMOOTH_AGENT_MODEL=deepseek-v4-flash"));
        assert!(has_pair(&a, "-e", "SMOOTH_OPERATOR_DB=/tmp/operator-storage.db"));
        assert!(has_pair(&a, "-e", "SMOOTH_TAILSCALE_SERVE=0"));
        assert!(has_pair(&a, "-e", "HOME=/root"));

        // Default-deny egress with exactly one hole: the gateway host.
        assert!(has_pair(&a, "--net-default-egress", "deny"));
        assert!(has_pair(&a, "--net-rule", "allow@llm.smoo.ai:tcp:443"));
        assert_eq!(a.iter().filter(|s| *s == "--net-rule").count(), 1, "exactly one egress hole");

        // Secret is host-scoped so it can't leak to another destination.
        assert!(has_pair(&a, "--secret", "SMOOAI_GATEWAY_KEY=sk-live-xyz@llm.smoo.ai"));

        // glibc binary → debian, and the command lands AFTER `--`
        // (attached mode; `-d` would silently drop it).
        assert!(!a.contains(&"-d".to_string()) && !a.contains(&"--detach".to_string()));
        let dashdash = a.iter().position(|s| s == "--").expect("`--` separator present");
        assert_eq!(a[dashdash - 1], "debian");
        assert_eq!(
            &a[dashdash + 1..],
            ["/bin/sh", "-c", "exec /opt/smooth-daemon >> /var/log/smooth/daemon.log 2>&1"]
        );
    }

    #[test]
    fn msb_invocation_omits_secret_when_no_key() {
        let a = spec_args(None);
        assert!(!a.iter().any(|s| s == "--secret"));
        // …but still denies egress by default.
        assert!(has_pair(&a, "--net-default-egress", "deny"));
    }

    /// Regression: a bare TCP listener that never speaks HTTP must NOT
    /// read as ready — that's exactly what msb's port forwarder looks
    /// like before the guest binds, and treating it as ready killed
    /// every microVM task with "Handshake not finished".
    #[test]
    fn wait_for_http_rejects_a_silent_tcp_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        // Accept and say nothing, like the forwarder does.
        std::thread::spawn(move || while l.accept().is_ok() {});
        let err = wait_for_http(addr, Duration::from_millis(1200)).unwrap_err().to_string();
        assert!(err.contains("timed out"), "silent listener must not read as ready: {err}");
    }

    #[test]
    fn wait_for_http_accepts_any_http_status() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        // Loop-accept and drain the request before replying. A one-shot
        // reply-then-close server races on Windows: closing before the
        // client's request write completes surfaces as WSAECONNRESET, and the
        // client's retry then finds an already-consumed (dead) listener.
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = l.accept() {
                use std::io::{Read, Write};
                let _ = s.read(&mut [0u8; 64]);
                // Strict-auth 401 still means "the engine is up".
                let _ = s.write_all(b"HTTP/1.1 401 Unauthorized\r\n\r\n");
            }
        });
        wait_for_http(addr, Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn gateway_host_strips_scheme_port_and_path() {
        assert_eq!(gateway_host("https://llm.smoo.ai/v1"), "llm.smoo.ai");
        assert_eq!(gateway_host("http://localhost:11434/v1"), "localhost");
        assert_eq!(gateway_host("llm.smoo.ai"), "llm.smoo.ai");
    }

    #[test]
    fn isolation_parsing_round_trips_and_rejects_junk() {
        assert_eq!(Isolation::from_name("host"), Some(Isolation::Host));
        assert_eq!(Isolation::from_name("microvm"), Some(Isolation::MicroVm));
        assert_eq!(Isolation::from_name("MicroVM"), Some(Isolation::MicroVm));
        assert_eq!(Isolation::from_name("docker"), None);
        assert_eq!(Isolation::default(), Isolation::Host);
        for i in [Isolation::Host, Isolation::MicroVm] {
            assert_eq!(Isolation::from_name(i.as_str()), Some(i));
        }
    }

    #[test]
    fn microvm_isolation_only_accepts_the_rust_engine() {
        assert!(Isolation::MicroVm.check_engines(&[Engine::Rust]).is_ok());
        for bad in [Engine::Go, Engine::Ts, Engine::Python, Engine::Dotnet] {
            let err = Isolation::MicroVm.check_engines(&[Engine::Rust, bad]).unwrap_err().to_string();
            assert!(err.contains("--engine rust"), "actionable error, got: {err}");
            assert!(err.contains(bad.as_str()), "names the offending engine, got: {err}");
        }
        // host isolation keeps working for every engine.
        assert!(Isolation::Host.check_engines(&Engine::ALL).is_ok());
    }

    /// The isolation flag picks a booter without disturbing the matrix
    /// seam: a microVM-gated engine set still runs through the same
    /// `run_engine_matrix` path the fake booter exercises.
    #[tokio::test]
    async fn microvm_gate_passes_rust_only_matrix_through_the_booter_seam() {
        let curated = CuratedList::default_embedded().unwrap();
        let engines = [Engine::Rust];
        Isolation::MicroVm.check_engines(&engines).unwrap();

        let booter = FakeBooter {
            booted: Mutex::new(Vec::new()),
        };
        let run = run_engine_matrix(
            &curated,
            &booter,
            &engines,
            &["deepseek-v4-flash".to_string()],
            &pr_one_per_lang(),
            &mut crate::sweep::StdoutObserver,
        )
        .await
        .unwrap();

        assert_eq!(run.results.len(), 1);
        assert_eq!(run.results[0].engine, "rust");
        assert_eq!(booter.booted.lock().unwrap().len(), 1);
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
    /// The host path must pin the model the SAME way the microVM path
    /// does. It didn't, and `--model` was silently ignored: every row of
    /// a `convo --model A --model B` matrix ran the daemon's own default
    /// model, so the two rows differed only by run-to-run variance —
    /// which read as one model beating another.
    #[test]
    fn host_spawn_pins_both_model_vars() {
        let mut c = Command::new("true");
        apply_engine_env(
            &mut c,
            "gpt-5.5",
            Path::new("/scratch/work"),
            &EngineEnv {
                gateway_url: Some("https://llm.smoo.ai/v1".into()),
                gateway_key: Some("k".into()),
                persona: None,
            },
        );
        let envs: Vec<(String, Option<String>)> = c
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        let get = |name: &str| envs.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.clone());

        assert_eq!(get("SMOOAI_MODEL").as_deref(), Some("gpt-5.5"));
        assert_eq!(
            get("SMOOTH_AGENT_MODEL").as_deref(),
            Some("gpt-5.5"),
            "the daemon reads SMOOTH_AGENT_MODEL; without it --model does nothing"
        );
        assert_eq!(get("SMOOAI_GATEWAY_URL").as_deref(), Some("https://llm.smoo.ai/v1"));
        assert_eq!(get("SMOOAI_GATEWAY_KEY").as_deref(), Some("k"));
        assert!(get("SMOOTH_PERSONA").is_none(), "an unset persona must not be exported");
        assert_eq!(
            get("SMOOTH_WORKSPACE").as_deref(),
            Some("/scratch/work"),
            "every host confines its coding tools to SMOOTH_WORKSPACE and falls back to cwd; \
             `dotnet run --project` makes cwd the engine checkout, not the scenario workspace (th-93112a)"
        );
    }

    #[test]
    fn engine_log_is_named_after_the_engine_and_appends() {
        // Two opens of the same log must not truncate each other —
        // stdout and stderr both write to it, interleaved.
        let d = tempfile::tempdir().expect("tmp");
        let (mut a, mut b) = open_engine_log(d.path(), Engine::Dotnet).expect("opens");
        use std::io::Write as _;
        writeln!(a, "from stdout").expect("write");
        writeln!(b, "from stderr").expect("write");
        let text = std::fs::read_to_string(d.path().join("dotnet.log")).expect("read");
        assert!(text.contains("from stdout"), "{text}");
        assert!(text.contains("from stderr"), "stderr must append, not truncate: {text}");
    }

    #[test]
    fn a_bad_log_dir_degrades_instead_of_failing_the_run() {
        // Losing the log is worth a degraded run, never a failed one.
        let f = tempfile::NamedTempFile::new().expect("tmp");
        assert!(
            open_engine_log(f.path(), Engine::Rust).is_none(),
            "a file where a dir belongs must yield None, not panic"
        );
    }
    /// Regression: the port check originally failed fast, which turned 23
    /// of 28 agentic scenarios into "engine boot failed" — each scenario
    /// boots a fresh engine and the previous one's socket is still
    /// closing.
    #[test]
    fn a_free_port_is_free_immediately() {
        // Bind and drop, so the address is known-unused.
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        drop(l);
        assert!(wait_for_port_free(addr, Duration::from_secs(2)), "an unused port must not be waited on");
    }

    #[test]
    fn an_occupied_port_is_reported_after_the_timeout_not_before() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        let start = std::time::Instant::now();
        assert!(!wait_for_port_free(addr, Duration::from_millis(900)), "a held port must not read as free");
        assert!(start.elapsed() >= Duration::from_millis(800), "it must actually wait before giving up");
        drop(l);
    }
}
