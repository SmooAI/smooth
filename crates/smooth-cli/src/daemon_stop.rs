//! `th down` — stop the daemon **and its whole process tree** (th-eed3de).
//!
//! `~/.smooth/smooth.pid` used to name the `th up --foreground` wrapper, whose
//! spawned `smooth-daemon` child outlived a plain `kill <pid>`: the port stayed
//! bound, `daemon.lock` stayed held, and `th down` had still printed "stopped".
//! The launcher now execs the daemon (so the pid *is* the daemon), and this
//! module covers the other half: whatever the pid file names, every descendant
//! is signalled too, and the result is verified instead of assumed.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use smooth_flow::proc::is_alive;

/// How long a TERM'd process gets before it is KILLed.
pub const GRACE: Duration = Duration::from_secs(5);

/// Direct children of `pid` (`pgrep -P`; empty where that is unavailable).
fn children(pid: u32) -> Vec<u32> {
    let Ok(out) = Command::new("pgrep").args(["-P", &pid.to_string()]).stderr(Stdio::null()).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect()
}

/// `pid` and every descendant, parents before children, collected **before**
/// any signal is sent — a TERM'd parent reaps or re-parents its children, and
/// then `pgrep -P` can no longer find them.
#[must_use]
pub fn tree(pid: u32) -> Vec<u32> {
    let mut out = vec![pid];
    let mut i = 0;
    while i < out.len() {
        for c in children(out[i]) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
        i += 1;
    }
    out
}

/// Alive and not a zombie. A killed process whose parent has not reaped it
/// still shows up in `ps` (so `is_alive` says yes) but is gone for every
/// purpose `th down` cares about — the port and `daemon.lock` are released.
fn alive(pid: u32) -> bool {
    if !is_alive(pid, None) {
        return false;
    }
    let stat = Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).stderr(Stdio::null()).output();
    !stat.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim_start().starts_with('Z'))
}

fn signal(sig: &str, pid: u32) {
    let _ = Command::new("kill")
        .args([sig, &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// TERM `pid` and its descendants, wait up to `grace`, KILL the survivors, and
/// return whatever is *still* alive afterwards (empty = success).
#[must_use]
pub fn stop_tree(pid: u32, grace: Duration) -> Vec<u32> {
    let pids = tree(pid);
    for p in &pids {
        signal("-TERM", *p);
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline && pids.iter().any(|p| alive(*p)) {
        std::thread::sleep(Duration::from_millis(100));
    }
    for p in pids.iter().filter(|p| alive(**p)) {
        signal("-KILL", *p);
    }
    // A KILLed process needs a moment to go before `ps` stops listing it.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && pids.iter().any(|p| alive(*p)) {
        std::thread::sleep(Duration::from_millis(50));
    }
    pids.into_iter().filter(|p| alive(*p)).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    /// Regression for th-eed3de: a wrapper whose child does the real work —
    /// stopping the wrapper must take the child down with it.
    #[test]
    #[cfg(unix)]
    fn stop_tree_kills_the_wrapper_and_its_child() {
        // `sh` stays the parent (it has a second command to run after `sleep`).
        let mut sh = Command::new("sh")
            .args(["-c", "sleep 60; sleep 60"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let wrapper = sh.id();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && tree(wrapper).len() < 2 {
            std::thread::sleep(Duration::from_millis(50));
        }
        let pids = tree(wrapper);
        assert_eq!(pids.len(), 2, "sh + its sleep child: {pids:?}");
        assert_eq!(pids[0], wrapper, "parents come first");
        let child = pids[1];
        assert!(alive(child));

        let survivors = stop_tree(wrapper, Duration::from_secs(3));
        assert!(survivors.is_empty(), "still alive: {survivors:?}");
        assert!(!alive(child), "the child (the daemon in real life) must not be orphaned");
        // The wrapper is our child: a zombie until reaped, which counts as dead.
        assert!(!alive(wrapper));
        let _ = sh.wait();
        assert!(!is_alive(wrapper, None));
    }

    #[test]
    fn tree_of_a_dead_pid_is_just_the_pid() {
        // No process has this pid for long; `pgrep -P` on it is empty.
        assert_eq!(tree(u32::MAX - 7), vec![u32::MAX - 7]);
    }

    #[test]
    fn stop_tree_of_a_dead_pid_reports_no_survivors() {
        assert!(stop_tree(u32::MAX - 7, Duration::from_millis(10)).is_empty());
    }
}
