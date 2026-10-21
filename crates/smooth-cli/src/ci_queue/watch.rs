//! Who is really using the CPU: the process table, a stall watchdog for running
//! jobs, and attribution of machine-wide CPU hogs to queue jobs.
//!
//! **The incident (2026-10-19).** Three heavy jobs sat in their slots for 173
//! minutes with ~2.3 CPU-minutes of process-group CPU each, while helpers they
//! had spawned (turbo daemon, esbuild service, a build script) spun at 90–270%
//! CPU *outside* the job's process group. The gate saw "cpu 100% busy", cut the
//! budget to its floor and held every new job; nothing ever freed the slots.
//!
//! **Idle means the whole tree is idle.** A job is judged by the CPU of its
//! process TREE — the pid, every descendant found by walking `ppid` (whatever
//! group or session they moved to), and its process group — never the group
//! alone, so a job whose real work runs in a detached child is not mistaken for
//! idle. Daemons that were re-parented to launchd leave the tree; their CPU is
//! *not* the job's work, which is exactly why such a job is a stall.
//!
//! **Attribution.** A hog that is not in any job's tree can still be traced to a
//! job: every job's environment carries [`TICKET_ENV`], which daemons inherit.
//!
//! Everything that parses or decides is pure, so the tests feed it fake process
//! tables with detached children; only [`snapshot`], [`env_lines`] and
//! [`cwd_of`] touch the machine.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Set in every job's environment (and so inherited by whatever it forks or
/// daemonizes): the job's ticket number. Unique per queue, unlike the slot name.
pub const TICKET_ENV: &str = "SMOOTH_CI_QUEUE_TICKET";

/// One row of the process table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    /// `ps`'s decayed %CPU: fine for ranking, not for accounting.
    pub pcpu: f64,
    /// Cumulative CPU time, user + system.
    pub cpu_ms: u64,
    pub rss_kb: u64,
    pub command: String,
}

/// `[[d-]hh:]mm:ss[.cc]` (what `ps -o time=` prints) in milliseconds.
pub fn parse_cpu_time(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let mut secs = 0.0_f64;
    for part in rest.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "non-negative, well within u64")]
    Some(days * 86_400_000 + (secs * 1000.0).round().max(0.0) as u64)
}

/// Parse `ps -A -o pid= -o ppid= -o pgid= -o pcpu= -o time= -o rss= -o command=`.
pub fn parse_ps(out: &str) -> Vec<Proc> {
    out.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let pgid = it.next()?.parse().ok()?;
            let pcpu = it.next()?.parse().ok()?;
            let cpu_ms = parse_cpu_time(it.next()?)?;
            let rss_kb = it.next()?.parse().ok()?;
            Some(Proc {
                pid,
                ppid,
                pgid,
                pcpu,
                cpu_ms,
                rss_kb,
                command: it.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

/// The machine's process table, or `None` if `ps` could not be run.
pub fn snapshot() -> Option<Vec<Proc>> {
    let out = Command::new("ps")
        .args([
            "-A", "-o", "pid=", "-o", "ppid=", "-o", "pgid=", "-o", "pcpu=", "-o", "time=", "-o", "rss=", "-o", "command=",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| parse_ps(&String::from_utf8_lossy(&out.stdout)))
}

/// `root`, everything under it by `ppid` (any group or session), and every
/// process in the group `root` leads. Never includes `exclude` (our own pid).
pub fn tree(procs: &[Proc], root: u32, exclude: u32) -> Vec<&Proc> {
    let mut children: HashMap<u32, Vec<&Proc>> = HashMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p);
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut out = Vec::new();
    let mut todo: Vec<u32> = vec![root];
    while let Some(pid) = todo.pop() {
        if pid == exclude || !seen.insert(pid) {
            continue;
        }
        if let Some(p) = procs.iter().find(|p| p.pid == pid) {
            out.push(p);
        }
        for c in children.get(&pid).into_iter().flatten() {
            todo.push(c.pid);
        }
    }
    for p in procs.iter().filter(|p| p.pgid == root && p.pid != exclude) {
        if seen.insert(p.pid) {
            out.push(p);
        }
    }
    out
}

/// What the watchdog concludes after a sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Using CPU (or not observed long enough to say).
    Busy,
    /// Idle for this long so far, not yet long enough to kill.
    Idle(Duration),
    /// Idle for the whole window: kill it.
    Stalled,
}

/// Tracks the CPU a job's process tree gains over time.
///
/// CPU is accounted per pid (cumulative CPU time diffed between samples), so
/// children that exit and new ones that start neither hide activity nor invent
/// it: a new pid's whole CPU time counts as gained, a vanished pid contributes
/// nothing further.
pub struct StallTracker {
    window: Duration,
    threshold_pct: u32,
    last: HashMap<u32, u64>,
    total_ms: u64,
    samples: VecDeque<(Duration, u64)>,
    started: Option<Duration>,
}

impl StallTracker {
    /// Stalled when the tree averaged under `threshold_pct` of ONE core for a
    /// whole `window`.
    pub fn new(window: Duration, threshold_pct: u32) -> Self {
        Self {
            window,
            threshold_pct,
            last: HashMap::new(),
            total_ms: 0,
            samples: VecDeque::new(),
            started: None,
        }
    }

    /// Feed the job's tree as of `at` (time since the job started).
    pub fn observe(&mut self, at: Duration, tree: &[&Proc]) -> Verdict {
        let mut next = HashMap::with_capacity(tree.len());
        for p in tree {
            let gained = p.cpu_ms.saturating_sub(self.last.get(&p.pid).copied().unwrap_or(0));
            self.total_ms += gained;
            next.insert(p.pid, p.cpu_ms);
        }
        self.last = next;
        let first = *self.started.get_or_insert(at);
        self.samples.push_back((at, self.total_ms));
        // Keep one sample at or before the window's start to measure against.
        while self.samples.len() > 2 && self.samples.get(1).is_some_and(|(t, _)| at.saturating_sub(*t) >= self.window) {
            self.samples.pop_front();
        }
        if at.saturating_sub(first) < self.window {
            return self.idle_for(at, first);
        }
        let Some(&(t0, c0)) = self.samples.front() else {
            return Verdict::Busy;
        };
        if self.pct(at, t0, c0) < f64::from(self.threshold_pct) {
            Verdict::Stalled
        } else {
            Verdict::Busy
        }
    }

    /// `Idle(d)` once the tree has averaged under the threshold for at least a
    /// quarter of the window (so `status` and the gate can see a suspect early).
    fn idle_for(&self, at: Duration, first: Duration) -> Verdict {
        let span = at.saturating_sub(first);
        if span < self.window / 4 {
            return Verdict::Busy;
        }
        let Some(&(t0, c0)) = self.samples.front() else {
            return Verdict::Busy;
        };
        if self.pct(at, t0, c0) < f64::from(self.threshold_pct) {
            Verdict::Idle(span)
        } else {
            Verdict::Busy
        }
    }

    #[allow(clippy::cast_precision_loss, reason = "a percentage")]
    fn pct(&self, at: Duration, t0: Duration, c0: u64) -> f64 {
        let wall = at.saturating_sub(t0).as_millis().max(1) as f64;
        (self.total_ms.saturating_sub(c0)) as f64 * 100.0 / wall
    }
}

/// A process burning CPU, and the queue job it belongs to, if any.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hog {
    pub pid: u32,
    pub pcpu: f64,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
}

/// The queue job a process descends from (`Tree`) or was tagged by (`Env`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub ticket: u64,
    pub label: String,
    /// The job is currently judged idle by its watchdog: this CPU is a stall's.
    pub stalled: bool,
    pub via: Via,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// A descendant (or group member) of the job's process.
    Tree,
    /// Not a descendant any more (re-parented, or a new session) but it carries
    /// the job's [`TICKET_ENV`].
    Env,
    /// Neither, but its working directory is inside the job's (where `ps -E`
    /// cannot show an environment, e.g. a sandboxed or hardened macOS).
    Cwd,
}

/// The facts about a running job that attribution needs.
#[derive(Clone, Debug)]
pub struct JobRef {
    pub ticket: u64,
    pub label: String,
    pub pid: u32,
    pub stalled: bool,
    /// Where it runs: a process whose working directory is inside it, and that
    /// is in no job's tree, is attributed to it as a last resort.
    pub cwd: PathBuf,
}

/// `TICKET_ENV=<n>` out of a `ps -E` command line.
pub fn ticket_in_env(line: &str) -> Option<u64> {
    let key = format!("{TICKET_ENV}=");
    line.split_whitespace().find_map(|w| w.strip_prefix(key.as_str())?.parse().ok())
}

/// Owner of `p` by tree membership alone.
fn tree_owner(procs: &[Proc], jobs: &[JobRef], p: &Proc, me: u32) -> Option<Owner> {
    jobs.iter().find_map(|j| {
        tree(procs, j.pid, me).iter().any(|t| t.pid == p.pid).then(|| Owner {
            ticket: j.ticket,
            label: j.label.clone(),
            stalled: j.stalled,
            via: Via::Tree,
        })
    })
}

/// The `n` hungriest processes at or above `min_pcpu`, each attributed to a
/// queue job: by tree first, then (for the few not in any tree) by the ticket
/// in its environment, read through `env_of`. `cwd_of` fills the working
/// directory. Both are injected so tests do not need real processes.
pub fn top_hogs(
    procs: &[Proc],
    jobs: &[JobRef],
    n: usize,
    min_pcpu: f64,
    me: u32,
    env_of: &dyn Fn(&[u32]) -> HashMap<u32, String>,
    cwd_of: &dyn Fn(u32) -> Option<PathBuf>,
) -> Vec<Hog> {
    let mut hungry: Vec<&Proc> = procs.iter().filter(|p| p.pcpu >= min_pcpu && p.pid != me).collect();
    hungry.sort_by(|a, b| b.pcpu.total_cmp(&a.pcpu));
    hungry.truncate(n);
    let mut owners: Vec<Option<Owner>> = hungry.iter().map(|p| tree_owner(procs, jobs, p, me)).collect();
    let unowned: Vec<u32> = hungry.iter().zip(&owners).filter(|(_, o)| o.is_none()).map(|(p, _)| p.pid).collect();
    if !jobs.is_empty() && !unowned.is_empty() {
        let envs = env_of(&unowned);
        for (p, o) in hungry.iter().zip(owners.iter_mut()) {
            if o.is_some() {
                continue;
            }
            let ticket = envs.get(&p.pid).and_then(|l| ticket_in_env(l));
            *o = ticket.and_then(|t| jobs.iter().find(|j| j.ticket == t)).map(|j| Owner {
                ticket: j.ticket,
                label: j.label.clone(),
                stalled: j.stalled,
                via: Via::Env,
            });
        }
    }
    hungry
        .into_iter()
        .zip(owners)
        .map(|(p, owner)| {
            let cwd = cwd_of(p.pid);
            let owner = owner.or_else(|| {
                let dir = cwd.as_ref()?;
                let j = jobs
                    .iter()
                    .filter(|j| !j.cwd.as_os_str().is_empty() && dir.starts_with(&j.cwd))
                    .max_by_key(|j| j.cwd.as_os_str().len())?;
                Some(Owner {
                    ticket: j.ticket,
                    label: j.label.clone(),
                    stalled: j.stalled,
                    via: Via::Cwd,
                })
            });
            Hog {
                pid: p.pid,
                pcpu: p.pcpu,
                command: short_command(&p.command),
                cwd,
                owner,
            }
        })
        .collect()
}

/// First word's file name plus up to 60 chars of the command.
fn short_command(c: &str) -> String {
    let s: String = c.chars().take(60).collect();
    s
}

/// Percent of the whole machine (all cores) that processes owned by a stalled
/// job are using: what the CPU gate and the AIMD scale should ignore.
pub fn stalled_machine_pct(hogs: &[Hog], cores: usize) -> f64 {
    let sum: f64 = hogs.iter().filter(|h| h.owner.as_ref().is_some_and(|o| o.stalled)).map(|h| h.pcpu).sum();
    #[allow(clippy::cast_precision_loss, reason = "core counts are tiny")]
    let cores = cores.max(1) as f64;
    sum / cores
}

/// Live: `pid → "<command> <ENV=...>"` lines from `ps -Eww` for `pids`.
pub fn env_lines(pids: &[u32]) -> HashMap<u32, String> {
    let list = pids.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
    let Ok(out) = Command::new("ps")
        .args(["-Eww", "-o", "pid=", "-o", "command=", "-p", &list])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (pid, rest) = l.split_once(char::is_whitespace)?;
            Some((pid.parse().ok()?, rest.to_string()))
        })
        .collect()
}

/// Live: the working directory of `pid`.
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        return std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
    }
    let out = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-Fn", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n'))
        .map(PathBuf::from)
}

/// Live attribution of the machine's top CPU hogs to `jobs`.
pub fn live_hogs(jobs: &[JobRef], n: usize, min_pcpu: f64) -> Vec<Hog> {
    let Some(procs) = snapshot() else {
        return Vec::new();
    };
    top_hogs(&procs, jobs, n, min_pcpu, std::process::id(), &env_lines, &cwd_of)
}

/// Pids of processes carrying `ticket` in their environment that are using
/// real CPU right now and are not part of the job's own tree: what a stalled
/// job left spinning.
pub fn tagged_hogs(procs: &[Proc], ticket: u64, min_pcpu: f64, me: u32, env_of: &dyn Fn(&[u32]) -> HashMap<u32, String>) -> Vec<u32> {
    let hungry: Vec<u32> = procs.iter().filter(|p| p.pcpu >= min_pcpu && p.pid != me).map(|p| p.pid).collect();
    if hungry.is_empty() {
        return Vec::new();
    }
    env_of(&hungry)
        .into_iter()
        .filter(|(_, l)| ticket_in_env(l) == Some(ticket))
        .map(|(pid, _)| pid)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, pgid: u32, pcpu: f64, cpu_ms: u64, cmd: &str) -> Proc {
        Proc {
            pid,
            ppid,
            pgid,
            pcpu,
            cpu_ms,
            rss_kb: 1000,
            command: cmd.into(),
        }
    }

    #[test]
    fn cpu_time_formats_parse() {
        assert_eq!(parse_cpu_time("0:12.34"), Some(12_340));
        assert_eq!(parse_cpu_time("1:02:03.50"), Some(3_723_500));
        assert_eq!(parse_cpu_time("2-01:00:00"), Some(2 * 86_400_000 + 3_600_000));
        assert_eq!(parse_cpu_time("34.5"), Some(34_500));
        assert_eq!(parse_cpu_time("x"), None);
    }

    #[test]
    fn ps_rows_parse_with_spaces_in_the_command() {
        let out = "  10     1    10   0.0   0:01.00   500 /bin/sh -c echo hi\n 11 10 10 95.5 1:00.00 9000 esbuild --service\ngarbage\n";
        let v = parse_ps(out);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].pid, v[0].command.as_str()), (10, "/bin/sh -c echo hi"));
        assert_eq!((v[1].ppid, v[1].pcpu, v[1].cpu_ms, v[1].rss_kb), (10, 95.5, 60_000, 9000));
    }

    #[test]
    fn the_tree_follows_ppid_across_groups_and_includes_the_group() {
        let procs = [
            p(1, 0, 1, 0.0, 0, "launchd"),
            p(100, 50, 100, 0.0, 0, "job leader"),
            p(101, 100, 100, 0.0, 0, "same group child"),
            p(102, 100, 102, 50.0, 0, "own group (setpgid) child"),
            p(103, 102, 103, 80.0, 0, "grandchild in a new session"),
            p(104, 1, 100, 0.0, 0, "orphan still in the group"),
            p(200, 1, 200, 99.0, 0, "orphaned daemon: not ours"),
            p(300, 100, 300, 0.0, 0, "th itself"),
        ];
        let mut pids: Vec<u32> = tree(&procs, 100, 300).iter().map(|x| x.pid).collect();
        pids.sort_unstable();
        assert_eq!(pids, vec![100, 101, 102, 103, 104]);
    }

    fn tracker() -> StallTracker {
        StallTracker::new(Duration::from_secs(1200), 5)
    }

    fn mins(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    #[test]
    fn a_tree_that_never_burns_cpu_is_stalled_after_the_window() {
        let mut t = tracker();
        let idle = [p(100, 1, 100, 0.0, 500, "job")];
        let tree: Vec<&Proc> = idle.iter().collect();
        assert_eq!(t.observe(mins(0), &tree), Verdict::Busy);
        assert_eq!(t.observe(mins(4), &tree), Verdict::Busy, "not suspect before a quarter window");
        assert!(matches!(t.observe(mins(6), &tree), Verdict::Idle(_)), "suspect after a quarter window");
        assert!(matches!(t.observe(mins(19), &tree), Verdict::Idle(_)));
        assert_eq!(t.observe(mins(20), &tree), Verdict::Stalled);
    }

    #[test]
    fn a_detached_child_doing_the_real_work_keeps_the_job_alive() {
        let mut t = tracker();
        let mut cpu = 0;
        for m in 0..=40 {
            // The leader is idle; a grandchild in another session burns a core.
            cpu += 60_000;
            let procs = [p(100, 1, 100, 0.0, 10, "leader"), p(103, 102, 103, 100.0, cpu, "rustc in a new session")];
            let tree: Vec<&Proc> = procs.iter().collect();
            assert_ne!(t.observe(mins(m), &tree), Verdict::Stalled, "minute {m}");
        }
    }

    #[test]
    fn short_lived_children_count_and_exited_ones_do_not_inflate() {
        let mut t = tracker();
        // Every minute a fresh child appears having used 30 CPU-s, then exits.
        for m in 0..=30 {
            let procs = [
                p(100, 1, 100, 0.0, 10, "leader"),
                p(1000 + u32::try_from(m).unwrap(), 100, 100, 0.0, 30_000, "cc"),
            ];
            let tree: Vec<&Proc> = procs.iter().collect();
            assert_ne!(t.observe(mins(m), &tree), Verdict::Stalled, "minute {m}");
        }
        // Then nothing but the leader: idle again, and stalls a window later.
        let idle = [p(100, 1, 100, 0.0, 10, "leader")];
        let tree: Vec<&Proc> = idle.iter().collect();
        let mut last = Verdict::Busy;
        for m in 31..=55 {
            last = t.observe(mins(m), &tree);
        }
        assert_eq!(last, Verdict::Stalled);
    }

    #[test]
    fn work_resuming_clears_the_suspicion() {
        let mut t = tracker();
        let idle = [p(100, 1, 100, 0.0, 500, "job")];
        let tree: Vec<&Proc> = idle.iter().collect();
        t.observe(mins(0), &tree);
        assert!(matches!(t.observe(mins(8), &tree), Verdict::Idle(_)));
        let busy = [p(100, 1, 100, 100.0, 500 + 8 * 60_000, "job")];
        let tree: Vec<&Proc> = busy.iter().collect();
        assert_eq!(t.observe(mins(9), &tree), Verdict::Busy);
    }

    fn jobs() -> Vec<JobRef> {
        vec![
            JobRef {
                ticket: 7,
                label: "attest test".into(),
                pid: 100,
                stalled: true,
                cwd: PathBuf::from("/work/a"),
            },
            JobRef {
                ticket: 8,
                label: "attest rust".into(),
                pid: 400,
                stalled: false,
                cwd: PathBuf::from("/work/b"),
            },
        ]
    }

    #[test]
    fn hogs_are_attributed_by_tree_then_by_env_tag() {
        let procs = [
            p(100, 1, 100, 0.0, 0, "job"),
            p(103, 100, 103, 100.0, 0, "rustc"),
            p(200, 1, 200, 270.0, 0, "esbuild --service"),
            p(201, 1, 201, 90.0, 0, "turbo daemon"),
            p(202, 1, 202, 95.0, 0, "someone's browser"),
            p(400, 1, 400, 0.0, 0, "other job"),
            p(401, 400, 400, 60.0, 0, "cargo"),
            p(500, 1, 500, 1.0, 0, "idle thing"),
        ];
        let env = |pids: &[u32]| -> HashMap<u32, String> {
            pids.iter()
                .map(|p| {
                    let line = match p {
                        200 | 201 => format!("x {TICKET_ENV}=7 HOME=/h"),
                        _ => "x HOME=/h".to_string(),
                    };
                    (*p, line)
                })
                .collect()
        };
        let hogs = top_hogs(&procs, &jobs(), 10, 50.0, 9999, &env, &|pid| match pid {
            202 => Some(PathBuf::from("/work/b/apps/web")), // matches no tree and no tag: by cwd
            _ => Some(PathBuf::from(format!("/cwd/{pid}"))),
        });
        let by = |pid: u32| hogs.iter().find(|h| h.pid == pid).unwrap();
        assert_eq!(hogs[0].pid, 200, "sorted by CPU");
        assert_eq!(hogs.len(), 5, "the 1% process is below the floor");
        assert_eq!((by(200).owner.as_ref().unwrap().ticket, by(200).owner.as_ref().unwrap().via), (7, Via::Env));
        assert!(by(200).owner.as_ref().unwrap().stalled);
        assert_eq!(by(103).owner.as_ref().unwrap().via, Via::Tree);
        assert_eq!(by(401).owner.as_ref().unwrap().ticket, 8);
        assert!(!by(401).owner.as_ref().unwrap().stalled);
        assert_eq!(by(202).owner.as_ref().unwrap().ticket, 8, "last resort: its cwd is inside job 8's");
        assert_eq!(by(202).owner.as_ref().unwrap().via, Via::Cwd);
        assert!(by(201).owner.is_some());
        assert_eq!(by(200).cwd, Some(PathBuf::from("/cwd/200")));
        // 270 + 90 (env-tagged to the stalled job) + 100 (its tree) on 10 cores.
        assert!((stalled_machine_pct(&hogs, 10) - 46.0).abs() < 1e-9);
    }

    #[test]
    fn env_tag_lookup_needs_the_exact_key_and_a_number() {
        assert_eq!(ticket_in_env(&format!("cmd A=1 {TICKET_ENV}=42 B=2")), Some(42));
        assert_eq!(ticket_in_env(&format!("cmd X{TICKET_ENV}=42")), None);
        assert_eq!(ticket_in_env(&format!("cmd {TICKET_ENV}=abc")), None);
        assert_eq!(ticket_in_env("cmd"), None);
    }

    #[test]
    fn tagged_hogs_are_only_this_tickets_busy_processes() {
        let procs = [
            p(200, 1, 200, 200.0, 0, "esbuild"),
            p(201, 1, 201, 1.0, 0, "idle daemon"),
            p(202, 1, 202, 99.0, 0, "other job's"),
        ];
        let env = |pids: &[u32]| -> HashMap<u32, String> { pids.iter().map(|p| (*p, format!("c {TICKET_ENV}={}", if *p == 202 { 8 } else { 7 }))).collect() };
        assert_eq!(tagged_hogs(&procs, 7, 50.0, 9999, &env), vec![200]);
    }

    /// Real processes: the live `ps` parse, the live ppid walk across a
    /// detached (new session) child, and the live `ps -E` tag lookup.
    #[test]
    #[cfg(unix)]
    fn live_table_tree_and_env_tag() {
        use std::os::unix::process::CommandExt as _;
        let mut leader = Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .env(TICKET_ENV, "424242")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // A child that moved to its own group, like esbuild's service does.
        let mut detached = Command::new("sleep")
            .arg("30")
            .env(TICKET_ENV, "424242")
            .process_group(0)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let procs = snapshot().expect("ps runs");
        let me = std::process::id();
        let lid = leader.id();
        let in_tree: Vec<u32> = tree(&procs, lid, me).iter().map(|x| x.pid).collect();
        assert!(in_tree.contains(&lid), "{in_tree:?}");
        assert!(in_tree.len() >= 2, "the sleep under the shell is a descendant: {in_tree:?}");
        assert!(!in_tree.contains(&detached.id()));
        // `ps -E` shows an environment only where the OS allows it (not from
        // inside some sandboxes): when it does, the tag must round-trip.
        let env = env_lines(&[detached.id()]);
        if env.get(&detached.id()).is_some_and(|l| l.contains("PATH=")) {
            assert_eq!(env.get(&detached.id()).and_then(|l| ticket_in_env(l)), Some(424_242), "{env:?}");
        } else {
            eprintln!("ps -E shows no environment here; env-tag round trip not exercised");
        }
        let _ = leader.kill();
        let _ = detached.kill();
        let _ = leader.wait();
        let _ = detached.wait();
    }
}
