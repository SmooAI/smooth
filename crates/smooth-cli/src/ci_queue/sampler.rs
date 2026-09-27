//! What a job actually used: the peak of its process group's TOTAL memory,
//! sampled, plus CPU time from `getrusage`.
//!
//! `ru_maxrss` alone is the largest single process. A cargo build is a dozen
//! parallel `rustc`s, so it undercounts exactly the jobs admission most needs
//! to size. The sampler sums RSS across the job's process group every tick and
//! keeps the peak; `ru_maxrss` is still recorded as `max_single_rss_kb`.
//!
//! The sampler is one `ps` per tick per running job, from a thread in the `th`
//! that holds the slot. It never holds the child, a slot or a lock (`ps` is
//! spawned by `std`, which opens every fd close-on-exec), and it stops the
//! moment it is told to, so it cannot keep a job — or `th` — alive.

use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

/// What one sample saw of a process group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GroupRss {
    pub total_kb: u64,
    pub largest_kb: u64,
}

/// Sum and max RSS of the processes in group `pgid`, from
/// `ps -A -o pgid= -o rss=` (RSS in KiB on macOS and Linux alike).
pub fn parse_ps(out: &str, pgid: u32) -> GroupRss {
    let mut g = GroupRss::default();
    for line in out.lines() {
        let mut f = line.split_whitespace();
        let (Some(pg), Some(rss)) = (f.next(), f.next()) else {
            continue;
        };
        if pg.parse::<u32>().ok() != Some(pgid) {
            continue;
        }
        if let Ok(kb) = rss.parse::<u64>() {
            g.total_kb += kb;
            g.largest_kb = g.largest_kb.max(kb);
        }
    }
    g
}

fn sample(pgid: u32) -> Option<GroupRss> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pgid=", "-o", "rss="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| parse_ps(&String::from_utf8_lossy(&out.stdout), pgid))
}

/// A running sampler. [`Sampler::stop`] ends it and returns the peaks.
pub struct Sampler {
    stop: Sender<()>,
    handle: JoinHandle<GroupRss>,
}

impl Sampler {
    /// Sample group `pgid` now and every `tick`, calling `on_sample` with each
    /// total so the caller can publish it (e.g. into the slot, for `status`).
    pub fn start(pgid: u32, tick: Duration, mut on_sample: impl FnMut(u64) + Send + 'static) -> Self {
        let (stop, rx) = mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            let mut peak = GroupRss::default();
            loop {
                if let Some(g) = sample(pgid) {
                    peak.total_kb = peak.total_kb.max(g.total_kb);
                    peak.largest_kb = peak.largest_kb.max(g.largest_kb);
                    on_sample(g.total_kb);
                }
                match rx.recv_timeout(tick) {
                    Err(RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            peak
        });
        Self { stop, handle }
    }

    /// Stop now (no waiting out the tick) and return the peaks seen.
    pub fn stop(self) -> GroupRss {
        let _ = self.stop.send(());
        self.handle.join().unwrap_or_default()
    }
}

/// CPU time and `ru_maxrss` of this process's reaped children so far. The
/// caller diffs before/after one job. `ru_maxrss` is bytes on macOS and KiB
/// elsewhere; this returns KiB.
#[cfg(unix)]
pub fn children_usage() -> Option<(u64, u64)> {
    use nix::sys::resource::{getrusage, UsageWho};
    let u = getrusage(UsageWho::RUSAGE_CHILDREN).ok()?;
    let ms = |t: nix::sys::time::TimeVal| u64::try_from(t.tv_sec()).unwrap_or(0) * 1000 + u64::try_from(t.tv_usec()).unwrap_or(0) / 1000;
    let cpu_ms = ms(u.user_time()) + ms(u.system_time());
    let raw = u64::try_from(u.max_rss()).unwrap_or(0);
    let maxrss_kb = if cfg!(target_os = "macos") { raw / 1024 } else { raw };
    Some((cpu_ms, maxrss_kb))
}

#[cfg(not(unix))]
pub fn children_usage() -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn parse_ps_sums_one_group_only() {
        let out = "    1  17840\n  464    480\n  538   7872\n  538   1000\n  5380   999999\n garbage\n";
        assert_eq!(
            parse_ps(out, 538),
            GroupRss {
                total_kb: 8872,
                largest_kb: 7872
            }
        );
        assert_eq!(parse_ps(out, 7), GroupRss::default());
    }

    #[test]
    #[cfg(unix)]
    fn samples_this_process_group() {
        // Our own group: the test runner is in it, so the total is non-zero.
        let pgid = u32::try_from(nix::unistd::getpgrp().as_raw()).unwrap();
        let g = sample(pgid).expect("ps runs");
        assert!(g.total_kb > 0 && g.largest_kb > 0 && g.largest_kb <= g.total_kb, "{g:?}");
    }

    /// The sampler must never be what keeps a job (or `th`) waiting: stop()
    /// returns at once, not at the next tick.
    #[test]
    #[cfg(unix)]
    fn stop_returns_without_waiting_out_the_tick() {
        let pgid = u32::try_from(nix::unistd::getpgrp().as_raw()).unwrap();
        let s = Sampler::start(pgid, Duration::from_secs(60), |_| {});
        std::thread::sleep(Duration::from_millis(200));
        let t = Instant::now();
        let peak = s.stop();
        assert!(t.elapsed() < Duration::from_secs(5), "stop waited {:?}", t.elapsed());
        assert!(peak.total_kb > 0);
    }

    #[test]
    #[cfg(unix)]
    fn children_usage_reads() {
        let before = children_usage().unwrap();
        let st = std::process::Command::new("sh")
            .args(["-c", "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done"])
            .status();
        assert!(st.unwrap().success());
        let after = children_usage().unwrap();
        assert!(after.0 >= before.0, "cpu went backwards: {before:?} → {after:?}");
        assert!(after.1 > 0);
    }
}
