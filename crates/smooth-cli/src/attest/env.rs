//! Preconditions the attest runner owns, ported from smooai's `scripts/ci/_env.sh`.
//!
//! The governing rule, paid for by three separate incidents: **a precondition you
//! can repair, repair. One you cannot, report as 97 — never as a verdict on the
//! commit.** A commit status is a claim about the *code*; "your Docker daemon is
//! off" is a claim about the *laptop*, and posting it as `failure` red-flags a PR
//! over a fact about someone's machine (th-514dc8).

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A check that could not START is not a check that failed. Scripts signal that
/// with this exit code, and so does `th attest` itself (th-514dc8).
pub const EXIT_PRECONDITION: i32 = 97;

/// Everything `attest` reaches outside its own process, in one injectable bag.
///
/// The bash suite stubbed these by shadowing `PATH`; here they are plain program
/// names that a test can point at a temp-dir script instead. Same fidelity — real
/// subprocesses, real exit codes — without mutating process-global `PATH`.
#[derive(Clone)]
pub struct Sys {
    pub gh: OsString,
    pub ssh: OsString,
    pub uptime: OsString,
    pub docker: OsString,
    /// Container-runtime starters, tried in order. Only ever STARTED, never stopped.
    pub starters: Vec<OsString>,
    pub pnpm: OsString,
    pub cores: usize,
    pub docker_wait: Duration,
    pub docker_poll: Duration,
    pub no_docker_start: bool,
    pub no_install: bool,
    /// Whether to append the well-known tool dirs to the process `PATH`. Off in
    /// tests so parallel cases don't race on a process-global.
    pub normalize_path: bool,
}

impl Default for Sys {
    fn default() -> Self {
        Self {
            gh: "gh".into(),
            ssh: "ssh".into(),
            uptime: "uptime".into(),
            docker: "docker".into(),
            starters: vec!["orbctl".into(), "colima".into()],
            pnpm: "pnpm".into(),
            cores: core_count(),
            docker_wait: Duration::from_secs(env_u64("DOCKER_WAIT").unwrap_or(120)),
            docker_poll: Duration::from_secs(2),
            no_docker_start: env_flag("NO_DOCKER_START"),
            no_install: env_flag("NO_INSTALL"),
            normalize_path: true,
        }
    }
}

/// Both spellings are honoured: these knobs were born as `SMOOAI_CI_*` in the
/// monorepo's bash and anyone with them already exported should not have to learn
/// a second name just because the runner is now Rust.
fn env_flag(suffix: &str) -> bool {
    [format!("SMOOAI_CI_{suffix}"), format!("SMOOTH_CI_{suffix}")]
        .iter()
        .any(|k| env::var_os(k).is_some_and(|v| !v.is_empty()))
}

fn env_u64(suffix: &str) -> Option<u64> {
    [format!("SMOOAI_CI_{suffix}"), format!("SMOOTH_CI_{suffix}")]
        .iter()
        .find_map(|k| env::var(k).ok())
        .and_then(|v| v.parse().ok())
}

fn core_count() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// The four dirs a non-login shell loses, in the order `_env.sh` appends them.
fn tool_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".cargo/bin"),
        home.join(".local/bin"),
    ]
}

/// th-92b35a: a non-login shell — launchd, cron, `ssh host cmd`, a service unit —
/// never sources the profile that puts Homebrew and cargo on `PATH`. Measured on
/// smoo-hub: Docker was RUNNING and the rust check exited 97 in zero seconds,
/// because neither `docker` nor `orbctl` resolved and the auto-start silently
/// degraded to a refusal. Critical for remote delegation, which is `ssh host cmd`
/// by construction.
///
/// APPENDED, never prepended: an explicitly-set `PATH` still wins, so this can
/// only ever add a tool that was missing, never shadow one deliberately chosen.
/// Idempotent — some scripts source their environment more than once.
pub fn normalize_path(current: &str, home: &Path) -> String {
    let mut out: Vec<PathBuf> = env::split_paths(current).collect();
    for dir in tool_dirs(home) {
        if !dir.is_dir() || out.contains(&dir) {
            continue;
        }
        out.push(dir);
    }
    env::join_paths(out).map_or_else(|_| current.to_string(), |p| p.to_string_lossy().into_owned())
}

/// The same normalisation as a `PATH=…` prefix for a remote shell, which never
/// gets to run this code. Kept beside its local twin so the two cannot drift.
pub fn remote_path_prelude() -> String {
    r#"export PATH="$PATH:/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:$HOME/.local/bin""#.to_string()
}

pub fn apply_path(sys: &Sys) {
    if !sys.normalize_path {
        return;
    }
    let (Ok(current), Some(home)) = (env::var("PATH"), dirs_next::home_dir()) else {
        return;
    };
    env::set_var("PATH", normalize_path(&current, &home));
}

/// True when `name` is an existing file — resolved on `PATH` for a bare name, or
/// directly for anything with a separator in it (how tests point at stubs).
fn resolves(name: &OsStr) -> bool {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.is_file();
    }
    env::var_os("PATH").is_some_and(|paths| env::split_paths(&paths).any(|d| d.join(p).is_file()))
}

fn quiet(cmd: &mut Command) -> &mut Command {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
}

fn docker_up(sys: &Sys) -> bool {
    quiet(Command::new(&sys.docker).arg("info")).status().is_ok_and(|s| s.success())
}

/// th-403a7d + th-98b32b: a Docker-backed suite that cannot reach a daemon does
/// not fail — it SKIPS, and a check whose every test skipped still exits 0 and
/// reports green over zero coverage. So the daemon has to be up.
///
/// Measured 2026-08-09, the day agents finally started attesting: every single
/// `rust` attempt died on a stopped runtime, so the 38-minute CI job ran anyway.
/// Starting the daemon takes seconds. **Prefer removing the precondition to
/// reporting it.**
///
/// ponytail: a machine with no `docker` at all is not a machine with a stopped
/// daemon — it is one that plainly runs no container-backed checks. Skip rather
/// than block, or every attest on every Docker-less host reports 97.
pub fn ensure_docker(sys: &Sys) -> Result<(), String> {
    if !resolves(&sys.docker) {
        return Ok(());
    }
    if docker_up(sys) {
        return Ok(());
    }

    if sys.no_docker_start {
        return Err("Docker is not running and auto-start is disabled (SMOOAI_CI_NO_DOCKER_START)".into());
    }

    let Some(starter) = sys.starters.iter().find(|s| resolves(s)) else {
        return Err("Docker is not running and no orbctl/colima was found to start it".into());
    };

    let limit = sys.docker_wait;
    eprintln!(
        "· Docker is not running — starting it ({}, up to {}s)…",
        Path::new(starter).file_name().unwrap_or(starter.as_os_str()).to_string_lossy(),
        limit.as_secs()
    );
    // Backgrounded: `orbctl start` can outlive the daemon becoming reachable, and
    // the poll below is the real readiness signal either way.
    if let Ok(mut child) = quiet(Command::new(starter).arg("start")).spawn() {
        std::thread::spawn(move || drop(child.wait()));
    }

    let began = Instant::now();
    while began.elapsed() < limit {
        std::thread::sleep(sys.docker_poll);
        if docker_up(sys) {
            eprintln!("· Docker up after {}s — continuing.", began.elapsed().as_secs());
            return Ok(());
        }
    }
    Err(format!("Docker did not come up within {}s", limit.as_secs()))
}

/// th-df8cbd: PR #3727 carried `ci-attest/lint failure "failed locally after 4s"`
/// while CI's own lint job passed in 4m8s — those 4 seconds were
/// `syncpack: command not found` in a worktree nobody had run `pnpm install` in.
/// A red status over that is worse than silence: it red-flags the PR AND sends the
/// row to CI anyway.
///
/// So install rather than refuse. `--frozen-lockfile` keeps this from rewriting
/// the lockfile behind the developer; a lockfile it cannot satisfy IS a broken
/// precondition, and 97 is right for it.
pub fn ensure_node_modules(sys: &Sys, root: &Path) -> Result<(), String> {
    if !root.join("package.json").is_file() || root.join("node_modules").is_dir() {
        return Ok(());
    }

    if !sys.no_install {
        eprintln!("· this worktree has no node_modules — pnpm install --frozen-lockfile…");
        let ok = quiet(Command::new(&sys.pnpm).args(["install", "--frozen-lockfile"]).current_dir(root))
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            eprintln!("· dependencies installed — continuing.");
            return Ok(());
        }
        return Err("pnpm install --frozen-lockfile failed".into());
    }

    Err("this worktree has no node_modules and installing is disabled (SMOOAI_CI_NO_INSTALL)".into())
}

/// Pull the 1-minute figure out of `uptime`, which prints `load average:` on Linux
/// and `load averages:` on macOS. Returned as the raw token so a summary line can
/// echo exactly what the machine said.
pub fn parse_load(uptime_out: &str) -> Option<&str> {
    let tail = uptime_out.find("load average").map(|i| &uptime_out[i..])?;
    let after_colon = tail.find(':').map(|i| &tail[i + 1..])?;
    after_colon
        .split(|c: char| c == ',' || c.is_whitespace())
        .find(|t| !t.is_empty() && t.parse::<f64>().is_ok())
}

fn load_now(sys: &Sys) -> Option<String> {
    let out = Command::new(&sys.uptime).stdin(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    parse_load(&text).map(ToString::to_string)
}

/// th-7e81db: the third member of the family, and the only one that cannot be
/// repaired — only distrusted. `ci-attest/rust` posted `failure` because
/// api-prime's ClickHouse tests unwrap a 600ms timeout, on a box at load average
/// 89 across 12 cores with 17 agents running. The code was fine.
///
/// True only above **twice** the core count. That is deliberately far past
/// "busy": a machine at 2x cores is not slow, it is oversubscribed, and a test
/// with a sub-second timeout is measuring the scheduler.
pub fn overloaded(sys: &Sys) -> bool {
    let Ok(cores) = u32::try_from(sys.cores) else {
        return false;
    };
    cores > 0 && load_now(sys).and_then(|l| l.parse::<f64>().ok()).is_some_and(|l| l > 2.0 * f64::from(cores))
}

pub fn load_summary(sys: &Sys) -> String {
    format!("load {} on {} cores", load_now(sys).unwrap_or_else(|| "?".into()), sys.cores)
}

/// Write an executable shell script and hand back its absolute path. The bash
/// suite stubbed `docker` / `orbctl` / `pnpm` / `uptime` / `ssh` this way; the
/// Rust tests do the same, pointing a [`Sys`] field at the result instead of
/// shadowing a process-global `PATH`.
#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::redundant_pub_crate,
    reason = "test-only helper, shared with the sibling remote:: and mod:: suites"
)]
pub(crate) fn test_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/usr/bin/env bash\n{body}\n")).expect("writing a stub");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod +x a stub");
    p
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::test_script as script;
    use super::*;
    use std::fs;

    fn test_sys() -> Sys {
        Sys {
            cores: 12,
            docker_wait: Duration::from_secs(6),
            docker_poll: Duration::from_millis(50),
            normalize_path: false,
            ..Sys::default()
        }
    }

    // ── load parsing ────────────────────────────────────────────────────────

    #[test]
    fn parses_macos_load_averages() {
        assert_eq!(parse_load("21:30  up 49 mins, 17 users, load averages: 89.04 1.00 1.00"), Some("89.04"));
    }

    #[test]
    fn parses_linux_load_average() {
        assert_eq!(parse_load(" 09:12:01 up 3 days,  2 users,  load average: 0.52, 0.58, 0.59"), Some("0.52"));
    }

    #[test]
    fn parse_load_is_none_without_the_field() {
        assert_eq!(parse_load("21:30 up 49 mins"), None);
    }

    #[test]
    fn overload_is_two_times_cores_not_merely_busy() {
        let tmp = tempfile::tempdir().unwrap();
        let busy = script(tmp.path(), "uptime-busy", "echo 'load averages: 23.00 1 1'");
        let over = script(tmp.path(), "uptime-over", "echo 'load averages: 89.04 1 1'");

        let sys = Sys {
            uptime: busy.into(),
            ..test_sys()
        };
        assert!(!overloaded(&sys), "23 on 12 cores is busy, not oversubscribed");

        let sys = Sys {
            uptime: over.into(),
            ..test_sys()
        };
        assert!(overloaded(&sys));
        assert_eq!(load_summary(&sys), "load 89.04 on 12 cores");
    }

    // ── PATH normalisation (th-92b35a) ──────────────────────────────────────

    #[test]
    fn appends_a_dir_a_stripped_path_missed() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".local/bin")).unwrap();

        let out = normalize_path("/usr/bin:/bin", home.path());
        assert!(env::split_paths(&out).any(|p| p == home.path().join(".local/bin")));
    }

    #[test]
    fn an_explicit_path_entry_still_wins() {
        let home = tempfile::tempdir().unwrap();
        let local = home.path().join(".local/bin");
        fs::create_dir_all(&local).unwrap();

        let out = normalize_path(&format!("{}:/usr/bin", local.display()), home.path());
        let first = env::split_paths(&out).next().unwrap();
        assert_eq!(first, local, "an explicitly-chosen entry must not be shadowed");
    }

    #[test]
    fn normalisation_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".local/bin")).unwrap();

        let once = normalize_path("/usr/bin:/bin", home.path());
        let twice = normalize_path(&once, home.path());
        assert_eq!(once, twice);
        assert_eq!(env::split_paths(&twice).filter(|p| p.ends_with(".local/bin")).count(), 1);
    }

    #[test]
    fn absent_dirs_are_not_added() {
        let home = tempfile::tempdir().unwrap(); // no .cargo/bin, no .local/bin
        let out = normalize_path("/usr/bin", home.path());
        assert!(!env::split_paths(&out).any(|p| p.ends_with(".cargo/bin")));
    }

    // ── docker (th-403a7d / th-98b32b) ──────────────────────────────────────

    /// `docker info` succeeds iff `$flag` exists, so a stubbed starter can flip it
    /// mid-poll — exactly how the bash suite did it.
    fn docker_stubs(dir: &Path, starter_body: &str) -> (PathBuf, PathBuf, PathBuf) {
        let flag = dir.join("up");
        let docker = script(dir, "docker", &format!("[ -f '{}' ]", flag.display()));
        let starter = script(dir, "orbctl", &starter_body.replace("@FLAG@", &flag.display().to_string()));
        (docker, starter, flag)
    }

    #[test]
    fn starts_the_runtime_and_continues() {
        let tmp = tempfile::tempdir().unwrap();
        let (docker, orbctl, flag) = docker_stubs(tmp.path(), "[ \"$1\" = start ] && touch '@FLAG@'");
        let sys = Sys {
            docker: docker.into(),
            starters: vec![orbctl.into()],
            ..test_sys()
        };

        assert_eq!(ensure_docker(&sys), Ok(()));
        assert!(flag.exists(), "the starter actually ran");
    }

    #[test]
    fn exits_precondition_when_the_start_never_lands() {
        let tmp = tempfile::tempdir().unwrap();
        let (docker, orbctl, _) = docker_stubs(tmp.path(), "exit 0");
        let sys = Sys {
            docker: docker.into(),
            starters: vec![orbctl.into()],
            ..test_sys()
        };

        assert!(ensure_docker(&sys).is_err(), "nothing was checked, so nothing may be claimed");
    }

    #[test]
    fn refuses_when_no_starter_is_available() {
        let tmp = tempfile::tempdir().unwrap();
        let (docker, _, _) = docker_stubs(tmp.path(), "exit 0");
        let sys = Sys {
            docker: docker.into(),
            starters: vec![tmp.path().join("nope").into()],
            ..test_sys()
        };

        assert!(ensure_docker(&sys).is_err());
    }

    #[test]
    fn opt_out_refuses_and_starts_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (docker, orbctl, flag) = docker_stubs(tmp.path(), "[ \"$1\" = start ] && touch '@FLAG@'");
        let sys = Sys {
            docker: docker.into(),
            starters: vec![orbctl.into()],
            no_docker_start: true,
            ..test_sys()
        };

        assert!(ensure_docker(&sys).is_err());
        assert!(!flag.exists(), "opt-out must not touch the machine");
    }

    #[test]
    fn no_op_when_docker_is_already_up() {
        let tmp = tempfile::tempdir().unwrap();
        let ran = tmp.path().join("ran");
        let (docker, _, flag) = docker_stubs(tmp.path(), "exit 0");
        fs::write(&flag, "").unwrap();
        let orbctl = script(tmp.path(), "orbctl2", &format!("touch '{}'", ran.display()));
        let sys = Sys {
            docker: docker.into(),
            starters: vec![orbctl.into()],
            ..test_sys()
        };

        assert_eq!(ensure_docker(&sys), Ok(()));
        assert!(!ran.exists(), "the CI case: daemon already up, start nothing");
    }

    #[test]
    fn a_machine_without_docker_is_not_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = Sys {
            docker: tmp.path().join("no-such-docker").into(),
            ..test_sys()
        };
        assert_eq!(ensure_docker(&sys), Ok(()), "no docker installed is not a stopped daemon");
    }

    // ── node_modules (th-df8cbd) ────────────────────────────────────────────

    fn node_repo(dir: &Path) -> PathBuf {
        let root = dir.join("repo");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("package.json"), "{}").unwrap();
        root
    }

    #[test]
    fn installs_deps_and_continues() {
        let tmp = tempfile::tempdir().unwrap();
        let root = node_repo(tmp.path());
        let pnpm = script(tmp.path(), "pnpm", &format!("mkdir -p '{}/node_modules'", root.display()));
        let sys = Sys {
            pnpm: pnpm.into(),
            ..test_sys()
        };

        assert_eq!(ensure_node_modules(&sys, &root), Ok(()));
        assert!(root.join("node_modules").is_dir(), "the install actually ran");
    }

    #[test]
    fn refuses_when_the_install_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let root = node_repo(tmp.path());
        let pnpm = script(tmp.path(), "pnpm", "exit 1");
        let sys = Sys {
            pnpm: pnpm.into(),
            ..test_sys()
        };

        assert!(ensure_node_modules(&sys, &root).is_err(), "an unsatisfiable lockfile IS a broken precondition");
    }

    #[test]
    fn install_opt_out_refuses_and_installs_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = node_repo(tmp.path());
        let pnpm = script(tmp.path(), "pnpm", &format!("mkdir -p '{}/node_modules'", root.display()));
        let sys = Sys {
            pnpm: pnpm.into(),
            no_install: true,
            ..test_sys()
        };

        assert!(ensure_node_modules(&sys, &root).is_err());
        assert!(!root.join("node_modules").exists());
    }

    #[test]
    fn no_op_when_node_modules_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = node_repo(tmp.path());
        fs::create_dir_all(root.join("node_modules")).unwrap();
        let ran = tmp.path().join("pnpm-ran");
        let pnpm = script(tmp.path(), "pnpm", &format!("touch '{}'", ran.display()));
        let sys = Sys {
            pnpm: pnpm.into(),
            ..test_sys()
        };

        assert_eq!(ensure_node_modules(&sys, &root), Ok(()));
        assert!(!ran.exists());
    }

    #[test]
    fn a_repo_without_package_json_needs_no_node_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let pnpm = script(tmp.path(), "pnpm", "exit 1");
        let sys = Sys {
            pnpm: pnpm.into(),
            ..test_sys()
        };

        assert_eq!(ensure_node_modules(&sys, tmp.path()), Ok(()));
    }

    #[test]
    fn env_flags_accept_both_spellings() {
        // Exercised without mutating the environment: both keys are consulted.
        assert!(!env_flag("DEFINITELY_UNSET_KNOB"));
        assert_eq!(env_u64("DEFINITELY_UNSET_KNOB"), None);
    }

    #[test]
    fn remote_prelude_appends_the_same_dirs() {
        let prelude = remote_path_prelude();
        for dir in ["/opt/homebrew/bin", "/usr/local/bin", ".cargo/bin", ".local/bin"] {
            assert!(prelude.contains(dir), "remote shells lose {dir} too");
        }
        assert!(prelude.contains("$PATH:"), "appended, never prepended");
    }
}
