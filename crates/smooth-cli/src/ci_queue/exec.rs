//! Running an admitted job: background QoS, its own process group, signals
//! forwarded to that group, and the whole group killed on timeout.
//!
//! The job runs as a CHILD of `th` rather than replacing it, because the slot's
//! flock lives in this process: it must outlive the job and die with `th`,
//! never with a daemon the job happened to fork.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::config::Qos;

/// What the job did, as far as `th` can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Exited(i32),
    /// Killed by signal `n` (not by our timeout).
    Signaled(i32),
    /// Our `--timeout` fired and the group was killed.
    TimedOut,
    /// The command could not be started at all.
    SpawnFailed,
}

/// `th ci-queue run`'s exit code for a job its `--timeout` killed — the same
/// code GNU `timeout` uses, so hook scripts that already handle one handle both.
pub const EXIT_TIMEOUT: i32 = 124;

/// The shell's "command not found / not executable".
pub const EXIT_SPAWN_FAILED: i32 = 127;

impl Ended {
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Exited(c) => c,
            Self::Signaled(n) => 128 + n,
            Self::TimedOut => EXIT_TIMEOUT,
            Self::SpawnFailed => EXIT_SPAWN_FAILED,
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::Exited(_) => "exit",
            Self::Signaled(_) => "signal",
            Self::TimedOut => "timeout",
            Self::SpawnFailed => "spawn-failed",
        }
    }
}

pub struct Spec<'a> {
    pub argv: &'a [OsString],
    pub cwd: &'a Path,
    pub env: Vec<(OsString, OsString)>,
    /// Starts when the job starts — i.e. after admission, never while queued.
    pub timeout: Option<Duration>,
    /// Already resolved with [`Qos::effective`].
    pub qos: Qos,
    pub kill_grace: Duration,
    /// Always give the job an empty stdin. Otherwise it gets ours, unless ours
    /// is a terminal.
    pub null_stdin: bool,
    /// Sample the job's process-group memory this often (see `sampler`).
    pub sample_every: Option<Duration>,
}

/// What a job used, for history and future estimates. Each field is `None`
/// when it could not be measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub peak_group_rss_kb: Option<u64>,
    pub max_single_rss_kb: Option<u64>,
    pub cpu_ms: Option<u64>,
}

/// Run `spec` to completion. `on_spawn` gets the job's pid (== its process
/// group id on Unix) as soon as it exists.
#[cfg(test)]
pub fn run(spec: &Spec<'_>, on_spawn: impl FnOnce(u32)) -> Ended {
    run_measured(spec, on_spawn, None::<fn(u64)>).0
}

/// [`run`], and measure the job: its process group's peak total memory
/// (sampled every `spec.sample_every`, each total also handed to `on_sample`)
/// and its CPU time.
pub fn run_measured<S: FnMut(u64) + Send + 'static>(spec: &Spec<'_>, on_spawn: impl FnOnce(u32), on_sample: Option<S>) -> (Ended, Usage) {
    let Some((program, args)) = spec.argv.split_first() else {
        return (Ended::SpawnFailed, Usage::default());
    };
    let prefix = qos_prefix(spec.qos);
    let mut cmd = if let Some((wrapper, wrapper_args)) = prefix.split_first() {
        let mut c = Command::new(wrapper);
        c.args(wrapper_args).arg(program).args(args);
        c
    } else {
        let mut c = Command::new(program);
        c.args(args);
        c
    };
    cmd.current_dir(spec.cwd).envs(spec.env.iter().map(|(k, v)| (k, v)));
    // A job in its own process group is not in the terminal's foreground
    // group, so reading a tty would stop it (SIGTTIN). Hand it the tty's stdin
    // only when stdin is not a tty.
    if spec.null_stdin || std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        cmd.stdin(Stdio::null());
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);

    #[cfg(unix)]
    signals::install();
    let before = super::sampler::children_usage();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("th ci-queue: cannot start {}: {e}", program.to_string_lossy());
            return (Ended::SpawnFailed, Usage::default());
        }
    };
    let pid = child.id();
    #[cfg(unix)]
    signals::forward_to(pid);
    on_spawn(pid);
    let sampler = spec.sample_every.filter(|_| cfg!(unix)).map(|tick| {
        let mut on_sample = on_sample;
        super::sampler::Sampler::start(pid, tick, move |kb| {
            if let Some(f) = on_sample.as_mut() {
                f(kb);
            }
        })
    });

    let (tx, rx) = mpsc::channel::<std::io::Result<ExitStatus>>();
    let waiter = std::thread::spawn(move || {
        let status = child.wait();
        #[cfg(unix)]
        signals::clear();
        let _ = tx.send(status);
    });

    let mut timed_out = false;
    #[allow(clippy::option_if_let_else, reason = "each branch also records the timeout and kills the group")]
    let status = if let Some(limit) = spec.timeout {
        if let Ok(s) = rx.recv_timeout(limit) {
            Some(s)
        } else {
            timed_out = true;
            kill_group(pid, false);
            if let Ok(s) = rx.recv_timeout(spec.kill_grace) {
                Some(s)
            } else {
                kill_group(pid, true);
                rx.recv().ok()
            }
        }
    } else {
        rx.recv().ok()
    };
    if timed_out {
        // The leader is gone; anything it left in the group goes too.
        kill_group(pid, true);
    }
    let _ = waiter.join();
    let peak = sampler.map(super::sampler::Sampler::stop);
    let after = super::sampler::children_usage();
    // RUSAGE_CHILDREN is cumulative over this process's lifetime (th attest
    // runs several jobs), so CPU is a difference, and ru_maxrss — a running
    // max — only says something about this job when it grew. It also counts
    // any OTHER child this process reaped meanwhile: none for `ci-queue run`;
    // for `th attest`, the ssh of a concurrent remote check (negligible CPU).
    let (cpu_ms, rusage_single) = match (before, after) {
        (Some((c0, m0)), Some((c1, m1))) => (Some(c1.saturating_sub(c0)), (m1 > m0).then_some(m1)),
        _ => (None, None),
    };
    let usage = Usage {
        peak_group_rss_kb: peak.map(|p| p.total_kb).filter(|kb| *kb > 0),
        max_single_rss_kb: match (peak.map(|p| p.largest_kb).filter(|kb| *kb > 0), rusage_single) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        },
        cpu_ms,
    };

    if timed_out {
        return (Ended::TimedOut, usage);
    }
    let ended = match status {
        Some(Ok(s)) => classify(s),
        _ => Ended::SpawnFailed,
    };
    (ended, usage)
}

fn classify(s: ExitStatus) -> Ended {
    if let Some(c) = s.code() {
        return Ended::Exited(c);
    }
    #[cfg(unix)]
    if let Some(n) = std::os::unix::process::ExitStatusExt::signal(&s) {
        return Ended::Signaled(n);
    }
    Ended::Exited(1)
}

/// The wrapper that puts the job at `qos`. Each tool is used only if present.
///
/// `Background`: `taskpolicy -b` on macOS (CPU, disk and network throttled
/// behind anything interactive); `ionice -c 3` + `nice -n 10` on Linux.
/// `Nice`: `nice -n 10` everywhere — lower priority that cannot starve, which
/// is what a job holding a shared lock needs (see `Qos`).
pub fn qos_prefix(qos: Qos) -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::new();
    let nice = |out: &mut Vec<OsString>| {
        if let Some(nice) = on_path("nice") {
            out.extend([nice, "-n".into(), "10".into()]);
        }
    };
    match qos {
        Qos::Normal => {}
        Qos::Nice => nice(&mut out),
        Qos::Background if cfg!(target_os = "macos") => {
            if Path::new("/usr/sbin/taskpolicy").exists() {
                out.extend(["/usr/sbin/taskpolicy".into(), "-b".into()]);
            }
        }
        Qos::Background => {
            if cfg!(target_os = "linux") {
                if let Some(ionice) = on_path("ionice") {
                    // `-t`: carry on at normal I/O priority if the idle class is refused.
                    out.extend([ionice, "-c".into(), "3".into(), "-t".into()]);
                }
            }
            nice(&mut out);
        }
    }
    out
}

fn on_path(name: &str) -> Option<OsString> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file()).map(Into::into)
}

#[cfg(unix)]
fn kill_group(pgid: u32, hard: bool) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    let Ok(raw) = i32::try_from(pgid) else {
        return;
    };
    let _ = killpg(Pid::from_raw(raw), if hard { Signal::SIGKILL } else { Signal::SIGTERM });
}

#[cfg(not(unix))]
fn kill_group(pid: u32, _hard: bool) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// INT / TERM / HUP to `th` go to the running job's whole process group. With
/// no job running they do what they always did — terminate `th` — which
/// releases any slot or ticket it holds.
#[cfg(unix)]
mod signals {
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::OnceLock;

    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    static CURRENT: AtomicI32 = AtomicI32::new(0);
    static INSTALLED: OnceLock<()> = OnceLock::new();

    pub fn install() {
        INSTALLED.get_or_init(|| {
            let Ok(mut signals) = Signals::new([SIGINT, SIGTERM, SIGHUP]) else {
                return;
            };
            std::thread::spawn(move || {
                for sig in signals.forever() {
                    let pg = CURRENT.load(Ordering::SeqCst);
                    if pg > 0 {
                        if let Ok(s) = Signal::try_from(sig) {
                            let _ = killpg(Pid::from_raw(pg), s);
                        }
                    } else {
                        let _ = signal_hook::low_level::emulate_default_handler(sig);
                    }
                }
            });
        });
    }

    pub fn forward_to(pgid: u32) {
        CURRENT.store(i32::try_from(pgid).unwrap_or(0), Ordering::SeqCst);
    }

    pub fn clear() {
        CURRENT.store(0, Ordering::SeqCst);
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use std::time::Instant;

    fn sh(script: &str) -> Vec<OsString> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    fn spec<'a>(argv: &'a [OsString], cwd: &'a Path, timeout_ms: Option<u64>) -> Spec<'a> {
        Spec {
            argv,
            cwd,
            env: vec![("CIQ_TEST".into(), "yes".into())],
            timeout: timeout_ms.map(Duration::from_millis),
            // Normal, not Background: at load 70+ Darwin background QoS can
            // starve a test job past its own timeout (seen: the grandchild pid
            // was never written within 300ms). The QoS prefixes are covered
            // by `nice_never_uses_background_qos_or_idle_io`.
            qos: Qos::Normal,
            kill_grace: Duration::from_millis(500),
            null_stdin: true,
            sample_every: None,
        }
    }

    #[test]
    fn exit_codes_pass_through() {
        let tmp = tempfile::tempdir().unwrap();
        for code in [0, 1, 3, 97] {
            let argv = sh(&format!("exit {code}"));
            assert_eq!(run(&spec(&argv, tmp.path(), None), |_| {}), Ended::Exited(code));
        }
    }

    #[test]
    fn a_signal_death_is_128_plus_n() {
        let tmp = tempfile::tempdir().unwrap();
        let argv = sh("kill -KILL $$");
        let ended = run(&spec(&argv, tmp.path(), None), |_| {});
        assert_eq!(ended, Ended::Signaled(9));
        assert_eq!(ended.exit_code(), 137);
    }

    #[test]
    fn runs_in_cwd_with_env_in_its_own_process_group() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let argv = sh(&format!(
            "echo \"$(pwd -P) $CIQ_TEST $(ps -o pgid= -p $$ | tr -d ' ') $$\" > '{}'",
            out.display()
        ));
        let mut spawned = 0;
        assert_eq!(run(&spec(&argv, tmp.path(), None), |p| spawned = p), Ended::Exited(0));
        let text = std::fs::read_to_string(&out).unwrap();
        let f: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(f[0], tmp.path().canonicalize().unwrap().to_str().unwrap());
        assert_eq!(f[1], "yes");
        assert_eq!(f[2], f[3], "the job is not its own process-group leader");
        assert_eq!(f[3], spawned.to_string());
    }

    #[test]
    fn the_timeout_kills_the_whole_group() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("grandchild");
        // The grandchild ignores nothing and outlives its parent unless the
        // GROUP is killed.
        let argv = sh(&format!("sleep 30 & echo $! > '{}'; wait", pidfile.display()));
        let began = Instant::now();
        // Long enough for sh to write the pid on a loaded machine.
        let ended = run(&spec(&argv, tmp.path(), Some(3_000)), |_| {});
        assert_eq!(ended, Ended::TimedOut);
        assert_eq!(ended.exit_code(), EXIT_TIMEOUT);
        assert!(began.elapsed() < Duration::from_secs(15));
        let gc: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        let alive = || nix::sys::signal::kill(nix::unistd::Pid::from_raw(gc), None).is_ok();
        let t = Instant::now();
        while alive() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(), "grandchild {gc} survived the timeout");
    }

    #[test]
    fn a_job_that_ignores_term_is_killed_after_the_grace() {
        let tmp = tempfile::tempdir().unwrap();
        let argv = sh("trap '' TERM; while :; do sleep 0.05; done");
        let began = Instant::now();
        assert_eq!(run(&spec(&argv, tmp.path(), Some(200)), |_| {}), Ended::TimedOut);
        assert!(began.elapsed() < Duration::from_secs(10));
    }

    /// The priority-inversion rule, at the level of what actually gets exec'd:
    /// `Nice` must never smuggle in Darwin background QoS or the idle I/O class.
    #[test]
    fn nice_never_uses_background_qos_or_idle_io() {
        let nice: Vec<String> = qos_prefix(Qos::Nice).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(!nice.iter().any(|a| a.contains("taskpolicy") || a.contains("ionice")), "{nice:?}");
        assert!(nice.iter().any(|a| a.ends_with("nice")), "{nice:?}");
        assert!(qos_prefix(Qos::Normal).is_empty());
        #[cfg(target_os = "macos")]
        assert_eq!(qos_prefix(Qos::Background), vec![OsString::from("/usr/sbin/taskpolicy"), "-b".into()]);
    }

    /// The whole point of sampling the GROUP: two parallel memory hogs peak at
    /// about twice the largest single one together. `ru_maxrss` alone would
    /// report one hog.
    #[test]
    fn measures_the_group_total_not_just_the_largest_process() {
        let tmp = tempfile::tempdir().unwrap();
        let hog = "perl -e '$x = q(a) x (100*1024*1024); sleep 2'";
        let argv = sh(&format!("{hog} & {hog} & wait"));
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let seen2 = seen.clone();
        let s = Spec {
            sample_every: Some(Duration::from_millis(200)),
            ..spec(&argv, tmp.path(), None)
        };
        let (ended, u) = run_measured(
            &s,
            |_| {},
            Some(move |kb: u64| {
                seen2.fetch_max(kb, std::sync::atomic::Ordering::SeqCst);
            }),
        );
        assert_eq!(ended, Ended::Exited(0));
        let (group, single) = (u.peak_group_rss_kb.unwrap_or(0), u.max_single_rss_kb.unwrap_or(0));
        assert!(single >= 100 * 1024, "largest single {single} KiB: {u:?}");
        // Two hogs at once: the group peak is near 2x the largest one.
        assert!(group * 10 >= single * 17, "group {group} KiB is not ~2x single {single} KiB: {u:?}");
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), group, "on_sample never saw the peak");
        assert!(u.cpu_ms.is_some());
    }

    /// Lower bound only: RUSAGE_CHILDREN is per PROCESS, and this test binary
    /// runs other tests' children in parallel, so an upper bound here would
    /// measure the neighbours. (`th ci-queue run` has exactly one job.)
    #[test]
    fn cpu_time_is_measured() {
        let tmp = tempfile::tempdir().unwrap();
        let argv = sh("perl -e '$t = time; 1 while time - $t < 2'");
        let (_, u) = run_measured(&spec(&argv, tmp.path(), None), |_| {}, None::<fn(u64)>);
        let cpu = u.cpu_ms.unwrap();
        assert!(cpu >= 500, "a ~2s busy loop reported {cpu} ms of CPU");
    }

    /// A sampler must never keep `th` (or the job's slot) waiting: `run`
    /// returns promptly after the job exits, however long the tick.
    #[test]
    fn a_long_sample_tick_does_not_delay_the_return() {
        let tmp = tempfile::tempdir().unwrap();
        let argv = sh("sleep 0.3");
        let s = Spec {
            sample_every: Some(Duration::from_secs(60)),
            ..spec(&argv, tmp.path(), None)
        };
        let t = Instant::now();
        let (ended, u) = run_measured(&s, |_| {}, None::<fn(u64)>);
        assert_eq!(ended, Ended::Exited(0));
        assert!(t.elapsed() < Duration::from_secs(10), "run waited {:?} on the sampler", t.elapsed());
        assert!(u.peak_group_rss_kb.is_some(), "the first sample is taken at once");
    }

    #[test]
    fn a_missing_program_is_spawn_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let argv: Vec<OsString> = vec!["/definitely/not/a/program".into()];
        let s = Spec {
            qos: Qos::Normal,
            ..spec(&argv, tmp.path(), None)
        };
        assert_eq!(run(&s, |_| {}).exit_code(), EXIT_SPAWN_FAILED);
    }
}
