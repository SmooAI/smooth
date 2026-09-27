//! Capacity-aware admission: estimates, the budget test, and AIMD on the scale.
//!
//! All of it is pure over its inputs, so the rules are tested directly. The
//! queue supplies readings, the running jobs' estimates and live memory, and
//! the persisted [`Aimd`] state, all under its mutex.
//!
//! **Memory.** The running jobs have already taken what they use now out of
//! "available"; what they may still take is `estimate − using now`. A new job
//! fits while
//!
//! ```text
//! new.estimate + Σ running max(0, estimate − rss_now)  ≤  (available − reserve) × min(scale, mem_scale_max)
//! ```
//!
//! **CPU.** `new.cores + Σ running cores ≤ cores × cpu_factor × scale`.
//!
//! **Never deadlock.** With nothing running, the head is admitted whatever it
//! is estimated at — waiting could not free anything the queue controls.

use serde::{Deserialize, Serialize};

use super::config::Budget as Cfg;
use super::history::Sample;
use super::pressure::Readings;
use super::queue::Class;

const KIB_PER_GIB: f64 = 1024.0 * 1024.0;

/// What a job is expected to need.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Estimate {
    pub rss_kb: u64,
    /// Cores × 1000, so estimates stay integer (and `Eq`) in ticket JSON.
    pub millicores: u64,
    /// How many past runs it came from; 0 = the class default.
    pub from_runs: usize,
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "non-negative, well within u64")]
fn to_u64(v: f64) -> u64 {
    v.max(0.0).round() as u64
}

#[allow(clippy::cast_precision_loss, reason = "estimates, not accounting")]
fn to_f64(v: u64) -> f64 {
    v as f64
}

/// p90 of the label's peak group memory; mean cores (CPU time / wall time).
/// With no history, the class default — deliberately generous, so an unknown
/// job is sized up, not down.
pub fn estimate(samples: &[Sample], class: Class, cfg: &Cfg, cores: usize) -> Estimate {
    if samples.is_empty() {
        let (gb, c) = match class {
            Class::Heavy => (cfg.heavy_rss_gb, cfg.heavy_cores),
            Class::Light => (cfg.light_rss_gb, cfg.light_cores),
        };
        return Estimate {
            rss_kb: to_u64(gb * KIB_PER_GIB),
            millicores: to_u64(c * 1000.0),
            from_runs: 0,
        };
    }
    let mut rss: Vec<u64> = samples.iter().map(|s| s.peak_group_rss_kb).collect();
    rss.sort_unstable();
    // Nearest-rank p90: the smallest value at or above 90% of the runs.
    let rank = (rss.len() * 9).div_ceil(10).max(1);
    let p90 = rss[rank - 1];
    let per_run: Vec<f64> = samples.iter().filter(|s| s.run_ms > 0).map(|s| to_f64(s.cpu_ms) / to_f64(s.run_ms)).collect();
    let mean = if per_run.is_empty() {
        1.0
    } else {
        per_run.iter().sum::<f64>() / to_f64(per_run.len() as u64)
    };
    let max_cores = to_f64(cores.max(1) as u64);
    Estimate {
        rss_kb: p90,
        millicores: to_u64(mean.clamp(0.1, max_cores) * 1000.0),
        from_runs: samples.len(),
    }
}

/// A running job as the budget sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Running {
    pub est: Estimate,
    /// Its process group's memory at the last sample, if sampled yet.
    pub rss_now_kb: Option<u64>,
}

/// Where the budget stands — shown by `status`, and why a waiter waits.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub scale: f64,
    /// Memory jobs may be admitted into: (available − reserve) × memory scale.
    pub mem_pool_kb: Option<u64>,
    /// What the running jobs may still grow into.
    pub mem_committed_kb: u64,
    pub cpu_budget_millicores: u64,
    pub cpu_used_millicores: u64,
}

pub fn view(running: &[Running], r: &Readings, cfg: &Cfg, scale: f64) -> View {
    let reserve = cfg.mem_reserve_gb * KIB_PER_GIB;
    let mem_pool_kb = r
        .mem_available_bytes
        .map(|b| to_u64((to_f64(b / 1024) - reserve).max(0.0) * scale.min(cfg.mem_scale_max)));
    View {
        scale,
        mem_pool_kb,
        mem_committed_kb: running.iter().map(|j| j.est.rss_kb.saturating_sub(j.rss_now_kb.unwrap_or(0))).sum(),
        cpu_budget_millicores: to_u64(to_f64(r.cores.max(1) as u64) * cfg.cpu_factor * scale * 1000.0),
        cpu_used_millicores: running.iter().map(|j| j.est.millicores).sum(),
    }
}

/// `Ok` if `new` fits beside `running`, else the reason it does not.
pub fn fits(new: Estimate, running: &[Running], r: &Readings, cfg: &Cfg, scale: f64) -> Result<(), String> {
    if !cfg.enabled || running.is_empty() {
        return Ok(()); // never deadlock
    }
    let v = view(running, r, cfg, scale);
    let gb = |kb: u64| to_f64(kb) / KIB_PER_GIB;
    if let Some(pool) = v.mem_pool_kb {
        if new.rss_kb + v.mem_committed_kb > pool {
            return Err(format!(
                "memory budget: needs {:.1} GB + {:.1} GB committed > {:.1} GB",
                gb(new.rss_kb),
                gb(v.mem_committed_kb),
                gb(pool)
            ));
        }
    }
    if new.millicores + v.cpu_used_millicores > v.cpu_budget_millicores {
        return Err(format!(
            "cpu budget: needs {:.1} + {:.1} cores busy > {:.1}",
            to_f64(new.millicores) / 1000.0,
            to_f64(v.cpu_used_millicores) / 1000.0,
            to_f64(v.cpu_budget_millicores) / 1000.0
        ));
    }
    Ok(())
}

/// The adaptive scale on the budget, persisted in the queue dir.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aimd {
    pub scale: f64,
    /// Calm samples in a row since the last change.
    pub clean: u32,
    pub last_decrease_ms: u64,
}

impl Default for Aimd {
    fn default() -> Self {
        Self {
            scale: 1.0,
            clean: 0,
            last_decrease_ms: 0,
        }
    }
}

/// A spike halves the scale at most this often, so one slow-draining episode
/// sampled every poll does not slam it to the floor in seconds.
const DECREASE_EVERY_MS: u64 = 10_000;

impl Aimd {
    /// Feed one pressure sample.
    pub fn observe(&mut self, pressured: bool, now_ms: u64, cfg: &Cfg) {
        if pressured {
            self.clean = 0;
            if now_ms.saturating_sub(self.last_decrease_ms) >= DECREASE_EVERY_MS {
                self.scale = (self.scale * 0.5).max(cfg.aimd_min);
                self.last_decrease_ms = now_ms;
            }
        } else {
            self.clean += 1;
            if self.clean >= cfg.aimd_clean_samples.max(1) {
                self.scale = (self.scale + cfg.aimd_step).min(cfg.aimd_max);
                self.clean = 0;
            }
        }
        self.scale = self.scale.clamp(cfg.aimd_min, cfg.aimd_max);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    const GIB_KB: u64 = 1024 * 1024;

    fn s(peak_gb: u64, cpu_ms: u64, run_ms: u64) -> Sample {
        Sample {
            peak_group_rss_kb: peak_gb * GIB_KB,
            cpu_ms,
            run_ms,
        }
    }

    fn readings(avail_gb: u64, cores: usize) -> Readings {
        Readings {
            mem_available_bytes: Some(avail_gb * GIB_KB * 1024),
            cores,
            ..Readings::default()
        }
    }

    fn est(gb: u64, cores: u64) -> Estimate {
        Estimate {
            rss_kb: gb * GIB_KB,
            millicores: cores * 1000,
            from_runs: 5,
        }
    }

    #[test]
    fn no_history_uses_the_class_default() {
        let cfg = Cfg::default();
        let h = estimate(&[], Class::Heavy, &cfg, 12);
        assert_eq!((h.rss_kb, h.millicores, h.from_runs), (4 * GIB_KB, 4000, 0));
        let l = estimate(&[], Class::Light, &cfg, 12);
        assert_eq!((l.rss_kb, l.millicores), (GIB_KB / 2, 1000));
    }

    #[test]
    fn memory_is_the_p90_of_peak_group_rss() {
        let runs: Vec<Sample> = (1..=10).map(|g| s(g, 1000, 1000)).collect();
        assert_eq!(estimate(&runs, Class::Heavy, &Cfg::default(), 12).rss_kb, 9 * GIB_KB);
        assert_eq!(estimate(&[s(3, 1, 1)], Class::Heavy, &Cfg::default(), 12).rss_kb, 3 * GIB_KB);
    }

    #[test]
    fn cores_are_mean_cpu_over_wall_clamped_to_the_machine() {
        let e = estimate(&[s(1, 4000, 1000), s(1, 2000, 1000)], Class::Heavy, &Cfg::default(), 12);
        assert_eq!(e.millicores, 3000);
        let e = estimate(&[s(1, 100_000, 1000)], Class::Heavy, &Cfg::default(), 12);
        assert_eq!(e.millicores, 12_000, "clamped to the core count");
    }

    #[test]
    fn with_nothing_running_anything_fits() {
        let huge = est(1000, 1000);
        assert!(fits(huge, &[], &readings(1, 1), &Cfg::default(), 1.0).is_ok());
    }

    #[test]
    fn memory_counts_what_running_jobs_may_still_grow_into() {
        let cfg = Cfg::default(); // 6 GB reserve
        let r = readings(20, 64); // 14 GB pool
        let growing = Running {
            est: est(8, 1),
            rss_now_kb: Some(2 * GIB_KB), // may grow 6 GB more
        };
        assert!(fits(est(8, 1), &[growing], &r, &cfg, 1.0).is_ok(), "8 + 6 = 14 fits");
        let err = fits(est(9, 1), &[growing], &r, &cfg, 1.0).unwrap_err();
        assert!(err.starts_with("memory budget: needs 9.0 GB + 6.0 GB committed > 14.0 GB"), "{err}");
        let at_peak = Running {
            rss_now_kb: Some(8 * GIB_KB),
            ..growing
        };
        assert!(fits(est(14, 1), &[at_peak], &r, &cfg, 1.0).is_ok(), "a job at its peak takes nothing more");
    }

    #[test]
    fn cpu_budget_is_cores_times_factor_times_scale() {
        let cfg = Cfg::default(); // factor 1.5
        let r = readings(1000, 8); // 12 cores of budget
        let busy = Running {
            est: est(0, 8),
            rss_now_kb: None,
        };
        assert!(fits(est(0, 4), &[busy], &r, &cfg, 1.0).is_ok());
        assert!(fits(est(0, 5), &[busy], &r, &cfg, 1.0).unwrap_err().starts_with("cpu budget"));
        assert!(fits(est(0, 5), &[busy], &r, &cfg, 2.0).is_ok(), "a grown scale admits more");
        assert!(fits(est(0, 1), &[busy], &r, &cfg, 0.5).unwrap_err().starts_with("cpu budget"));
    }

    #[test]
    fn the_memory_side_never_overcommits_past_mem_scale_max() {
        let cfg = Cfg::default(); // mem_scale_max 1.0
        let r = readings(16, 64); // 10 GB pool
        let one = Running {
            est: est(1, 1),
            rss_now_kb: Some(GIB_KB),
        };
        assert!(fits(est(11, 1), &[one], &r, &cfg, 2.0).is_err(), "scale 2 must not double memory");
    }

    #[test]
    fn unknown_memory_skips_the_memory_check_only() {
        let r = Readings {
            cores: 4,
            ..Readings::default()
        };
        let one = Running {
            est: est(1, 1),
            rss_now_kb: None,
        };
        assert!(fits(est(500, 1), &[one], &r, &Cfg::default(), 1.0).is_ok());
        assert!(fits(est(1, 6), &[one], &r, &Cfg::default(), 1.0).is_err());
    }

    #[test]
    fn disabled_budget_admits_everything() {
        let cfg = Cfg {
            enabled: false,
            ..Cfg::default()
        };
        let one = Running {
            est: est(1, 1),
            rss_now_kb: None,
        };
        assert!(fits(est(10_000, 10_000), &[one], &readings(1, 1), &cfg, 1.0).is_ok());
    }

    #[test]
    fn aimd_grows_slowly_while_calm() {
        let cfg = Cfg::default(); // +0.1 per 5 calm samples, max 2.0
        let mut a = Aimd::default();
        for i in 0..4 {
            a.observe(false, i, &cfg);
        }
        assert!((a.scale - 1.0).abs() < f64::EPSILON, "grew before 5 samples");
        a.observe(false, 5, &cfg);
        assert!((a.scale - 1.1).abs() < 1e-9);
        for i in 0..1000 {
            a.observe(false, 10 + i, &cfg);
        }
        assert!((a.scale - 2.0).abs() < 1e-9, "capped at aimd_max");
    }

    #[test]
    fn aimd_halves_on_a_spike_at_most_every_ten_seconds() {
        let cfg = Cfg::default();
        let mut a = Aimd::default();
        a.observe(true, 100_000, &cfg);
        assert!((a.scale - 0.5).abs() < 1e-9);
        a.observe(true, 101_000, &cfg);
        assert!((a.scale - 0.5).abs() < 1e-9, "halved twice within 10s");
        a.observe(true, 110_000, &cfg);
        assert!((a.scale - 0.25).abs() < 1e-9);
        a.observe(true, 200_000, &cfg);
        assert!((a.scale - 0.25).abs() < 1e-9, "floored at aimd_min");
        // A spike resets the calm streak.
        for i in 0..4 {
            a.observe(false, 300_000 + i, &cfg);
        }
        a.observe(true, 310_000, &cfg);
        a.observe(false, 310_001, &cfg);
        assert!((a.scale - 0.25).abs() < 1e-9, "a spike did not reset the calm streak");
    }
}
