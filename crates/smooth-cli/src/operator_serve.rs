//! `th operator serve` — boot any of the 5 polyglot smooth-operator LocalServer
//! implementations behind a uniform env contract, so Big Smooth's control
//! surface (and the bench harness) can drive each engine over the shared WS
//! protocol. Pearl th-3f46fd.
//!
//! This is the first-class version of `scripts/operator-serve.sh` — the value
//! over the raw script is discoverability (`th operator serve --help`),
//! self-documenting per-engine caveats, and repo-path resolution. The servers
//! live in the sibling `smooth-operator` repo; override the location with
//! `SMOOTH_OPERATOR_REPO` (default `~/dev/smooai/smooth-operator`).
//!
//! Uniform env contract passed through to every engine (inherited from the
//! current process): `SMOOAI_GATEWAY_URL`, `SMOOAI_GATEWAY_KEY`,
//! `SMOOTH_PERSONA`, `SMOOAI_MODEL` (default `deepseek-v4-flash`). The launcher
//! sets the right per-engine bind var from `--port`.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

/// Which polyglot LocalServer implementation to boot. clap renders these as
/// `rust` / `go` / `ts` / `python` / `dotnet`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Lang {
    /// Rust — the `th daemon` LocalServer (the only runnable Rust server;
    /// carries the daemon's narc/storage/persona extras, not a bare engine).
    Rust,
    /// Go — `go run ./cmd/serve`.
    Go,
    /// TypeScript — `node dist/main.js` (always rebuilt first; tsc is incremental).
    Ts,
    /// Python — `uv run python -m smooth_operator_server` (bind is hardcoded
    /// 127.0.0.1:8787 upstream; `--port` is ignored).
    Python,
    /// .NET — `dotnet run`.
    Dotnet,
}

/// Default agent model when `SMOOAI_MODEL` is unset.
const DEFAULT_MODEL: &str = "deepseek-v4-flash";
/// Default bind port (matches `scripts/operator-serve.sh`).
const DEFAULT_PORT: u16 = 8799;
/// Python's upstream-hardcoded bind port.
const PYTHON_FIXED_PORT: u16 = 8787;

/// A fully-resolved launch plan: the exact process to spawn. Kept pure (no I/O,
/// no side effects) so the lang→invocation mapping is unit-testable.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    program: String,
    args: Vec<String>,
    /// Working directory to `cd` into before spawning; `None` = inherit.
    cwd: Option<PathBuf>,
    /// Engine-specific env vars to set on top of the inherited environment.
    env: Vec<(String, String)>,
}

/// Resolve the smooth-operator repo root. `SMOOTH_OPERATOR_REPO` wins; otherwise
/// `~/dev/smooai/smooth-operator`. Pulled out (env value passed in) so it stays
/// testable without mutating process env.
fn resolve_repo(env_override: Option<String>, home: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = env_override.filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let home = home.context("no home directory; set SMOOTH_OPERATOR_REPO to the smooth-operator repo path")?;
    Ok(home.join("dev").join("smooai").join("smooth-operator"))
}

/// Warning to print when `--port` is meaningless for the chosen engine.
/// Currently only Python (bind is hardcoded upstream).
fn port_warning(lang: Lang, port: u16) -> Option<String> {
    if lang == Lang::Python && port != PYTHON_FIXED_PORT {
        Some(format!(
            "note: python bind is hardcoded 127.0.0.1:{PYTHON_FIXED_PORT} upstream; ignoring --port {port}"
        ))
    } else {
        None
    }
}

/// Build the launch plan for `lang` on `port`, against `repo`. `th_bin` is the
/// path to the running `th` (used for the Rust `th daemon` server). Pure.
fn plan(lang: Lang, port: u16, repo: &std::path::Path, th_bin: &std::path::Path, model: &str) -> Plan {
    let bind = format!("127.0.0.1:{port}");
    let model_env = ("SMOOAI_MODEL".to_string(), model.to_string());
    match lang {
        Lang::Rust => Plan {
            program: th_bin.to_string_lossy().into_owned(),
            args: vec!["daemon".to_string()],
            cwd: None,
            env: vec![("SMOOTH_ADDR".to_string(), bind), model_env],
        },
        Lang::Go => Plan {
            program: "go".to_string(),
            args: vec!["run".to_string(), "./cmd/serve".to_string()],
            cwd: Some(repo.join("go").join("server")),
            env: vec![("SMOOTH_OPERATOR_BIND".to_string(), bind), model_env],
        },
        Lang::Ts => Plan {
            program: "node".to_string(),
            args: vec!["dist/main.js".to_string()],
            cwd: Some(repo.join("typescript").join("server")),
            env: vec![
                ("SMOOTH_OPERATOR_HOST".to_string(), "127.0.0.1".to_string()),
                ("SMOOTH_OPERATOR_PORT".to_string(), port.to_string()),
                model_env,
            ],
        },
        Lang::Python => Plan {
            program: "uv".to_string(),
            args: vec!["run".to_string(), "python".to_string(), "-m".to_string(), "smooth_operator_server".to_string()],
            cwd: Some(repo.join("python").join("server")),
            env: vec![model_env],
        },
        Lang::Dotnet => Plan {
            program: "dotnet".to_string(),
            args: vec!["run".to_string()],
            cwd: Some(repo.join("dotnet").join("server").join("host")),
            env: vec![("ASPNETCORE_URLS".to_string(), format!("http://{bind}")), model_env],
        },
    }
}

/// Run a build/prep step in `cwd`, streaming its output. Bails on failure.
fn prep(cwd: &std::path::Path, program: &str, args: &[&str]) -> Result<()> {
    eprintln!("th operator serve: {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("running `{program}` in {}", cwd.display()))?;
    anyhow::ensure!(status.success(), "`{program} {}` failed in {}", args.join(" "), cwd.display());
    Ok(())
}

/// Side-effecting pre-build steps that must run before the server can boot:
/// TS needs `dist/main.js`; Python needs its uv env synced.
fn prebuild(lang: Lang, cwd: Option<&PathBuf>) -> Result<()> {
    let Some(cwd) = cwd else { return Ok(()) };
    match lang {
        Lang::Ts => {
            // ALWAYS, not "only when dist/main.js is missing" — `dist/` is
            // gitignored and can be arbitrarily older than the checkout. Pearl
            // th-11284c: a bundle built before the coding toolset landed booted
            // fine and answered turns while registering ZERO tools, which reads
            // as a bad model rather than a stale artifact. `tsc` is incremental,
            // same cost as Python's unconditional `uv sync` below.
            prep(cwd, "pnpm", &["install"])?;
            prep(cwd, "pnpm", &["build"])?;
        }
        Lang::Python => prep(cwd, "uv", &["sync", "--quiet"])?,
        _ => {}
    }
    Ok(())
}

/// `th operator serve --lang <x> [--port <n>]` entry point.
pub fn serve(lang: Lang, port: Option<u16>) -> Result<()> {
    let port = port.unwrap_or(DEFAULT_PORT);
    let repo = resolve_repo(std::env::var("SMOOTH_OPERATOR_REPO").ok(), dirs_next::home_dir())?;
    anyhow::ensure!(
        repo.is_dir(),
        "smooth-operator repo not found at {} — clone it there or set SMOOTH_OPERATOR_REPO",
        repo.display()
    );
    let model = std::env::var("SMOOAI_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let th_bin = std::env::current_exe().context("resolving the running `th` binary")?;

    if let Some(w) = port_warning(lang, port) {
        eprintln!("{w}");
    }

    let plan = plan(lang, port, &repo, &th_bin, &model);
    if let Some(cwd) = &plan.cwd {
        anyhow::ensure!(
            cwd.is_dir(),
            "server directory not found: {} (is the smooth-operator repo complete?)",
            cwd.display()
        );
    }
    prebuild(lang, plan.cwd.as_ref())?;

    eprintln!("th operator serve: booting {lang:?} LocalServer on 127.0.0.1:{port} (model {model})");
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args);
    if let Some(cwd) = &plan.cwd {
        cmd.current_dir(cwd);
    }
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }
    let status = cmd.status().with_context(|| format!("spawning `{}`", plan.program))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn repo() -> PathBuf {
        PathBuf::from("/repo")
    }
    fn th() -> PathBuf {
        PathBuf::from("/usr/local/bin/th")
    }

    #[test]
    fn lang_value_enum_accepts_the_five_and_rejects_unknown() {
        use clap::ValueEnum;
        assert_eq!(Lang::from_str("rust", false).unwrap(), Lang::Rust);
        assert_eq!(Lang::from_str("go", false).unwrap(), Lang::Go);
        assert_eq!(Lang::from_str("ts", false).unwrap(), Lang::Ts);
        assert_eq!(Lang::from_str("python", false).unwrap(), Lang::Python);
        assert_eq!(Lang::from_str("dotnet", false).unwrap(), Lang::Dotnet);
        assert!(Lang::from_str("cobol", false).is_err());
        assert!(Lang::from_str("", false).is_err());
    }

    #[test]
    fn rust_plan_runs_th_daemon_with_smooth_addr() {
        let p = plan(Lang::Rust, 9000, &repo(), &th(), "m1");
        assert_eq!(p.program, "/usr/local/bin/th");
        assert_eq!(p.args, vec!["daemon"]);
        assert_eq!(p.cwd, None);
        assert!(p.env.contains(&("SMOOTH_ADDR".to_string(), "127.0.0.1:9000".to_string())));
        assert!(p.env.contains(&("SMOOAI_MODEL".to_string(), "m1".to_string())));
    }

    #[test]
    fn go_plan_binds_via_smooth_operator_bind_in_go_server_dir() {
        let p = plan(Lang::Go, 8801, &repo(), &th(), "m");
        assert_eq!(p.program, "go");
        assert_eq!(p.args, vec!["run", "./cmd/serve"]);
        assert_eq!(p.cwd, Some(Path::new("/repo/go/server").to_path_buf()));
        assert!(p.env.contains(&("SMOOTH_OPERATOR_BIND".to_string(), "127.0.0.1:8801".to_string())));
    }

    #[test]
    fn ts_plan_uses_host_port_env_and_typescript_server_dir() {
        let p = plan(Lang::Ts, 8802, &repo(), &th(), "m");
        assert_eq!(p.program, "node");
        assert_eq!(p.args, vec!["dist/main.js"]);
        assert_eq!(p.cwd, Some(Path::new("/repo/typescript/server").to_path_buf()));
        assert!(p.env.contains(&("SMOOTH_OPERATOR_HOST".to_string(), "127.0.0.1".to_string())));
        assert!(p.env.contains(&("SMOOTH_OPERATOR_PORT".to_string(), "8802".to_string())));
    }

    #[test]
    fn python_plan_ignores_port_and_runs_uv_module() {
        let p = plan(Lang::Python, 9999, &repo(), &th(), "m");
        assert_eq!(p.program, "uv");
        assert_eq!(p.args, vec!["run", "python", "-m", "smooth_operator_server"]);
        assert_eq!(p.cwd, Some(Path::new("/repo/python/server").to_path_buf()));
        // No bind env — port is meaningless for python.
        assert!(p.env.iter().all(|(k, _)| k == "SMOOAI_MODEL"));
    }

    #[test]
    fn dotnet_plan_sets_aspnetcore_urls_in_host_dir() {
        let p = plan(Lang::Dotnet, 8803, &repo(), &th(), "m");
        assert_eq!(p.program, "dotnet");
        assert_eq!(p.args, vec!["run"]);
        assert_eq!(p.cwd, Some(Path::new("/repo/dotnet/server/host").to_path_buf()));
        assert!(p.env.contains(&("ASPNETCORE_URLS".to_string(), "http://127.0.0.1:8803".to_string())));
    }

    #[test]
    fn python_port_warns_only_when_not_the_fixed_port() {
        assert!(port_warning(Lang::Python, 8799).unwrap().contains("ignoring --port 8799"));
        assert!(port_warning(Lang::Python, 9000).is_some());
        assert!(port_warning(Lang::Python, PYTHON_FIXED_PORT).is_none());
        // Other engines never warn about the port.
        assert!(port_warning(Lang::Rust, 1234).is_none());
        assert!(port_warning(Lang::Go, 1234).is_none());
    }

    #[test]
    fn repo_override_wins_over_home_default() {
        let r = resolve_repo(Some("/custom/operator".to_string()), Some(PathBuf::from("/home/u"))).unwrap();
        assert_eq!(r, PathBuf::from("/custom/operator"));
    }

    #[test]
    fn repo_falls_back_to_home_dev_path() {
        let r = resolve_repo(None, Some(PathBuf::from("/home/u"))).unwrap();
        assert_eq!(r, PathBuf::from("/home/u/dev/smooai/smooth-operator"));
    }

    #[test]
    fn repo_empty_override_falls_back_to_home() {
        let r = resolve_repo(Some(String::new()), Some(PathBuf::from("/home/u"))).unwrap();
        assert_eq!(r, PathBuf::from("/home/u/dev/smooai/smooth-operator"));
    }

    #[test]
    fn default_port_matches_the_shell_launcher() {
        assert_eq!(DEFAULT_PORT, 8799);
    }
}
