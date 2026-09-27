//! `th ci-queue` — run git-hook and CI checks through a machine-wide,
//! capacity-aware queue (SMOODEV-3355).
//!
//! On 2026-09-26 about 35 agent sessions on one 12-core Mac each ran their
//! pre-commit checks (turbo typecheck of dependents at concurrency 10, full
//! cargo fmt + clippy) at full priority, all at once. Load average hit 1,022,
//! swap filled, and Chrome froze. This is the durable fix: every heavy check
//! on the machine takes a numbered slot first, waits its turn in FIFO order,
//! waits longer while the machine is under memory, swap, load or disk
//! pressure, and then runs at background QoS.
//!
//! There is no daemon. Coordination is kernel `flock`s on files under
//! `~/.smooth/ci-queue/` (see `queue`), so a crashed or `kill -9`'d job
//! releases its slot the instant it dies.
//!
//! | Exit | Meaning                                                   |
//! | ---- | --------------------------------------------------------- |
//! | n    | the job exited n                                          |
//! | 128+n| the job died on signal n                                  |
//! | 124  | the job's `--timeout` fired; its process group was killed |
//! | 127  | the job could not be started                              |
//! | 75   | no slot within `--max-wait` (EX_TEMPFAIL) — nothing ran   |
//! | 71   | the queue itself is unusable (EX_OSERR) — nothing ran     |

pub mod config;
pub mod exec;
pub mod pressure;
pub mod queue;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Args, Subcommand};

pub use queue::{AdmitError, Class, Queue, Request};

/// No slot freed within `--max-wait`. EX_TEMPFAIL: try again later.
pub const EXIT_WAIT_TIMEOUT: i32 = 75;

/// The queue directory is unusable. EX_OSERR.
pub const EXIT_QUEUE_ERROR: i32 = 71;

#[derive(Subcommand, Debug)]
pub enum CiQueueCmd {
    /// Wait for a slot, then run a command in it.
    ///
    /// Heavy jobs share `slots.heavy` machine-wide slots (default 2) and are
    /// also held while the machine is under memory, swap, load or disk
    /// pressure — except that one is always admitted when no other heavy job
    /// is running. Light jobs share `slots.light` (default 6) and ignore
    /// pressure. Waiters are served first-come, first-served. The job runs at
    /// background QoS in its own process group; INT/TERM/HUP are forwarded to
    /// it. Exit: the job's code (128+n on signal n), 124 when `--timeout`
    /// fires, 75 when no slot frees within `--max-wait`.
    Run(RunArgs),
    /// Running jobs, the queue, current pressure vs thresholds, and recent history.
    Status(StatusArgs),
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Which slot pool to queue in.
    #[arg(long, value_enum, default_value_t = Class::Heavy)]
    pub class: Class,

    /// Name shown in `status` and in other jobs' waiting lines (default: the command).
    #[arg(long)]
    pub label: Option<String>,

    /// Kill the job's whole process group after this many seconds of RUNNING.
    /// The clock starts at admission, never while queued. 0 = no limit.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,

    /// Give up (exit 75) after waiting this long for a slot. Default: `run.max_wait_secs` (1800).
    #[arg(long, value_name = "SECS")]
    pub max_wait: Option<u64>,

    /// A machine-shared resource the job will hold, e.g. `cargo` (the cargo
    /// target dir). Jobs naming the same resource never run together, a cargo
    /// build started outside the queue also holds a `cargo` job, and a job
    /// holding any lock never runs below `nice` priority. Repeatable. Added
    /// automatically when the command itself is `cargo`.
    #[arg(long = "lock", value_name = "NAME")]
    pub locks: Vec<String>,

    /// Priority: `background` (default; macOS background QoS), `nice`, or
    /// `normal`. A job with a `--lock` is capped at `nice`.
    #[arg(long, value_enum)]
    pub qos: Option<config::Qos>,

    /// Run the command here instead of the current directory.
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// The command, after `--`.
    #[arg(last = true, required = true, value_name = "CMD")]
    pub cmd: Vec<OsString>,
}

#[derive(Args, Debug, Default)]
pub struct StatusArgs {
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,

    /// How many finished jobs to show.
    #[arg(long, default_value_t = 10)]
    pub history: usize,
}

pub fn cmd(cmd: Option<CiQueueCmd>) -> Result<()> {
    let q = Queue::from_env();
    let code = match cmd {
        Some(CiQueueCmd::Run(a)) => run(&q, &a),
        Some(CiQueueCmd::Status(a)) => status(&q, &a)?,
        None => status(&q, &StatusArgs::default())?,
    };
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

pub fn run(q: &Queue, a: &RunArgs) -> i32 {
    let cwd = match a.cwd.clone().map_or_else(std::env::current_dir, Ok) {
        Ok(d) => std::path::absolute(&d).unwrap_or(d),
        Err(e) => {
            eprintln!("th ci-queue: no usable working directory ({e})");
            eprintln!("  pass --cwd DIR");
            return EXIT_QUEUE_ERROR;
        }
    };
    let label = a.label.clone().unwrap_or_else(|| default_label(&a.cmd));
    let max_wait = Duration::from_secs(a.max_wait.unwrap_or(q.config.run.max_wait_secs));
    let locks = job_locks(q, &a.locks, &a.cmd, &cwd);
    let qos = a.qos.unwrap_or(q.config.run.qos).effective(!locks.is_empty());
    let req = Request {
        class: a.class,
        label: label.clone(),
        cwd: cwd.clone(),
        max_wait,
        locks,
    };
    let mut admission = match q.admit(&req) {
        Ok(adm) => adm,
        Err(AdmitError::WaitTimeout { waited, reason }) => {
            eprintln!(
                "th ci-queue: {label} gave up after {}s without a {} slot ({reason})",
                waited.as_secs(),
                a.class.name()
            );
            eprintln!("  nothing ran. See who holds the slots with `th ci-queue status`, or raise --max-wait.");
            return EXIT_WAIT_TIMEOUT;
        }
        Err(AdmitError::Queue(e)) => {
            eprintln!("th ci-queue: the queue at {} is unusable: {e:#}", q.dir().display());
            eprintln!("  nothing ran. Check free disk and permissions there (SMOOTH_CI_QUEUE_DIR moves it).");
            return EXIT_QUEUE_ERROR;
        }
    };
    let timeout = a.timeout.filter(|t| *t > 0).map(Duration::from_secs);
    let began = Instant::now();
    let ended = exec::run(
        &exec::Spec {
            argv: &a.cmd,
            cwd: &cwd,
            env: vec![(queue::NESTED_ENV.into(), admission.slot_name().into())],
            timeout,
            qos,
            kill_grace: Duration::from_secs(q.config.run.kill_grace_secs),
            null_stdin: false,
        },
        |pid| admission.set_child(pid),
    );
    if ended == exec::Ended::TimedOut {
        eprintln!(
            "th ci-queue: {label} timed out after {}s of running — killed its process group",
            timeout.map_or(0, |t| t.as_secs())
        );
    }
    admission.finish(ended, began.elapsed());
    ended.exit_code()
}

/// The resolved `--lock`s, plus `cargo` when the command is cargo itself.
fn job_locks(q: &Queue, named: &[String], cmd: &[OsString], cwd: &Path) -> Vec<String> {
    let mut names: Vec<&str> = named.iter().map(String::as_str).collect();
    let runs_cargo = cmd.first().and_then(|c| Path::new(c).file_name()).is_some_and(|n| n == "cargo");
    if runs_cargo && !names.contains(&"cargo") {
        names.push("cargo");
    }
    let mut out: Vec<String> = names.iter().map(|n| q.resolve_lock(n, cwd)).collect();
    out.sort();
    out.dedup();
    out
}

/// `pnpm turbo typecheck …` — the command, trimmed for a one-line display.
fn default_label(cmd: &[OsString]) -> String {
    let mut parts: Vec<String> = cmd.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    if let Some(first) = parts.first_mut() {
        if let Some(name) = Path::new(first.as_str()).file_name() {
            *first = name.to_string_lossy().into_owned();
        }
    }
    let joined = parts.join(" ");
    if joined.chars().count() > 60 {
        format!("{}…", joined.chars().take(59).collect::<String>())
    } else {
        joined
    }
}

fn status(q: &Queue, a: &StatusArgs) -> Result<i32> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let snap = q.snapshot(a.history, &cwd)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&snap)?);
        return Ok(0);
    }
    print!("{}", render(&snap));
    Ok(0)
}

fn secs_since(now_ms: u64, then_ms: u64) -> u64 {
    now_ms.saturating_sub(then_ms) / 1000
}

fn dur(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn with_locks(j: &queue::JobInfo) -> String {
    if j.locks.is_empty() {
        j.label.clone()
    } else {
        format!("{} [{}]", j.label, j.locks.join(", "))
    }
}

fn render(s: &queue::Snapshot) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let count = |c: Class| s.running.iter().filter(|j| j.class == c).count();
    let _ = writeln!(
        o,
        "ci-queue  {}  ·  heavy {}/{}  ·  light {}/{}",
        s.dir.display(),
        count(Class::Heavy),
        s.config.slots.heavy,
        count(Class::Light),
        s.config.slots.light
    );

    let _ = writeln!(o, "\nRunning");
    if s.running.is_empty() {
        let _ = writeln!(o, "  (nothing running)");
    }
    for j in &s.running {
        let _ = writeln!(
            o,
            "  ● {:<8} {:<32} pid {:<7} {:>7}  {}",
            format!("{}-{}", j.class.name(), j.slot.unwrap_or(0)),
            with_locks(j),
            j.child_pid.unwrap_or(j.pid),
            dur(j.admitted_at_ms.map_or(0, |t| secs_since(s.now_ms, t))),
            j.cwd.display()
        );
    }

    let _ = writeln!(o, "\nWaiting");
    if s.waiting.is_empty() {
        let _ = writeln!(o, "  (nobody waiting)");
    }
    for j in &s.waiting {
        let _ = writeln!(
            o,
            "  ○ #{:<6} {:<5} {:<32} pid {:<7} {:>7}  {}",
            j.ticket,
            j.class.name(),
            with_locks(j),
            j.pid,
            dur(secs_since(s.now_ms, j.queued_at_ms)),
            j.cwd.display()
        );
    }

    let g = &s.config.gate;
    let r = &s.readings;
    let _ = writeln!(o, "\nPressure (holds new heavy jobs while another is running; 0 = off)");
    let row = |o: &mut String, name: &str, now: String, limit: String, on: bool| {
        let _ = writeln!(o, "  {:<18} {:<30} {limit}", name, if on { now } else { format!("{now} (off)") });
    };
    let na = || "unknown".to_string();
    row(
        &mut o,
        "memory available",
        r.mem_available_pct().map_or_else(na, |p| format!("{p:.0}%")),
        format!("hold below {}%", g.min_available_memory_pct),
        g.min_available_memory_pct > 0.0,
    );
    if cfg!(target_os = "macos") {
        row(
            &mut o,
            "memory pressure",
            r.memory_pressure_level
                .map_or_else(na, |l| format!("{} (level {l})", pressure::pressure_name(l))),
            format!("hold above level {}", g.max_memory_pressure_level),
            g.max_memory_pressure_level > 0,
        );
    }
    row(
        &mut o,
        "swap used",
        r.swap_used_pct().map_or_else(na, |p| format!("{p:.0}%")),
        format!("hold above {}% while memory is tight", g.max_swap_used_pct),
        g.max_swap_used_pct > 0.0,
    );
    row(
        &mut o,
        "load (1m)",
        r.load1
            .map_or_else(na, |l| format!("{l:.1} on {} cores = {:.1}/core", r.cores, r.load_per_core().unwrap_or(0.0))),
        format!("hold above {}/core", g.max_load_per_core),
        g.max_load_per_core > 0.0,
    );
    for d in &r.disks {
        #[allow(clippy::cast_precision_loss, reason = "GB display")]
        let now = d.free_bytes.map_or_else(na, |b| format!("{:.0} GB free", b as f64 / 1_073_741_824.0));
        row(
            &mut o,
            "disk",
            format!("{now} at {}", d.path.display()),
            format!("hold below {} GB", g.min_free_disk_gb),
            g.min_free_disk_gb > 0.0,
        );
    }
    let _ = writeln!(
        o,
        "  → {}",
        if s.holds.is_empty() {
            "clear: a new heavy job would be admitted if a slot is free".to_string()
        } else {
            format!("HOLDING new heavy jobs: {}", s.holds.join(" / "))
        }
    );

    let _ = writeln!(o, "\nRecent");
    if s.history.is_empty() {
        let _ = writeln!(o, "  (no finished jobs yet)");
    }
    for h in s.history.iter().rev() {
        let when = chrono::DateTime::from_timestamp_millis(i64::try_from(h.queued_at_ms).unwrap_or(0))
            .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let _ = writeln!(
            o,
            "  {when}  {:<5} {:<32} waited {:>7}  ran {:>7}  {} {}",
            h.class.name(),
            h.label,
            dur(h.wait_ms / 1000),
            dur(h.run_ms / 1000),
            h.outcome,
            h.exit
        );
    }
    o
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn default_label_is_the_command_trimmed() {
        let cmd: Vec<OsString> = ["/opt/homebrew/bin/pnpm", "turbo", "typecheck"].iter().map(Into::into).collect();
        assert_eq!(default_label(&cmd), "pnpm turbo typecheck");
        let long: Vec<OsString> = vec!["x".into(); 80];
        let l = default_label(&long);
        assert_eq!(l.chars().count(), 60);
        assert!(l.ends_with('…'));
    }

    #[test]
    fn a_cargo_command_takes_the_cargo_lock_by_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let q = Queue::at(tmp.path().join("q"), config::Config::default(), std::sync::Arc::new(Quiet)).with_cargo_target("/t".into());
        let cmd: Vec<OsString> = ["/usr/bin/cargo", "test", "-p", "x"].iter().map(Into::into).collect();
        let locks = job_locks(&q, &[], &cmd, Path::new("/repo"));
        assert_eq!(locks, vec!["cargo:/t".to_string()]);
        assert_eq!(job_locks(&q, &["cargo".into()], &cmd, Path::new("/repo")), locks, "named twice is one lock");
        let tsc: Vec<OsString> = ["pnpm", "typecheck"].iter().map(Into::into).collect();
        assert!(job_locks(&q, &[], &tsc, Path::new("/repo")).is_empty());
        assert_eq!(job_locks(&q, &["docker".into()], &tsc, Path::new("/repo")), vec!["docker".to_string()]);
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(dur(5), "5s");
        assert_eq!(dur(125), "2m05s");
        assert_eq!(dur(3 * 3600 + 7 * 60), "3h07m");
    }

    #[test]
    fn status_renders_every_section_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let q = Queue::at(tmp.path().join("q"), config::Config::default(), std::sync::Arc::new(Quiet));
        let text = render(&q.snapshot(5, tmp.path()).unwrap());
        for want in [
            "heavy 0/2",
            "light 0/6",
            "(nothing running)",
            "(nobody waiting)",
            "load (1m)",
            "→ clear",
            "(no finished jobs yet)",
        ] {
            assert!(text.contains(want), "missing {want:?}:\n{text}");
        }
    }

    struct Quiet;
    impl pressure::Probe for Quiet {
        fn read(&self, _: &[PathBuf]) -> pressure::Readings {
            pressure::Readings {
                load1: Some(1.0),
                cores: 4,
                ..pressure::Readings::default()
            }
        }
    }
}
