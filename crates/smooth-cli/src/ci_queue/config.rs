//! `~/.smooth/ci-queue.toml` — slot counts, the pressure gate's thresholds, and
//! the run knobs. Every field has a default, so a missing file is a valid
//! config, and so is a file that sets one key.
//!
//! A broken file is warned about and ignored rather than fatal: this runs
//! inside every pre-commit hook on the machine, and a typo in one config file
//! must not stop every commit in every worktree.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where the config lives unless `SMOOTH_CI_QUEUE_CONFIG` says otherwise.
pub fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SMOOTH_CI_QUEUE_CONFIG").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    dirs_next::home_dir().map(|h| h.join(".smooth").join("ci-queue.toml"))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub slots: Slots,
    pub gate: Gate,
    pub run: Run,
}

/// How many jobs of each class may run at once, machine-wide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Slots {
    pub heavy: usize,
    pub light: usize,
}

impl Default for Slots {
    fn default() -> Self {
        Self { heavy: 2, light: 6 }
    }
}

/// Thresholds that hold new **heavy** jobs while the machine is under
/// pressure. A value of 0 turns that signal off. None of them can hold a job
/// when no other heavy job is running (see `queue`), so a machine that is
/// simply busy with non-queued work still makes progress one job at a time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Gate {
    /// Hold while available memory (free + inactive + speculative + purgeable
    /// on macOS, `MemAvailable` on Linux) is below this percent of RAM.
    pub min_available_memory_pct: f64,
    /// macOS only: hold while `kern.memorystatus_vm_pressure_level` is above
    /// this (1 = normal, 2 = warn, 4 = critical). This is the kernel's own
    /// judgement and the best single memory signal the platform offers.
    pub max_memory_pressure_level: u32,
    /// Hold while swap is fuller than this percent AND memory is tight (the
    /// pressure level is above normal, or available memory is under twice
    /// `min_available_memory_pct`). Swap on its own is not a signal on macOS:
    /// swapped pages stay there long after the pressure that pushed them out
    /// has gone, so a quiet machine routinely reads over 90%.
    pub max_swap_used_pct: f64,
    /// Hold while the 1-minute load average is above this many per core.
    /// macOS load counts more than runnable threads, which is why the default
    /// is well above "busy".
    pub max_load_per_core: f64,
    /// Hold while any watched volume has less than this free: the job's cwd,
    /// the cargo target dir, and `disk_paths`.
    pub min_free_disk_gb: f64,
    /// Extra paths whose volumes must keep `min_free_disk_gb` free.
    pub disk_paths: Vec<PathBuf>,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            min_available_memory_pct: 5.0,
            max_memory_pressure_level: 1,
            max_swap_used_pct: 90.0,
            max_load_per_core: 4.0,
            min_free_disk_gb: 20.0,
            disk_paths: Vec::new(),
        }
    }
}

impl Gate {
    /// Every signal off — the gate can hold nothing, so nothing needs sampling.
    pub fn disabled(&self) -> bool {
        self.min_available_memory_pct <= 0.0
            && self.max_memory_pressure_level == 0
            && self.max_swap_used_pct <= 0.0
            && self.max_load_per_core <= 0.0
            && self.min_free_disk_gb <= 0.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Run {
    /// Default cap on time spent WAITING for a slot. Past it, `run` exits 75.
    pub max_wait_secs: u64,
    /// Default priority for jobs (`--qos` overrides). A job that holds a
    /// machine-shared lock never runs below `nice`: see [`Qos`].
    pub qos: Qos,
    /// After a timeout, how long the job's process group gets between SIGTERM
    /// and SIGKILL.
    pub kill_grace_secs: u64,
    /// How many finished jobs `history.jsonl` keeps.
    pub history_keep: usize,
    /// How often a waiting job re-checks the queue.
    pub poll_ms: u64,
    /// How often a waiting job prints its `waiting (…)` line.
    pub note_every_secs: u64,
}

impl Default for Run {
    fn default() -> Self {
        Self {
            max_wait_secs: 1800,
            qos: Qos::Background,
            kill_grace_secs: 10,
            history_keep: 500,
            poll_ms: 1000,
            note_every_secs: 30,
        }
    }
}

/// How hard a job yields to everything else on the machine.
///
/// **Rule: a job that takes a machine-shared lock must not run at
/// `Background`.** Darwin background QoS throttles CPU and I/O almost to
/// zero under contention, and Linux's idle I/O class only gets the disk when
/// nobody else wants it. On 2026-09-26 a `cargo test` under `taskpolicy -b`
/// at load ~270 sat at 0% CPU for 35+ minutes while holding the shared cargo
/// target's `.cargo-lock`, and every other Rust build on the machine queued
/// behind it — priority inversion. So a job with any `--lock` is capped at
/// `Nice` (see [`Qos::effective`]); `Background` stays for lock-free work
/// such as a typecheck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Qos {
    /// `taskpolicy -b` on macOS; `ionice -c 3` + `nice -n 10` on Linux.
    Background,
    /// `nice -n 10`: lower CPU priority that can still make progress.
    Nice,
    /// Unchanged priority.
    Normal,
}

impl Qos {
    /// What a job actually runs at, given whether it holds a shared lock.
    pub const fn effective(self, holds_shared_lock: bool) -> Self {
        match self {
            Self::Background if holds_shared_lock => Self::Nice,
            q => q,
        }
    }
}

impl Config {
    /// Read `path`. Missing → defaults. Unreadable or invalid → defaults plus a
    /// warning on stderr naming the file and the problem.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                eprintln!("th ci-queue: cannot read {} ({e}) — using defaults", path.display());
                return Self::default();
            }
        };
        match Self::parse(&text) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("th ci-queue: ignoring {} — {e}", path.display());
                eprintln!("  using defaults until it parses; `th ci-queue status` shows what is in effect");
                Self::default()
            }
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut c: Self = toml::from_str(text).map_err(|e| e.message().to_string())?;
        // A zero-slot class could never admit anything; treat it as one.
        c.slots.heavy = c.slots.heavy.max(1);
        c.slots.light = c.slots.light.max(1);
        c.run.poll_ms = c.run.poll_ms.max(10);
        Ok(c)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn one_key_overrides_one_key() {
        let c = Config::parse("[slots]\nheavy = 3\n").unwrap();
        assert_eq!(c.slots.heavy, 3);
        assert_eq!(c.slots.light, Slots::default().light);
        assert_eq!(c.gate, Gate::default());
    }

    #[test]
    fn zero_slots_is_clamped_to_one_so_the_class_can_still_run() {
        let c = Config::parse("[slots]\nheavy = 0\nlight = 0\n").unwrap();
        assert_eq!((c.slots.heavy, c.slots.light), (1, 1));
    }

    #[test]
    fn a_typo_is_an_error_not_a_silent_default() {
        let err = Config::parse("[gate]\nmax_swap_pct = 50\n").unwrap_err();
        assert!(err.contains("max_swap_pct"), "{err}");
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(Some(&tmp.path().join("nope.toml"))), Config::default());
    }

    #[test]
    fn a_broken_file_falls_back_to_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("ci-queue.toml");
        std::fs::write(&p, "[slots\nheavy = ").unwrap();
        assert_eq!(Config::load(Some(&p)), Config::default());
    }

    #[test]
    fn a_job_holding_a_shared_lock_never_runs_at_background_qos() {
        assert_eq!(Qos::Background.effective(true), Qos::Nice);
        assert_eq!(Qos::Background.effective(false), Qos::Background);
        assert_eq!(Qos::Nice.effective(true), Qos::Nice);
        assert_eq!(Qos::Normal.effective(true), Qos::Normal);
        assert_eq!(Config::parse("[run]\nqos = \"nice\"\n").unwrap().run.qos, Qos::Nice);
    }

    #[test]
    fn all_zero_thresholds_disable_the_gate() {
        assert!(!Gate::default().disabled());
        let c = Config::parse(
            "[gate]\nmin_available_memory_pct = 0\nmax_memory_pressure_level = 0\nmax_swap_used_pct = 0\nmax_load_per_core = 0\nmin_free_disk_gb = 0\n",
        )
        .unwrap();
        assert!(c.gate.disabled());
    }
}
