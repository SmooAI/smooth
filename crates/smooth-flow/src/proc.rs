//! Process liveness — `pid` + start time, so a recycled pid can't pass for
//! the agent that died (supervision rules 2 and 4).

use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::{Local, NaiveDateTime, TimeZone};

/// Parse `ps -o lstart=` output (`Mon Sep  7 21:57:00 2026`) into local
/// epoch seconds. Pure, so the format is pinned by test.
#[must_use]
pub fn parse_lstart(s: &str) -> Option<i64> {
    let compact: Vec<&str> = s.split_whitespace().collect();
    if compact.len() != 5 {
        return None;
    }
    // "%a %b %e %H:%M:%S %Y" — rebuild with single spaces so `%e` parses.
    let normalized = compact.join(" ");
    let naive = NaiveDateTime::parse_from_str(&normalized, "%a %b %d %H:%M:%S %Y").ok()?;
    Local.from_local_datetime(&naive).earliest().map(|d| d.timestamp())
}

/// Parse `ps -o stat=,lstart=` output: `None` for a zombie (`Z…` — it has
/// exited; only its parent's reaping is outstanding), else the start time.
#[must_use]
pub fn parse_stat_lstart(s: &str) -> Option<i64> {
    let (stat, lstart) = s.trim_start().split_once(char::is_whitespace)?;
    if stat.starts_with('Z') {
        return None;
    }
    parse_lstart(lstart)
}

/// The start time (local epoch seconds) of `pid`, or `None` when it is gone.
///
/// A zombie counts as gone in every way that matters here: a kill that left
/// one succeeded, and it owns no harness session (th-b00115 found the
/// conformance suite waiting on `<defunct>` fakes tmux had yet to reap).
#[cfg(unix)]
#[must_use]
pub fn start_time(pid: u32) -> Option<i64> {
    let out = Command::new("ps").args(["-o", "stat=,lstart=", "-p", &pid.to_string()]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_stat_lstart(&String::from_utf8_lossy(&out.stdout))
}

/// True when `pid` is alive AND started when we recorded it did. A `None`
/// recorded start (never captured) degrades to a bare pid check.
#[must_use]
pub fn is_alive(pid: u32, recorded_start: Option<i64>) -> bool {
    match (start_time(pid), recorded_start) {
        (None, _) => false,
        (Some(_), None) => true,
        // ps rounds to the second; allow a one-second skew either way.
        (Some(now), Some(then)) => (now - then).abs() <= 1,
    }
}

/// SIGTERM the process group of `pid`, wait up to `grace`, then SIGKILL
/// whatever is left. Best effort — never errors.
#[cfg(unix)]
pub fn kill_tree(pid: u32, grace: Duration) {
    let group = format!("-{pid}");
    let _ = Command::new("kill")
        .args(["-TERM", "--", &group])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        if start_time(pid).is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = Command::new("kill")
        .args(["-KILL", "--", &group])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The start time (epoch seconds) of `pid`, or `None` when it is gone.
///
/// Windows has no `ps` (th-64d4ab): shelling out to it failed, so every
/// agent read as dead and supervision resumed the whole fleet. `sysinfo`
/// reads the process table directly; its start time is epoch seconds, the
/// same unit the Unix path records.
#[cfg(windows)]
#[must_use]
pub fn start_time(pid: u32) -> Option<i64> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing());
    sys.process(pid).and_then(|p| i64::try_from(p.start_time()).ok())
}

/// End `pid` and its descendants: `taskkill /T` asks, then after `grace`
/// `taskkill /T /F` forces whatever is left. Best effort — never errors.
#[cfg(windows)]
pub fn kill_tree(pid: u32, grace: Duration) {
    let taskkill = |force: bool| {
        let mut c = Command::new("taskkill");
        c.args(["/T", "/PID", &pid.to_string()]);
        if force {
            c.arg("/F");
        }
        let _ = c.stdout(Stdio::null()).stderr(Stdio::null()).status();
    };
    taskkill(false);
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        if start_time(pid).is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    taskkill(true);
}

/// Neither Unix nor Windows: nothing to ask, so nothing is alive.
#[cfg(not(any(unix, windows)))]
#[must_use]
pub fn start_time(_pid: u32) -> Option<i64> {
    None
}

/// See [`start_time`]'s fallback.
#[cfg(not(any(unix, windows)))]
pub fn kill_tree(_pid: u32, _grace: Duration) {}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn parses_bsd_lstart() {
        let ts = parse_lstart("Mon Sep  7 21:57:00 2026\n").unwrap();
        let expected = Local.with_ymd_and_hms(2026, 9, 7, 21, 57, 0).unwrap().timestamp();
        assert_eq!(ts, expected);
        assert!(parse_lstart("").is_none());
        assert!(parse_lstart("garbage in here now ok").is_none());
    }

    #[test]
    fn a_zombie_has_no_start_time() {
        let expected = Local.with_ymd_and_hms(2026, 9, 7, 21, 57, 0).unwrap().timestamp();
        assert_eq!(parse_stat_lstart("Ss   Mon Sep  7 21:57:00 2026\n"), Some(expected));
        assert_eq!(parse_stat_lstart("S+ Mon Sep  7 21:57:00 2026"), Some(expected));
        assert_eq!(parse_stat_lstart("Zs   Mon Sep  7 21:57:00 2026"), None, "exited, unreaped");
        assert_eq!(parse_stat_lstart("Z+ Mon Sep  7 21:57:00 2026"), None);
        assert_eq!(parse_stat_lstart(""), None);
        assert_eq!(parse_stat_lstart("Ss"), None);
    }

    #[test]
    #[cfg(unix)]
    fn an_unreaped_child_is_not_alive() {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        // Not waited on: once `true` exits it is a zombie of this process.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while start_time(pid).is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!is_alive(pid, None), "a zombie is not a live process");
        let _ = child.wait();
    }

    #[test]
    #[cfg(unix)]
    fn own_process_is_alive_and_dead_pid_is_not() {
        let me = std::process::id();
        let start = start_time(me).unwrap();
        assert!(is_alive(me, Some(start)));
        assert!(is_alive(me, None));
        assert!(!is_alive(me, Some(start - 100_000)), "a different start time is a different process");
        // A pid nobody has: max pid space on macOS/Linux is far below this.
        assert!(!is_alive(4_000_000, None));
    }

    #[test]
    #[cfg(unix)]
    fn kill_tree_reaps_a_sleeper() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        assert!(start_time(pid).is_some());
        kill_tree(pid, Duration::from_secs(2));
        // Reap the zombie so ps stops seeing it, then assert it is gone.
        let mut child = child;
        let _ = child.wait();
        assert!(start_time(pid).is_none() || !is_alive(pid, None));
    }

    /// th-64d4ab: on Windows the old `ps` path read every process as dead.
    #[test]
    fn this_process_is_alive_and_a_bogus_pid_is_not() {
        let me = std::process::id();
        let started = start_time(me).expect("our own start time");
        assert!(is_alive(me, Some(started)));
        assert!(is_alive(me, None));
        assert!(!is_alive(me, Some(started - 3600)), "a recycled pid (different start) is not the same process");
        assert!(start_time(u32::MAX - 7).is_none());
    }
}
