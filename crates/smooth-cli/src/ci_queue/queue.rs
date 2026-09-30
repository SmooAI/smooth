//! Admission: machine-wide slots, FIFO tickets, and the pressure gate — all of
//! it kernel `flock`s on files under the queue dir. There is no daemon.
//!
//! ```text
//! <dir>/mutex.lock          held briefly around every read-modify-write below
//! <dir>/next-ticket         the ticket counter
//! <dir>/tickets/<n>.json    one per waiting job; the waiter holds its flock
//! <dir>/slots/<class>-<i>.slot   one per slot; the running job holds its flock
//! <dir>/locks/<hash>.lock   one per named shared resource (`--lock`)
//! <dir>/tickets/<n>.passes  how many times smaller jobs have passed waiter n
//! <dir>/budget.json         the AIMD scale on the capacity budget
//! <dir>/history.db          finished jobs (SQLite, rolling; see `history`)
//! ```
//!
//! **Liveness is the lock, never the file.** A job is waiting iff someone holds
//! its ticket's flock, and running iff someone holds its slot's flock. The
//! kernel drops both when the holder dies — `kill -9` included — so there is no
//! pid to check, nothing stale to reap on a timer, and no reaping race. (A
//! process-list or pid-file check is not a lock: the old disk-cleanup timer
//! corrupted live cargo builds on exactly that race.) The JSON inside a file is
//! a description for `status`, and is ignored whenever the lock says otherwise.
//!
//! Every fd here is opened by `std`, which always sets `O_CLOEXEC`, and the
//! slot fd lives in the `th` process that forks the job. So the job — and any
//! daemon it leaves behind (turbo, sccache) — can never inherit a slot.
//!
//! **FIFO.** `flock` alone is not fair: whichever waiter polls first after a
//! release wins. So each waiter takes a ticket under the mutex, and only the
//! lowest live ticket in its class may take a slot.
//!
//! **Named locks.** A job may also name machine-shared resources it will hold
//! (`--lock cargo` → the cargo target dir). Two jobs naming the same resource
//! are never admitted together — the second would only sit behind the first's
//! lock holding a slot. For cargo, cargo's OWN `.cargo-lock` in the target dir
//! is probed too, so a build started outside the queue also holds the job. A
//! waiter blocked only on a named lock does not hold up the waiters behind it
//! that don't need that lock.
//!
//! **Capacity.** Within the slot ceiling a job is admitted only while its
//! estimate fits the machine's budget (see `budget`). An earlier waiter that
//! does not fit may be passed by smaller jobs — at most `budget.max_passes`
//! times. Then it holds a reservation: nobody passes it, and it is admitted as
//! running jobs drain (with nothing running, anything is admitted).

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::budget::{self, Aimd, Estimate, Running};
use super::config::{self, Config};
use super::exec::{Ended, Usage};
use super::history::History;
pub use super::history::HistoryEntry;
use super::pressure::{self, CpuState, Probe, Readings, SystemProbe};

/// Set in every admitted job's environment. A nested `th ci-queue run` (or
/// `th attest`) inside a queued job runs directly instead of queueing again:
/// with 2 heavy slots, two outer jobs each waiting on an inner slot would
/// deadlock the class.
pub const NESTED_ENV: &str = "SMOOTH_CI_QUEUE_SLOT";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// Typecheck, clippy, test suites — capped hard and held under pressure.
    #[default]
    Heavy,
    /// Formatters, linters, guards — a looser cap, never held by the gate.
    Light,
}

impl Class {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Heavy => "heavy",
            Self::Light => "light",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "heavy" => Some(Self::Heavy),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub const fn slots(self, c: &Config) -> usize {
        match self {
            Self::Heavy => c.slots.heavy,
            Self::Light => c.slots.light,
        }
    }
}

/// What a job says about itself, in its ticket and then in its slot.
/// `Default` is for literals in tests and demos (`..Default::default()`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobInfo {
    pub class: Class,
    pub ticket: u64,
    pub label: String,
    /// The `th` process holding the lock.
    pub pid: u32,
    /// The job itself, once spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_pid: Option<u32>,
    pub cwd: PathBuf,
    pub queued_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<usize>,
    /// Resolved named locks (`cargo:/path/to/target`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locks: Vec<String>,
    /// While waiting: exactly what holds it (the text of its `waiting (…)`
    /// line), rewritten into its ticket whenever it changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<String>,
    /// When `waiting_on` last changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on_since_ms: Option<u64>,
    /// When a named lock holds it: the ticket of the job holding that lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by_ticket: Option<u64>,
    /// What admission expects it to need (from its label's history, or the
    /// class default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est: Option<Estimate>,
    /// While running: its process group's memory at the last sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_now_kb: Option<u64>,
}

pub struct Request {
    pub class: Class,
    pub label: String,
    pub cwd: PathBuf,
    pub max_wait: Duration,
    /// Already resolved with [`Queue::resolve_lock`].
    pub locks: Vec<String>,
    /// sha256 (first 16 hex) of the argv, for history.
    pub cmd_hash: Option<String>,
}

/// sha256 of an argv, first 16 hex chars.
pub fn cmd_hash(argv: &[std::ffi::OsString]) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    for a in argv {
        h.update(a.as_encoded_bytes());
        h.update([0]);
    }
    h.finalize().iter().take(8).fold(String::new(), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[derive(Debug)]
pub enum AdmitError {
    /// No slot within `max_wait`. `reason` is the last thing that held it.
    WaitTimeout { waited: Duration, reason: String },
    /// The queue itself is unusable (unwritable dir, disk full, …).
    Queue(anyhow::Error),
}

impl From<anyhow::Error> for AdmitError {
    fn from(e: anyhow::Error) -> Self {
        Self::Queue(e)
    }
}

impl From<io::Error> for AdmitError {
    fn from(e: io::Error) -> Self {
        Self::Queue(e.into())
    }
}

#[derive(Clone)]
pub struct Queue {
    dir: PathBuf,
    pub config: Config,
    probe: Arc<dyn Probe>,
    /// Whether `NESTED_ENV` in our own environment means "already admitted".
    /// Off for queues built by tests, which may themselves be running under a
    /// queued `cargo test`.
    honor_nesting: bool,
    /// Where cargo builds go. `None` = resolve it (`CARGO_TARGET_DIR`, then
    /// `~/.cargo/config.toml`, then `<cwd>/target`); tests pin it.
    cargo_target: Option<PathBuf>,
}

impl std::fmt::Debug for Queue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Queue")
            .field("dir", &self.dir)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Default for Queue {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Queue {
    /// The machine's queue: `SMOOTH_CI_QUEUE_DIR` or `~/.smooth/ci-queue`,
    /// configured by `SMOOTH_CI_QUEUE_CONFIG` or `~/.smooth/ci-queue.toml`.
    pub fn from_env() -> Self {
        let dir = std::env::var_os("SMOOTH_CI_QUEUE_DIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs_next::home_dir().map(|h| h.join(".smooth").join("ci-queue")))
            .unwrap_or_else(|| std::env::temp_dir().join("smooth-ci-queue"));
        Self {
            dir,
            config: Config::load(config::default_path().as_deref()),
            probe: Arc::new(SystemProbe::new()),
            honor_nesting: true,
            cargo_target: None,
        }
    }

    /// A queue at `dir` with explicit config and probe, ignoring `NESTED_ENV`.
    #[cfg(all(test, unix))]
    pub fn at(dir: PathBuf, config: Config, probe: Arc<dyn Probe>) -> Self {
        Self {
            dir,
            config,
            probe,
            honor_nesting: false,
            cargo_target: None,
        }
    }

    /// Pin the cargo target dir instead of resolving the machine's.
    #[cfg(all(test, unix))]
    #[must_use]
    pub fn with_cargo_target(mut self, dir: PathBuf) -> Self {
        self.cargo_target = Some(dir);
        self
    }

    fn cargo_target_for(&self, cwd: &Path) -> PathBuf {
        let t = self.cargo_target.clone().or_else(cargo_target_dir).unwrap_or_else(|| cwd.join("target"));
        if t.is_absolute() {
            t
        } else {
            cwd.join(t)
        }
    }

    /// `cargo` → `cargo:<target dir>` (the dir cargo will lock, so two jobs on
    /// different target dirs do not exclude each other). Any other name is
    /// used as-is.
    pub fn resolve_lock(&self, name: &str, cwd: &Path) -> String {
        if name == "cargo" {
            return format!("cargo:{}", self.cargo_target_for(cwd).display());
        }
        name.to_string()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn tickets_dir(&self) -> PathBuf {
        self.dir.join("tickets")
    }

    fn slots_dir(&self) -> PathBuf {
        self.dir.join("slots")
    }

    fn locks_dir(&self) -> PathBuf {
        self.dir.join("locks")
    }

    fn store(&self) -> History {
        History::new(&self.dir, self.config.run.history_keep)
    }

    fn ensure_dirs(&self) -> Result<()> {
        for d in [self.tickets_dir(), self.slots_dir(), self.locks_dir()] {
            fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
        }
        // One-time move of the pre-SQLite history into history.db.
        let old = self.dir.join("history.jsonl");
        if old.exists() {
            if let Err(e) = self.store().import_jsonl(&old) {
                eprintln!("th ci-queue: could not import {} ({e:#})", old.display());
            }
        }
        Ok(())
    }

    /// What a job with this label and class is expected to need. History
    /// problems degrade to the class default; they never fail admission.
    fn estimate_for(&self, label: &str, class: Class) -> Estimate {
        let samples = self.store().samples(label, self.config.budget.window).unwrap_or_else(|e| {
            eprintln!("th ci-queue: history unreadable, using class defaults ({e:#})");
            Vec::new()
        });
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        budget::estimate(&samples, class, &self.config.budget, cores)
    }

    fn aimd_path(&self) -> PathBuf {
        self.dir.join("budget.json")
    }

    /// Under the mutex.
    fn load_aimd(&self) -> Aimd {
        fs::read_to_string(self.aimd_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Under the mutex.
    fn save_aimd(&self, a: &Aimd) {
        if let Ok(bytes) = serde_json::to_vec(a) {
            let _ = write_atomic(&self.aimd_path(), &bytes);
        }
    }

    fn cpu_path(&self) -> PathBuf {
        self.dir.join("cpu.json")
    }

    /// One pressure sample, with CPU busy smoothed across every `th` through
    /// `cpu.json` (see [`CpuState`]). A process too new to have measured an
    /// interval of its own uses the kept reading while it is fresh. Under the
    /// mutex.
    fn read_pressure(&self, cwd: &Path) -> Readings {
        let mut r = self.probe.read(&self.disk_paths(cwd));
        let now = now_ms();
        let kept: Option<CpuState> = fs::read_to_string(self.cpu_path()).ok().and_then(|t| serde_json::from_str(&t).ok());
        r.cpu_busy_pct = match r.cpu_busy_pct {
            Some(raw) => {
                let next = CpuState::update(kept, raw, now);
                if let Ok(bytes) = serde_json::to_vec(&next) {
                    let _ = write_atomic(&self.cpu_path(), &bytes);
                }
                Some(next.busy_pct)
            }
            None => kept.and_then(|k| k.fresh(now)),
        };
        r
    }

    fn passes_path(&self, ticket: u64) -> PathBuf {
        self.tickets_dir().join(format!("{ticket:020}.passes"))
    }

    fn passes(&self, ticket: u64) -> u32 {
        fs::read_to_string(self.passes_path(ticket))
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0)
    }

    /// The queue-wide mutex. Held for milliseconds; released on drop, or by the
    /// kernel if we die holding it.
    fn mutex(&self) -> Result<File> {
        let p = self.dir.join("mutex.lock");
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&p)
            .with_context(|| format!("opening {}", p.display()))?;
        f.lock().with_context(|| format!("locking {}", p.display()))?;
        Ok(f)
    }

    /// Wait for a slot, then hand back the [`Admission`] that holds it.
    pub fn admit(&self, req: &Request) -> Result<Admission, AdmitError> {
        if self.honor_nesting {
            if let Some(outer) = std::env::var(NESTED_ENV).ok().filter(|v| !v.is_empty()) {
                return Ok(Admission::nested(req, outer));
            }
        }
        // Unix only: the queue leans on flock(2) semantics (advisory, readable
        // while held) and process groups. Windows' LockFileEx is mandatory — a
        // held ticket cannot even be read — so there the job runs unqueued.
        if !cfg!(unix) {
            return Ok(Admission::nested(req, String::new()));
        }
        self.ensure_dirs()?;
        let started = Instant::now();
        let queued_at_ms = now_ms();
        let est = self.estimate_for(&req.label, req.class);
        let mut me = JobInfo {
            class: req.class,
            ticket: 0,
            label: req.label.clone(),
            pid: std::process::id(),
            child_pid: None,
            cwd: req.cwd.clone(),
            queued_at_ms,
            admitted_at_ms: None,
            slot: None,
            locks: req.locks.clone(),
            waiting_on: None,
            waiting_on_since_ms: None,
            blocked_by_ticket: None,
            est: Some(est),
            rss_now_kb: None,
        };
        let (ticket_path, mut ticket_file) = self.take_ticket(&mut me)?;
        let poll = Duration::from_millis(self.config.run.poll_ms);
        let note_every = Duration::from_secs(self.config.run.note_every_secs.max(1));
        let mut next_note = Duration::ZERO;
        let mut held = false;

        loop {
            let reason = match self.try_admit(&me, &ticket_path)? {
                Attempt::Admitted { slot, file, locks, readings } => {
                    drop(ticket_file);
                    let waited = started.elapsed();
                    if held {
                        eprintln!("th ci-queue: {} admitted after {}s", req.label, waited.as_secs());
                    }
                    me.slot = Some(slot);
                    me.admitted_at_ms = Some(now_ms());
                    me.waiting_on = None;
                    me.waiting_on_since_ms = None;
                    me.blocked_by_ticket = None;
                    return Ok(Admission {
                        lock: Some(file),
                        held_locks: locks,
                        info: me,
                        waited,
                        queue: Some(self.clone()),
                        cmd_hash: req.cmd_hash.clone(),
                        pressure: readings.map(|r| *r),
                    });
                }
                Attempt::Wait { reason, by_ticket } => {
                    // Publish why, for `status` and `top`. We hold the ticket's
                    // flock, so only we write it; the mutex keeps readers from
                    // seeing a half-written file.
                    if me.waiting_on.as_deref() != Some(reason.as_str()) || me.blocked_by_ticket != by_ticket {
                        me.waiting_on = Some(reason.clone());
                        me.waiting_on_since_ms = Some(now_ms());
                        me.blocked_by_ticket = by_ticket;
                        let _m = self.mutex()?;
                        rewrite(&mut ticket_file, &me)?;
                    }
                    reason
                }
            };
            held = true;
            let waited = started.elapsed();
            if waited >= req.max_wait {
                self.remove_ticket(&ticket_path)?;
                drop(ticket_file);
                self.record(&HistoryEntry {
                    label: req.label.clone(),
                    class: Some(req.class),
                    cwd: req.cwd.clone(),
                    repo: repo_root(&req.cwd),
                    cmd_hash: req.cmd_hash.clone(),
                    ticket: me.ticket,
                    queued_at_ms,
                    finished_at_ms: now_ms(),
                    wait_ms: millis(waited),
                    outcome: "wait-timeout".into(),
                    exit: super::EXIT_WAIT_TIMEOUT,
                    est_rss_kb: Some(est.rss_kb),
                    est_millicores: Some(est.millicores),
                    ..HistoryEntry::default()
                });
                return Err(AdmitError::WaitTimeout { waited, reason });
            }
            if waited >= next_note {
                eprintln!("th ci-queue: {} waiting ({}s, reason: {reason})", req.label, waited.as_secs());
                next_note += note_every;
            }
            std::thread::sleep(poll.min(req.max_wait.saturating_sub(waited)).max(Duration::from_millis(1)));
        }
    }

    /// Draw the next ticket and publish it as a locked file, all under the
    /// mutex, so no prober can see the file before its lock is held.
    fn take_ticket(&self, me: &mut JobInfo) -> Result<(PathBuf, File)> {
        let _m = self.mutex()?;
        let counter = self.dir.join("next-ticket");
        let stored: u64 = fs::read_to_string(&counter).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        // A lost or corrupt counter must not hand out a number a live waiter
        // already holds.
        let highest = self.ticket_files()?.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let n = stored.max(highest + 1).max(1);
        write_atomic(&counter, format!("{}\n", n + 1).as_bytes())?;

        me.ticket = n;
        let path = self.tickets_dir().join(format!("{n:020}.json"));
        let mut f = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("creating {}", path.display()))?;
        f.try_lock().map_err(|e| anyhow::anyhow!("locking fresh ticket {}: {e}", path.display()))?;
        f.write_all(&serde_json::to_vec(me)?)?;
        f.flush()?;
        Ok((path, f))
    }

    fn remove_ticket(&self, path: &Path) -> Result<()> {
        let _m = self.mutex()?;
        let _ = fs::remove_file(path.with_extension("passes"));
        match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e).with_context(|| format!("removing {}", path.display())),
            _ => Ok(()),
        }
    }

    fn ticket_files(&self) -> Result<Vec<(u64, PathBuf)>> {
        let dir = self.tickets_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
        };
        let mut out: Vec<(u64, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let p = e.path();
                let n = p.file_stem()?.to_str()?.parse().ok()?;
                (p.extension()? == "json").then_some((n, p))
            })
            .collect();
        out.sort_unstable_by_key(|(n, _)| *n);
        Ok(out)
    }

    /// Every waiting job, lowest ticket first. A ticket nobody holds belongs to
    /// a waiter that died; it is deleted here. Call under the mutex.
    fn live_tickets(&self) -> Result<Vec<JobInfo>> {
        let mut live = Vec::new();
        for (n, path) in self.ticket_files()? {
            let Ok(mut f) = OpenOptions::new().read(true).write(true).open(&path) else {
                continue;
            };
            match f.try_lock() {
                Ok(()) => {
                    let _ = fs::remove_file(path.with_extension("passes"));
                    let _ = fs::remove_file(&path);
                }
                Err(TryLockError::WouldBlock) => {
                    let mut s = String::new();
                    let _ = f.read_to_string(&mut s);
                    live.push(serde_json::from_str(&s).unwrap_or_else(|_| unknown_job(n)));
                }
                Err(TryLockError::Error(e)) => return Err(e).with_context(|| format!("probing {}", path.display())),
            }
        }
        Ok(live)
    }

    fn slot_path(&self, class: Class, i: usize) -> PathBuf {
        self.slots_dir().join(format!("{}-{i}.slot", class.name()))
    }

    /// Probe slots 1..=max(configured, highest file on disk) of `class`. A slot
    /// beyond the configured count (the config shrank while it ran) still
    /// counts as busy but is never handed out. Call under the mutex.
    fn scan_slots(&self, class: Class) -> Result<Vec<SlotState>> {
        let prefix = format!("{}-", class.name());
        let on_disk = fs::read_dir(self.slots_dir()).map_or(0, |rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| e.file_name().to_str()?.strip_prefix(&prefix)?.strip_suffix(".slot")?.parse::<usize>().ok())
                .max()
                .unwrap_or(0)
        });
        let configured = class.slots(&self.config);
        let mut out = Vec::new();
        for i in 1..=configured.max(on_disk) {
            let path = self.slot_path(class, i);
            let mut f = if i <= configured {
                OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(&path)
                    .with_context(|| format!("opening {}", path.display()))?
            } else {
                match OpenOptions::new().read(true).write(true).open(&path) {
                    Ok(f) => f,
                    Err(_) => continue,
                }
            };
            match f.try_lock() {
                Ok(()) if i <= configured => out.push(SlotState::Free { index: i, file: f }),
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => {
                    let mut s = String::new();
                    let _ = f.read_to_string(&mut s);
                    let mut info: JobInfo = serde_json::from_str(&s).unwrap_or_else(|_| unknown_job(0));
                    info.slot = Some(i);
                    out.push(SlotState::Busy(Box::new(info)));
                }
                Err(TryLockError::Error(e)) => return Err(e).with_context(|| format!("probing {}", path.display())),
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_lines, reason = "one admission decision, in the order it is made")]
    fn try_admit(&self, me: &JobInfo, ticket_path: &Path) -> Result<Attempt> {
        let _m = self.mutex()?;
        let class = me.class;

        // Everything running, both classes: the budget is machine-wide.
        let mut slots = self.scan_slots(class)?;
        let other = self.scan_slots(match class {
            Class::Heavy => Class::Light,
            Class::Light => Class::Heavy,
        })?;
        let running: Vec<Running> = slots
            .iter()
            .chain(other.iter())
            .filter_map(|s| match s {
                // A held slot with no readable info is not a job: see `is_phantom`.
                SlotState::Busy(i) if is_phantom(i) => None,
                SlotState::Busy(i) => Some(Running {
                    est: i.est.unwrap_or_else(|| self.estimate_for(&i.label, i.class)),
                    rss_now_kb: i.rss_now_kb,
                }),
                SlotState::Free { .. } => None,
            })
            .collect();
        drop(other);
        let busy: Vec<&JobInfo> = slots
            .iter()
            .filter_map(|s| match s {
                SlotState::Busy(i) => Some(&**i),
                SlotState::Free { .. } => None,
            })
            .collect();
        let busy_note = busy_summary(class, &busy);
        let any_busy = !busy.is_empty();

        // One pressure sample per attempt while anything runs. It drives the
        // gate, the budget's memory side, and the AIMD scale.
        let readings = (!running.is_empty()).then(|| self.read_pressure(&me.cwd));
        let mut aimd = self.load_aimd();
        let holds = readings.as_ref().map(|r| pressure::holds(r, &self.config.gate)).unwrap_or_default();
        if readings.is_some() {
            aimd.observe(!holds.is_empty(), now_ms(), &self.config.budget);
            self.save_aimd(&aimd);
        }
        let empty = Readings::default();
        let r = readings.as_ref().unwrap_or(&empty);
        let fits = |e: Option<Estimate>| e.map_or(Ok(()), |e| budget::fits(e, &running, r, &self.config.budget, aimd.scale));

        // FIFO, with two bounded exceptions: an earlier waiter blocked only on
        // a named lock, or one that does not fit the budget and has been passed
        // fewer than `max_passes` times, does not hold up the line.
        let mut ahead = 0;
        let mut would_pass = Vec::new();
        for w in self.live_tickets()?.iter().filter(|w| w.class == class && w.ticket < me.ticket) {
            if !w.locks.is_empty() && self.acquire_locks(&w.locks)?.is_err() {
                continue;
            }
            if fits(w.est).is_err() && self.passes(w.ticket) < self.config.budget.max_passes {
                would_pass.push(w.ticket);
                continue;
            }
            ahead += 1;
        }
        if ahead > 0 {
            return Ok(Attempt::wait(format!("{ahead} ahead in the {} queue / {busy_note}", class.name())));
        }
        let Some(pos) = slots.iter().position(|s| matches!(s, SlotState::Free { .. })) else {
            return Ok(Attempt::wait(busy_note));
        };
        let locks = match self.acquire_locks(&me.locks)? {
            Ok(files) => files,
            Err((why, by_ticket)) => {
                return Ok(Attempt::Wait {
                    reason: format!("{why} / {busy_note}"),
                    by_ticket,
                })
            }
        };
        // The gate holds the head of the heavy queue — but never when nothing
        // else heavy is running. Then waiting cannot relieve anything we
        // control, and holding would stall every commit on the machine forever
        // on a box that is merely busy with work outside the queue.
        if class == Class::Heavy && any_busy && !self.config.gate.disabled() && !holds.is_empty() {
            return Ok(Attempt::wait(format!("{} / {busy_note}", holds.join(" / "))));
        }
        if let Err(why) = fits(me.est) {
            return Ok(Attempt::wait(format!("{why} (scale {:.2}) / {busy_note}", aimd.scale)));
        }
        let SlotState::Free { index, mut file } = slots.swap_remove(pos) else {
            return Ok(Attempt::wait(busy_note));
        };
        let info = JobInfo {
            slot: Some(index),
            admitted_at_ms: Some(now_ms()),
            waiting_on: None,
            waiting_on_since_ms: None,
            blocked_by_ticket: None,
            ..me.clone()
        };
        rewrite(&mut file, &info)?;
        let mut locks = locks;
        for l in &mut locks {
            rewrite(l, &info)?;
        }
        // Only now that we are really admitted does passing count against them.
        for t in would_pass {
            let _ = write_atomic(&self.passes_path(t), format!("{}\n", self.passes(t) + 1).as_bytes());
        }
        let _ = fs::remove_file(ticket_path.with_extension("passes"));
        match fs::remove_file(ticket_path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e).with_context(|| format!("removing {}", ticket_path.display())),
            _ => {}
        }
        Ok(Attempt::Admitted {
            slot: index,
            file,
            locks,
            readings: readings.map(Box::new),
        })
    }

    fn lock_path(&self, name: &str) -> PathBuf {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(name.as_bytes());
        let hex: String = digest.iter().take(8).fold(String::new(), |mut h, b| {
            use std::fmt::Write as _;
            let _ = write!(h, "{b:02x}");
            h
        });
        self.locks_dir().join(format!("{hex}.lock"))
    }

    /// Take every named lock, or none: `Ok(Err((reason, holder's ticket)))`
    /// names the first one somebody else holds. Call under the mutex.
    fn acquire_locks(&self, names: &[String]) -> Result<std::result::Result<Vec<File>, Blocked>> {
        let mut held = Vec::new();
        for name in names {
            let path = self.lock_path(name);
            let mut f = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .with_context(|| format!("opening {}", path.display()))?;
            match f.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => {
                    let mut text = String::new();
                    let _ = f.read_to_string(&mut text);
                    let holder = serde_json::from_str::<JobInfo>(&text).ok();
                    let who = holder.as_ref().map_or_else(|| "?".to_string(), |j| format!("{} (#{})", j.label, j.ticket));
                    return Ok(Err((format!("lock {name} held by {who}"), holder.map(|j| j.ticket))));
                }
                Err(TryLockError::Error(e)) => return Err(e).with_context(|| format!("locking {}", path.display())),
            }
            if let Some(target) = name.strip_prefix("cargo:") {
                if let Some(busy) = cargo_lock_busy(Path::new(target)) {
                    return Ok(Err((format!("cargo is building outside the queue ({})", busy.display()), None)));
                }
            }
            held.push(f);
        }
        Ok(Ok(held))
    }

    /// The volumes a heavy job will fill: its cwd, the cargo target dir, and
    /// whatever the config adds.
    fn disk_paths(&self, cwd: &Path) -> Vec<PathBuf> {
        let mut out = vec![cwd.to_path_buf(), self.cargo_target_for(cwd)];
        out.extend(self.config.gate.disk_paths.iter().cloned());
        out.dedup();
        out
    }

    fn record(&self, e: &HistoryEntry) {
        if let Err(err) = self.store().record(e) {
            eprintln!("th ci-queue: could not write history ({err:#})");
        }
    }

    pub fn history(&self, n: usize) -> Vec<HistoryEntry> {
        self.store().recent(n).unwrap_or_else(|e| {
            eprintln!("th ci-queue: could not read history ({e:#})");
            Vec::new()
        })
    }

    /// Everything `th ci-queue status` shows.
    pub fn snapshot(&self, history: usize, cwd: &Path) -> Result<Snapshot> {
        self.ensure_dirs()?;
        let (running, waiting) = {
            let _m = self.mutex()?;
            let mut running = Vec::new();
            for class in [Class::Heavy, Class::Light] {
                for s in self.scan_slots(class)? {
                    if let SlotState::Busy(i) = s {
                        if !is_phantom(&i) {
                            running.push(*i);
                        }
                    }
                }
            }
            (running, self.live_tickets()?)
        };
        let (readings, aimd) = {
            let _m = self.mutex()?;
            (self.read_pressure(cwd), self.load_aimd())
        };
        let holds = pressure::holds(&readings, &self.config.gate);
        let budget_running: Vec<Running> = running
            .iter()
            .map(|i| Running {
                est: i.est.unwrap_or_else(|| self.estimate_for(&i.label, i.class)),
                rss_now_kb: i.rss_now_kb,
            })
            .collect();
        let budget = budget::view(&budget_running, &readings, &self.config.budget, aimd.scale);
        Ok(Snapshot {
            schema: SNAPSHOT_SCHEMA,
            budget,
            dir: self.dir.clone(),
            config: self.config.clone(),
            now_ms: now_ms(),
            running,
            waiting,
            readings,
            holds,
            history: self.history(history),
        })
    }
}

/// Why a named lock is unavailable, and the ticket holding it (if queued).
type Blocked = (String, Option<u64>);

impl Attempt {
    const fn wait(reason: String) -> Self {
        Self::Wait { reason, by_ticket: None }
    }
}

enum Attempt {
    Admitted {
        slot: usize,
        file: File,
        locks: Vec<File>,
        readings: Option<Box<Readings>>,
    },
    Wait {
        reason: String,
        by_ticket: Option<u64>,
    },
}

enum SlotState {
    Free { index: usize, file: File },
    Busy(Box<JobInfo>),
}

/// The shape of [`Snapshot`] as JSON. Bump on any breaking change, so a
/// consumer (`th ci-queue top`, the daemon's web panel) can refuse a shape it
/// does not understand instead of rendering garbage.
///
/// 2: history from SQLite with measured memory/CPU; `JobInfo.est` and
/// `rss_now_kb`; the `budget` view.
pub const SNAPSHOT_SCHEMA: u32 = 2;

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub schema: u32,
    /// Where the capacity budget stands right now.
    pub budget: budget::View,
    pub dir: PathBuf,
    pub config: Config,
    pub now_ms: u64,
    pub running: Vec<JobInfo>,
    pub waiting: Vec<JobInfo>,
    pub readings: Readings,
    /// What the gate would hold a new heavy job on right now.
    pub holds: Vec<String>,
    pub history: Vec<HistoryEntry>,
}

/// A held slot. Dropping it (or dying) releases the slot; [`Admission::finish`]
/// also records the job in the history.
pub struct Admission {
    /// `None` for a nested job, which runs under its enclosing job's slot.
    lock: Option<File>,
    /// The named locks (`--lock`), held for the job's whole run.
    held_locks: Vec<File>,
    info: JobInfo,
    waited: Duration,
    queue: Option<Queue>,
    cmd_hash: Option<String>,
    /// Machine pressure when admitted, for history.
    pressure: Option<Readings>,
}

impl Admission {
    fn nested(req: &Request, _outer: String) -> Self {
        Self {
            lock: None,
            held_locks: Vec::new(),
            info: JobInfo {
                class: req.class,
                ticket: 0,
                label: req.label.clone(),
                pid: std::process::id(),
                child_pid: None,
                cwd: req.cwd.clone(),
                queued_at_ms: now_ms(),
                admitted_at_ms: Some(now_ms()),
                slot: None,
                locks: Vec::new(),
                waiting_on: None,
                waiting_on_since_ms: None,
                blocked_by_ticket: None,
                est: None,
                rss_now_kb: None,
            },
            waited: Duration::ZERO,
            queue: None,
            cmd_hash: None,
            pressure: None,
        }
    }

    /// `heavy-1` — what a nested job sees in `NESTED_ENV`.
    pub fn slot_name(&self) -> String {
        self.info
            .slot
            .map_or_else(|| "nested".to_string(), |i| format!("{}-{i}", self.info.class.name()))
    }

    #[cfg(test)]
    pub const fn is_nested(&self) -> bool {
        self.lock.is_none()
    }

    /// Note the spawned job's pid in the slot file, for `status`.
    pub fn set_child(&mut self, pid: u32) {
        self.info.child_pid = Some(pid);
        let (Some(file), Some(q)) = (self.lock.as_mut(), self.queue.as_ref()) else {
            return;
        };
        if let Ok(_m) = q.mutex() {
            let _ = rewrite(file, &self.info);
        }
    }

    /// A callback that publishes the job's live memory into its slot (for
    /// `status` and the budget), for the sampler thread. It writes through its
    /// own dup of the slot fd — same open file, same lock, still close-on-exec —
    /// and only under the queue mutex. Dropping it never releases the slot:
    /// the original fd, held here, keeps the flock.
    ///
    /// It re-reads the slot before writing, so it only ever changes
    /// `rss_now_kb`: a snapshot of the info taken before the job was spawned
    /// would otherwise overwrite the `child_pid` that `set_child` wrote.
    pub fn progress(&self) -> Option<impl FnMut(u64) + Send + 'static> {
        let mut file = self.lock.as_ref()?.try_clone().ok()?;
        let q = self.queue.clone()?;
        let fallback = self.info.clone();
        Some(move |kb: u64| {
            let Ok(_m) = q.mutex() else {
                return;
            };
            let mut text = String::new();
            let current = file
                .seek(SeekFrom::Start(0))
                .and_then(|_| file.read_to_string(&mut text))
                .ok()
                .and_then(|_| serde_json::from_str::<JobInfo>(&text).ok());
            let mut info = current.unwrap_or_else(|| fallback.clone());
            info.rss_now_kb = Some(kb);
            let _ = rewrite(&mut file, &info);
        })
    }

    /// Record the finished job in the history and release the slot.
    pub fn finish(self, ended: Ended, ran: Duration, usage: Usage) {
        if let Some(q) = &self.queue {
            q.record(&HistoryEntry {
                label: self.info.label.clone(),
                class: Some(self.info.class),
                cwd: self.info.cwd.clone(),
                repo: repo_root(&self.info.cwd),
                cmd_hash: self.cmd_hash.clone(),
                ticket: self.info.ticket,
                queued_at_ms: self.info.queued_at_ms,
                finished_at_ms: now_ms(),
                wait_ms: millis(self.waited),
                run_ms: millis(ran),
                outcome: ended.outcome().into(),
                exit: ended.exit_code(),
                peak_group_rss_kb: usage.peak_group_rss_kb,
                max_single_rss_kb: usage.max_single_rss_kb,
                cpu_ms: usage.cpu_ms,
                est_rss_kb: self.info.est.map(|e| e.rss_kb),
                est_millicores: self.info.est.map(|e| e.millicores),
                pressure: self.pressure.clone(),
            });
        }
        drop(self.held_locks);
        drop(self.lock);
    }
}

fn busy_summary(class: Class, busy: &[&JobInfo]) -> String {
    if busy.is_empty() {
        return format!("0 {} busy", class.name());
    }
    let now = now_ms();
    let who: Vec<String> = busy
        .iter()
        .map(|j| {
            let secs = j.admitted_at_ms.map_or(0, |a| now.saturating_sub(a) / 1000);
            let place = j
                .cwd
                .file_name()
                .map_or_else(|| j.cwd.display().to_string(), |n| n.to_string_lossy().into_owned());
            format!("{} (pid {}, {secs}s, {place})", j.label, j.child_pid.unwrap_or(j.pid))
        })
        .collect();
    format!("{} {} busy: {}", busy.len(), class.name(), who.join("; "))
}

/// A slot that is locked but holds no job description. A real holder writes
/// its info under the mutex before the mutex is released, so under the mutex
/// this is never a running job: it is a lock fd briefly inherited by a child
/// another thread of this process forked (between its fork and exec), which
/// holds the open file — and so the flock — until the exec closes it. It is
/// still busy (it cannot be taken), but it is not counted or shown.
fn is_phantom(i: &JobInfo) -> bool {
    i.pid == 0 && i.label == "?"
}

fn unknown_job(ticket: u64) -> JobInfo {
    JobInfo {
        class: Class::Heavy,
        ticket,
        label: "?".into(),
        pid: 0,
        child_pid: None,
        cwd: PathBuf::new(),
        queued_at_ms: 0,
        admitted_at_ms: None,
        slot: None,
        locks: Vec::new(),
        waiting_on: None,
        waiting_on_since_ms: None,
        blocked_by_ticket: None,
        est: None,
        rss_now_kb: None,
    }
}

/// The nearest ancestor of `cwd` holding a `.git` (dir, or file in a worktree).
fn repo_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

/// A cargo build holding `<target>/<profile>/.cargo-lock` right now, queued or
/// not. Probed with a non-blocking flock that is released at once; a file
/// that does not exist is not created.
fn cargo_lock_busy(target: &Path) -> Option<PathBuf> {
    let profiles = fs::read_dir(target).ok()?;
    for entry in profiles.filter_map(Result::ok) {
        let lock = entry.path().join(".cargo-lock");
        let Ok(f) = File::open(&lock) else {
            continue;
        };
        if matches!(f.try_lock(), Err(TryLockError::WouldBlock)) {
            return Some(lock);
        }
    }
    None
}

/// Replace a locked file's contents in place (the lock is on this fd, so the
/// file cannot be swapped out by rename).
fn rewrite(f: &mut File, info: &JobInfo) -> Result<()> {
    f.set_len(0)?;
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&serde_json::to_vec(info)?)?;
    f.flush()?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("renaming onto {}", path.display()))?;
    Ok(())
}

/// `CARGO_TARGET_DIR`, else `[build] target-dir` from `~/.cargo/config.toml`.
fn cargo_target_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CARGO_TARGET_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs_next::home_dir().map(|h| h.join(".cargo")))?;
    let text = fs::read_to_string(home.join("config.toml")).ok()?;
    let v: toml::Value = toml::from_str(&text).ok()?;
    v.get("build")?.get("target-dir")?.as_str().map(PathBuf::from)
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, millis)
}

pub fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

// Under plain `cargo test` these share one process with every other test, and
// a sibling that forks (Command falls back to fork+exec when, e.g., PATH is
// changed) briefly inherits whatever lock fds this process holds at that
// instant: a released slot can read as busy for the fork→exec window. CI runs
// nextest (a process per test), where that cannot happen; `is_phantom` keeps an
// info-less held slot out of the counts either way.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Mutex;

    /// Readings a test controls. Nothing here reads the real machine.
    struct FakeProbe(Mutex<Readings>);

    impl Probe for FakeProbe {
        fn read(&self, _disk_paths: &[PathBuf]) -> Readings {
            self.0.lock().unwrap().clone()
        }
    }

    fn calm() -> Readings {
        Readings {
            mem_total_bytes: Some(64 << 30),
            mem_available_bytes: Some(26 << 30),
            memory_pressure_level: Some(1),
            swap_total_bytes: Some(0),
            swap_used_bytes: Some(0),
            load1: Some(2.0),
            cpu_busy_pct: Some(20.0),
            cores: 12,
            disks: vec![],
        }
    }

    fn swapping() -> Readings {
        Readings {
            memory_pressure_level: Some(4),
            swap_total_bytes: Some(100),
            swap_used_bytes: Some(91),
            ..calm()
        }
    }

    struct Fx {
        _tmp: tempfile::TempDir,
        q: Queue,
        probe: Arc<FakeProbe>,
    }

    fn fx(heavy: usize) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.slots.heavy = heavy;
        config.run.poll_ms = 10;
        config.run.note_every_secs = 3600;
        let probe = Arc::new(FakeProbe(Mutex::new(calm())));
        let q = Queue::at(tmp.path().join("q"), config, probe.clone());
        Fx { _tmp: tmp, q, probe }
    }

    fn req(class: Class, label: &str, max_wait_ms: u64) -> Request {
        Request {
            class,
            label: label.into(),
            cwd: PathBuf::from("/work"),
            max_wait: Duration::from_millis(max_wait_ms),
            locks: Vec::new(),
            cmd_hash: None,
        }
    }

    fn locked(class: Class, label: &str, max_wait_ms: u64, lock: &str) -> Request {
        Request {
            locks: vec![lock.to_string()],
            ..req(class, label, max_wait_ms)
        }
    }

    fn admit(q: &Queue, class: Class, label: &str) -> Admission {
        q.admit(&req(class, label, 5_000)).unwrap_or_else(|e| panic!("{label} not admitted: {e:?}"))
    }

    /// Admit on a thread and report back when it lands.
    fn admit_async(q: &Queue, class: Class, label: &str) -> mpsc::Receiver<Admission> {
        let (tx, rx) = mpsc::channel();
        let (q, label) = (q.clone(), label.to_string());
        std::thread::spawn(move || {
            if let Ok(a) = q.admit(&req(class, &label, 10_000)) {
                let _ = tx.send(a);
            }
        });
        rx
    }

    #[test]
    fn slots_serialize_a_class() {
        let f = fx(1);
        let a = admit(&f.q, Class::Heavy, "a");
        let b = admit_async(&f.q, Class::Heavy, "b");
        assert!(b.recv_timeout(Duration::from_millis(300)).is_err(), "b ran while a held the only slot");
        a.finish(Ended::Exited(0), Duration::ZERO, Usage::default());
        b.recv_timeout(Duration::from_secs(5)).expect("b admitted once a finished");
    }

    #[test]
    fn classes_do_not_share_slots() {
        let f = fx(1);
        let _heavy = admit(&f.q, Class::Heavy, "h");
        let light = f.q.admit(&req(Class::Light, "l", 500));
        assert!(light.is_ok(), "a light job waited on a heavy slot: {light:?}", light = light.err());
    }

    #[test]
    fn n_slots_admit_n_jobs_at_once() {
        let f = fx(2);
        let _a = admit(&f.q, Class::Heavy, "a");
        let _b = admit(&f.q, Class::Heavy, "b");
        let c = f.q.admit(&req(Class::Heavy, "c", 200));
        assert!(matches!(c, Err(AdmitError::WaitTimeout { .. })), "a third job ran on two slots");
    }

    /// Mutation-checked: with the `ahead > 0` early return in `try_admit`
    /// removed, waiters take slots in poll order and this fails.
    #[test]
    fn waiters_are_served_in_ticket_order() {
        let f = fx(1);
        let holder = admit(&f.q, Class::Heavy, "holder");
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for i in 0..6 {
            let (q, order) = (f.q.clone(), order.clone());
            handles.push(std::thread::spawn(move || {
                let a = q.admit(&req(Class::Heavy, &format!("w{i}"), 20_000)).unwrap();
                order.lock().unwrap().push(i);
                std::thread::sleep(Duration::from_millis(15));
                a.finish(Ended::Exited(0), Duration::ZERO, Usage::default());
            }));
            // Stagger so tickets are drawn in order 0..6.
            wait_for(|| f.q.ticket_files().unwrap().len() == i + 1);
        }
        holder.finish(Ended::Exited(0), Duration::ZERO, Usage::default());
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), (0..6).collect::<Vec<_>>());
    }

    #[test]
    fn a_dead_waiters_ticket_does_not_block_the_line() {
        let f = fx(1);
        f.q.ensure_dirs().unwrap();
        // A ticket file nobody holds: its waiter was kill -9'd mid-wait.
        let mut ghost = unknown_job(1);
        let (path, file) = f.q.take_ticket(&mut ghost).unwrap();
        drop(file);
        assert!(path.exists());
        let a = f.q.admit(&req(Class::Heavy, "a", 1_000));
        assert!(a.is_ok(), "a dead ticket held the line");
        assert!(!path.exists(), "the dead ticket was not reaped");
    }

    #[test]
    fn a_dropped_admission_frees_its_slot() {
        let f = fx(1);
        let a = admit(&f.q, Class::Heavy, "a");
        drop(a);
        admit(&f.q, Class::Heavy, "b");
    }

    #[test]
    fn the_wait_cap_gives_up_and_leaves_no_ticket() {
        let f = fx(1);
        let _a = admit(&f.q, Class::Heavy, "a");
        let began = Instant::now();
        let r = f.q.admit(&req(Class::Heavy, "b", 150));
        let Err(AdmitError::WaitTimeout { waited, reason }) = r else {
            panic!("expected a wait timeout");
        };
        assert!(waited >= Duration::from_millis(150) && began.elapsed() < Duration::from_secs(3));
        assert!(reason.contains("1 heavy busy: a"), "{reason}");
        assert!(f.q.ticket_files().unwrap().is_empty(), "a timed-out waiter left its ticket behind");
        let h = f.q.history(10);
        assert_eq!(
            h.last().map(|e| (e.outcome.as_str(), e.exit)),
            Some(("wait-timeout", super::super::EXIT_WAIT_TIMEOUT))
        );
    }

    #[test]
    fn the_gate_holds_heavy_under_pressure_and_releases_when_it_clears() {
        let f = fx(2);
        let _a = admit(&f.q, Class::Heavy, "a");
        *f.probe.0.lock().unwrap() = swapping();
        let b = admit_async(&f.q, Class::Heavy, "b");
        assert!(b.recv_timeout(Duration::from_millis(300)).is_err(), "b admitted while the machine was swapping");
        // The same pressure never holds a light job.
        assert!(f.q.admit(&req(Class::Light, "l", 200)).is_ok());
        *f.probe.0.lock().unwrap() = calm();
        b.recv_timeout(Duration::from_secs(5)).expect("b admitted once pressure cleared");
    }

    #[test]
    fn the_gate_reason_names_the_pressure() {
        let f = fx(2);
        let _a = admit(&f.q, Class::Heavy, "a");
        *f.probe.0.lock().unwrap() = swapping();
        let Err(AdmitError::WaitTimeout { reason, .. }) = f.q.admit(&req(Class::Heavy, "b", 100)) else {
            panic!("expected the gate to hold b");
        };
        assert!(reason.starts_with("memory pressure critical / swap 91% / 1 heavy busy: a"), "{reason}");
    }

    /// 2026-09-30: load far past its backstop, CPUs mostly idle, the AIMD
    /// scale already driven to its floor by the old load gate. The scale must
    /// climb back and the waiter be admitted; with the CPUs saturated it must
    /// not.
    #[test]
    fn the_budget_scale_recovers_on_idle_cpus_however_high_the_load() {
        let parked = Readings {
            load1: Some(200.0),
            cpu_busy_pct: Some(20.0),
            ..calm()
        };
        let floor = Aimd {
            scale: 0.25,
            ..Aimd::default()
        };

        let f = fx(2);
        let _a = admit(&f.q, Class::Heavy, "a");
        f.q.save_aimd(&floor);
        *f.probe.0.lock().unwrap() = parked.clone();
        // 12 cores × 1.5 × 0.25 = 4.5 cores fits one 4-core heavy job, not two.
        assert!(f.q.admit(&req(Class::Heavy, "b", 5_000)).is_ok(), "idle CPUs at load 200 still held b");
        assert!(f.q.load_aimd().scale > 0.25, "scale {}", f.q.load_aimd().scale);

        let f = fx(2);
        let _a = admit(&f.q, Class::Heavy, "a");
        f.q.save_aimd(&floor);
        *f.probe.0.lock().unwrap() = Readings {
            cpu_busy_pct: Some(97.0),
            ..parked
        };
        let Err(AdmitError::WaitTimeout { reason, .. }) = f.q.admit(&req(Class::Heavy, "b", 300)) else {
            panic!("saturated CPUs admitted b");
        };
        assert!(reason.starts_with("cpu 97% busy / load 200.0 on 12 cores"), "{reason}");
        assert!((f.q.load_aimd().scale - 0.25).abs() < 1e-9, "scale grew while the CPUs were saturated");
    }

    /// A `th` too new to have measured a CPU interval of its own uses the
    /// reading the queue kept, while it is fresh.
    #[test]
    fn a_missing_cpu_reading_falls_back_to_the_kept_one() {
        let f = fx(2);
        let _a = admit(&f.q, Class::Heavy, "a");
        *f.probe.0.lock().unwrap() = Readings {
            cpu_busy_pct: Some(98.0),
            ..calm()
        };
        assert!(f.q.admit(&req(Class::Heavy, "b", 50)).is_err());
        *f.probe.0.lock().unwrap() = Readings { cpu_busy_pct: None, ..calm() };
        let snap = f.q.snapshot(0, Path::new("/work")).unwrap();
        assert!(snap.readings.cpu_busy_pct.is_some_and(|c| c > 90.0), "{:?}", snap.readings.cpu_busy_pct);
        assert_eq!(snap.holds, vec!["cpu 98% busy"]);
        // A stale one is not used.
        let old = CpuState {
            busy_pct: 98.0,
            at_ms: now_ms() - pressure::CPU_STALE_MS - 1_000,
        };
        fs::write(f.q.cpu_path(), serde_json::to_vec(&old).unwrap()).unwrap();
        let snap = f.q.snapshot(0, Path::new("/work")).unwrap();
        assert_eq!(snap.readings.cpu_busy_pct, None);
        assert!(snap.holds.is_empty(), "{:?}", snap.holds);
    }

    /// The never-deadlock rule: with nothing heavy running, the gate cannot
    /// hold — pressure from outside the queue is not ours to wait out.
    #[test]
    fn with_nothing_heavy_running_the_gate_admits_regardless() {
        let f = fx(2);
        *f.probe.0.lock().unwrap() = swapping();
        let a = f.q.admit(&req(Class::Heavy, "a", 200));
        assert!(a.is_ok(), "the gate held the only heavy job on the machine");
    }

    #[test]
    fn a_shrunk_config_still_counts_the_extra_running_slot() {
        let f = fx(2);
        let _a = admit(&f.q, Class::Heavy, "a");
        let _b = admit(&f.q, Class::Heavy, "b");
        let mut small = f.q;
        small.config.slots.heavy = 1;
        let snap = small.snapshot(0, Path::new("/work")).unwrap();
        assert_eq!(snap.running.len(), 2);
        assert!(matches!(small.admit(&req(Class::Heavy, "c", 100)), Err(AdmitError::WaitTimeout { .. })));
    }

    #[test]
    fn status_shows_running_and_waiting_jobs() {
        let f = fx(1);
        let mut a = admit(&f.q, Class::Heavy, "typecheck");
        a.set_child(4242);
        let _b = admit_async(&f.q, Class::Heavy, "clippy");
        wait_for(|| f.q.ticket_files().unwrap().len() == 1);
        let snap = f.q.snapshot(5, Path::new("/work")).unwrap();
        assert_eq!(snap.running.len(), 1);
        assert_eq!(snap.running[0].label, "typecheck");
        assert_eq!(snap.running[0].child_pid, Some(4242));
        assert_eq!(snap.running[0].slot, Some(1));
        assert_eq!(snap.waiting.len(), 1);
        assert_eq!(snap.waiting[0].label, "clippy");
        assert!(snap.holds.is_empty());
    }

    #[test]
    fn history_records_and_trims() {
        let mut f = fx(1);
        f.q.config.run.history_keep = 3;
        for i in 0..10 {
            admit(&f.q, Class::Light, &format!("j{i}")).finish(Ended::Exited(i % 2), Duration::from_millis(5), Usage::default());
        }
        let h = f.q.history(100);
        let labels: Vec<&str> = h.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["j7", "j8", "j9"]);
        assert_eq!(h.last().unwrap().exit, 1);
    }

    #[test]
    fn a_nested_job_runs_without_a_slot() {
        let f = fx(1);
        let mut nesting = f.q.clone();
        nesting.honor_nesting = true;
        let _outer = admit(&f.q, Class::Heavy, "outer");
        // Simulates the env an admitted job's children inherit.
        std::env::set_var(NESTED_ENV, "heavy-1");
        let inner = nesting.admit(&req(Class::Heavy, "inner", 100));
        std::env::remove_var(NESTED_ENV);
        let inner = inner.expect("a nested job must not wait on its own parent's slot");
        assert!(inner.is_nested());
    }

    #[test]
    fn a_lost_counter_never_reuses_a_live_ticket() {
        let f = fx(1);
        let _a = admit(&f.q, Class::Heavy, "a");
        let b = admit_async(&f.q, Class::Heavy, "b");
        wait_for(|| f.q.ticket_files().unwrap().len() == 1);
        fs::remove_file(f.q.dir().join("next-ticket")).unwrap();
        let mut c = unknown_job(0);
        let (_p, _f) = f.q.take_ticket(&mut c).unwrap();
        let b_ticket = f.q.ticket_files().unwrap()[0].0;
        assert!(c.ticket > b_ticket, "counter reset handed out {} while {b_ticket} is live", c.ticket);
        drop(b);
    }

    #[test]
    fn a_named_lock_admits_one_holder_at_a_time() {
        let f = fx(2);
        let a = f.q.admit(&locked(Class::Light, "a", 1_000, "cargo:/t")).unwrap();
        let Err(AdmitError::WaitTimeout { reason, .. }) = f.q.admit(&locked(Class::Light, "b", 100, "cargo:/t")) else {
            panic!("two holders of one named lock were admitted together");
        };
        assert!(reason.starts_with("lock cargo:/t held by a"), "{reason}");
        // A different resource is not excluded.
        assert!(f.q.admit(&locked(Class::Light, "c", 100, "cargo:/other")).is_ok());
        a.finish(Ended::Exited(0), Duration::ZERO, Usage::default());
        assert!(f.q.admit(&locked(Class::Light, "b", 1_000, "cargo:/t")).is_ok());
    }

    /// Mutation-checked: counting lock-blocked earlier waiters as `ahead`
    /// makes c wait behind b even though a slot is free.
    #[test]
    fn a_waiter_blocked_on_a_lock_does_not_hold_up_the_line() {
        let f = fx(2);
        let _a = f.q.admit(&locked(Class::Heavy, "a", 1_000, "cargo:/t")).unwrap();
        let _b = {
            let q = f.q.clone();
            std::thread::spawn(move || q.admit(&locked(Class::Heavy, "b", 3_000, "cargo:/t")).is_ok())
        };
        wait_for(|| f.q.ticket_files().unwrap().len() == 1);
        let c = f.q.admit(&req(Class::Heavy, "c", 500));
        assert!(c.is_ok(), "c waited behind b, which cannot run until a's lock is free");
    }

    #[test]
    fn cargos_own_lock_outside_the_queue_holds_a_cargo_job() {
        let f = fx(2);
        let target = f.q.dir().join("target");
        fs::create_dir_all(target.join("debug")).unwrap();
        let outside = File::create(target.join("debug/.cargo-lock")).unwrap();
        outside.lock().unwrap(); // a `cargo build` nobody queued
        let name = format!("cargo:{}", target.display());
        let Err(AdmitError::WaitTimeout { reason, .. }) = f.q.admit(&locked(Class::Heavy, "a", 150, &name)) else {
            panic!("admitted a cargo job to sit behind an outside build's lock");
        };
        assert!(reason.contains("cargo is building outside the queue"), "{reason}");
        drop(outside);
        assert!(f.q.admit(&locked(Class::Heavy, "a", 1_000, &name)).is_ok());
        assert!(!target.join("release/.cargo-lock").exists(), "probing must not create cargo's lock files");
    }

    #[test]
    fn a_waiter_publishes_what_holds_it() {
        let f = fx(2);
        let holder = f.q.admit(&locked(Class::Heavy, "clippy", 1_000, "cargo:/t")).unwrap();
        let _w = {
            let q = f.q.clone();
            std::thread::spawn(move || q.admit(&locked(Class::Heavy, "test", 5_000, "cargo:/t")).is_ok())
        };
        wait_for(|| {
            let snap = f.q.snapshot(0, Path::new("/work")).unwrap();
            snap.waiting.first().is_some_and(|w| w.waiting_on.is_some())
        });
        let snap = f.q.snapshot(0, Path::new("/work")).unwrap();
        assert_eq!(snap.schema, SNAPSHOT_SCHEMA);
        let w = &snap.waiting[0];
        let on = w.waiting_on.as_deref().unwrap();
        assert!(on.starts_with("lock cargo:/t held by clippy (#"), "{on}");
        assert_eq!(w.blocked_by_ticket, Some(snap.running[0].ticket));
        assert!(w.waiting_on_since_ms.is_some());
        drop(holder);
    }

    // ── capacity budget (phase 1.5) ──────────────────────────────────────

    const GB: u64 = 1024 * 1024; // in KiB

    /// Give `label` a history: `runs` measured exits peaking at `gb`.
    fn seed(q: &Queue, label: &str, gb: u64, runs: usize) {
        for _ in 0..runs {
            q.store()
                .record(&HistoryEntry {
                    label: label.into(),
                    class: Some(Class::Heavy),
                    outcome: "exit".into(),
                    run_ms: 1000,
                    cpu_ms: Some(100),
                    peak_group_rss_kb: Some(gb * GB),
                    ..HistoryEntry::default()
                })
                .unwrap();
        }
    }

    /// calm(): 26 GB available − 6 GB reserve = a 20 GB pool; 12 cores.
    fn budget_fx() -> Fx {
        let mut f = fx(6); // slots are not the limit here
        f.q.config.budget.aimd_max = 1.0; // keep the pool fixed while calm
        seed(&f.q, "big", 15, 3);
        seed(&f.q, "small", 1, 3);
        f
    }

    #[test]
    fn estimates_come_from_the_labels_history() {
        let f = budget_fx();
        let _a = admit(&f.q, Class::Heavy, "big");
        let _b = admit(&f.q, Class::Heavy, "unknown");
        let snap = f.q.snapshot(0, Path::new("/work")).unwrap();
        let est = |l: &str| snap.running.iter().find(|j| j.label == l).and_then(|j| j.est).unwrap();
        assert_eq!((est("big").rss_kb, est("big").from_runs), (15 * GB, 3));
        assert_eq!((est("unknown").rss_kb, est("unknown").from_runs), (4 * GB, 0), "no history: the heavy default");
        assert_eq!(snap.schema, 2);
        assert_eq!(snap.budget.mem_pool_kb, Some(20 * GB));
    }

    #[test]
    fn a_job_that_does_not_fit_waits_until_the_running_set_drains() {
        let f = budget_fx();
        let a = admit(&f.q, Class::Heavy, "big");
        let Err(AdmitError::WaitTimeout { reason, .. }) = f.q.admit(&req(Class::Heavy, "big", 150)) else {
            panic!("a second 15 GB job was admitted into a 20 GB pool");
        };
        assert!(reason.starts_with("memory budget: needs 15.0 GB + 15.0 GB committed > 20.0 GB"), "{reason}");
        a.finish(Ended::Exited(0), Duration::ZERO, Usage::default());
        assert!(f.q.admit(&req(Class::Heavy, "big", 1_000)).is_ok());
    }

    /// Invariant: at least one job is always admissible. Nothing running →
    /// admitted, whatever its estimate. Mutation-checked: without the
    /// `running.is_empty()` early return in `budget::fits`, this job (every
    /// core on the machine, 500 GB) waits forever.
    #[test]
    fn an_oversized_job_is_admitted_when_nothing_runs() {
        let f = budget_fx();
        for _ in 0..3 {
            f.q.store()
                .record(&HistoryEntry {
                    label: "enormous".into(),
                    outcome: "exit".into(),
                    run_ms: 1000,
                    cpu_ms: Some(1_000_000),
                    peak_group_rss_kb: Some(500 * GB),
                    ..HistoryEntry::default()
                })
                .unwrap();
        }
        assert!(f.q.admit(&req(Class::Heavy, "enormous", 200)).is_ok());
    }

    /// Mutation-checked: without the `max_passes` bound, the fourth small job
    /// also passes and the big one can starve.
    #[test]
    fn smaller_jobs_pass_a_big_waiter_at_most_max_passes_times() {
        let f = budget_fx(); // max_passes = 3
        let a = admit(&f.q, Class::Heavy, "big");
        let h = admit_async(&f.q, Class::Heavy, "big");
        wait_for(|| f.q.ticket_files().unwrap().len() == 1);
        let mut small = Vec::new();
        for i in 0..3 {
            let s = f.q.admit(&req(Class::Heavy, "small", 500));
            small.push(s.unwrap_or_else(|e| panic!("small job {i} did not pass the big waiter: {e:?}")));
        }
        let Err(AdmitError::WaitTimeout { reason, .. }) = f.q.admit(&req(Class::Heavy, "small", 300)) else {
            panic!("a fourth small job passed a waiter that had been passed max_passes times");
        };
        assert!(reason.starts_with("1 ahead in the heavy queue"), "{reason}");
        // The reservation holds until the running set drains, then the big one runs.
        a.finish(Ended::Exited(0), Duration::ZERO, Usage::default());
        for s in small {
            s.finish(Ended::Exited(0), Duration::ZERO, Usage::default());
        }
        h.recv_timeout(Duration::from_secs(10))
            .expect("the big waiter was admitted once the running set drained");
    }

    #[test]
    fn pressure_halves_the_budget_scale_and_status_shows_it() {
        let mut f = budget_fx();
        f.q.config.budget.aimd_max = 2.0;
        let _a = admit(&f.q, Class::Heavy, "small");
        *f.probe.0.lock().unwrap() = swapping();
        let _ = f.q.admit(&req(Class::Heavy, "small", 50)); // one sample under pressure
        let snap = f.q.snapshot(0, Path::new("/work")).unwrap();
        assert!((snap.budget.scale - 0.5).abs() < 1e-9, "scale {}", snap.budget.scale);
    }

    #[test]
    fn a_running_job_publishes_its_live_memory() {
        let f = budget_fx();
        let a = admit(&f.q, Class::Heavy, "big");
        let mut publish = a.progress().expect("a queued job can publish progress");
        publish(3 * GB);
        let snap = f.q.snapshot(0, Path::new("/work")).unwrap();
        assert_eq!(snap.running[0].rss_now_kb, Some(3 * GB));
        assert_eq!(snap.budget.mem_committed_kb, 12 * GB, "15 GB estimate − 3 GB already in use");
        drop(publish);
        // The progress writer is a dup of the slot fd: dropping it must not
        // release the slot while the admission still holds it.
        assert!(matches!(f.q.admit(&req(Class::Heavy, "big", 100)), Err(AdmitError::WaitTimeout { .. })));
        a.finish(Ended::Exited(0), Duration::ZERO, Usage::default());
    }

    /// CI caught this on #676: the progress writer held a copy of the info
    /// from before spawn, so its first sample erased `child_pid`.
    #[test]
    fn progress_never_erases_the_child_pid() {
        let f = budget_fx();
        let mut a = admit(&f.q, Class::Heavy, "big");
        let mut publish = a.progress().unwrap(); // taken before spawn, as run does
        a.set_child(4242);
        publish(GB);
        let snap = f.q.snapshot(0, Path::new("/work")).unwrap();
        assert_eq!(snap.running[0].child_pid, Some(4242));
        assert_eq!(snap.running[0].rss_now_kb, Some(GB));
        drop(publish);
        a.finish(Ended::Exited(0), Duration::ZERO, Usage::default());
    }

    #[test]
    fn finish_records_what_the_job_used() {
        let f = budget_fx();
        let a = admit(&f.q, Class::Heavy, "measured");
        a.finish(
            Ended::Exited(0),
            Duration::from_millis(2000),
            Usage {
                peak_group_rss_kb: Some(5 * GB),
                max_single_rss_kb: Some(GB),
                cpu_ms: Some(3000),
            },
        );
        let h = f.q.history(1);
        assert_eq!(
            (h[0].peak_group_rss_kb, h[0].max_single_rss_kb, h[0].cpu_ms),
            (Some(5 * GB), Some(GB), Some(3000))
        );
        assert_eq!(h[0].est_rss_kb, Some(4 * GB), "the estimate it was admitted on");
        // …and the next admission of that label estimates from it.
        let b = admit(&f.q, Class::Heavy, "measured");
        let snap = f.q.snapshot(0, Path::new("/work")).unwrap();
        let est = snap.running[0].est.unwrap();
        assert_eq!((est.rss_kb, est.millicores, est.from_runs), (5 * GB, 1500, 1));
        b.finish(Ended::Exited(0), Duration::ZERO, Usage::default());
    }

    #[test]
    fn resolve_lock_expands_cargo_to_its_target_dir() {
        let f = fx(1);
        let rel = f.q.clone().with_cargo_target("tgt".into());
        let q = f.q.with_cargo_target("/shared/target".into());
        assert_eq!(q.resolve_lock("cargo", Path::new("/repo")), "cargo:/shared/target");
        assert_eq!(rel.resolve_lock("cargo", Path::new("/repo")), "cargo:/repo/tgt");
        assert_eq!(q.resolve_lock("docker", Path::new("/repo")), "docker");
    }

    fn wait_for(cond: impl Fn() -> bool) {
        let t = Instant::now();
        while !cond() {
            assert!(t.elapsed() < Duration::from_secs(10), "condition never became true");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
