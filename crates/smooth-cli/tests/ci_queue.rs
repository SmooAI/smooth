//! `th ci-queue` end to end, through the real binary and real kernel flocks.
//!
//! The in-process tests in `src/ci_queue/` cover FIFO order and the pressure
//! gate. These cover what only separate processes can: a holder that is
//! `kill -9`'d, a job that leaves an orphan behind, signals arriving at `th`,
//! and the exit codes a hook script sees.
//!
//! Every run gets its own queue dir and config, so nothing here touches the
//! machine's `~/.smooth/ci-queue`, and the gate is switched off so the real
//! machine's load cannot change an outcome.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Q {
    tmp: tempfile::TempDir,
}

impl Q {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("ci-queue.toml"),
            "[slots]\nheavy = 1\nlight = 2\n\
             [gate]\nmin_available_memory_pct = 0\nmax_memory_pressure_level = 0\nmax_swap_used_pct = 0\nmax_load_per_core = 0\nmin_free_disk_gb = 0\n\
             [run]\npoll_ms = 20\nkill_grace_secs = 1\nqos = \"normal\"\n",
        )
        .unwrap();
        // qos = normal: these cases are about admission, and at load 70+
        // Darwin background QoS can hold even `true` past a 1s timeout.
        Self { tmp }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn th(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_th"));
        c.arg("ci-queue")
            .args(args)
            .current_dir(self.tmp.path())
            .env("SMOOTH_CI_QUEUE_DIR", self.path("q"))
            .env("SMOOTH_CI_QUEUE_CONFIG", self.path("ci-queue.toml"))
            // This suite may itself be running inside a queued job.
            .env_remove("SMOOTH_CI_QUEUE_SLOT")
            .stdin(Stdio::null());
        c
    }

    fn spawn(&self, args: &[&str]) -> Child {
        self.th(args).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap()
    }

    fn output(&self, args: &[&str]) -> Output {
        self.th(args).output().unwrap()
    }

    /// Start a heavy job that holds the only heavy slot until killed, and
    /// return once it is really running. `$$` of the job lands in `pidfile`.
    fn hold(&self, pidfile: &Path) -> Child {
        let script = format!("echo $$ > '{}'; exec sleep 30", pidfile.display());
        let child = self.spawn(&["run", "--class", "heavy", "--label", "holder", "--", "sh", "-c", &script]);
        wait_for("the holder to start", || pidfile.exists() && !read(pidfile).is_empty());
        child
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default().trim().to_string()
}

fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let t = Instant::now();
    while !cond() {
        assert!(t.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_exit(child: &mut Child, within: Duration) -> Option<i32> {
    let t = Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return Some(s.code().unwrap_or(-1));
        }
        if t.elapsed() > within {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn kill(pid: &str, sig: &str) {
    let _ = Command::new("kill").args([sig, pid]).status();
}

#[test]
fn exit_codes_pass_through() {
    let q = Q::new();
    assert_eq!(q.output(&["run", "--", "sh", "-c", "exit 3"]).status.code(), Some(3));
    assert_eq!(q.output(&["run", "--class", "light", "--", "true"]).status.code(), Some(0));
    assert_eq!(q.output(&["run", "--", "sh", "-c", "kill -TERM $$"]).status.code(), Some(128 + 15));
}

#[test]
fn a_run_timeout_is_124_and_starts_only_at_admission() {
    let q = Q::new();
    let pid = q.path("holder.pid");
    let mut holder = q.hold(&pid);
    // Queued behind the holder for longer than its own timeout: the timeout
    // must not have been burning while it waited.
    let mut waiter = q.spawn(&["run", "--timeout", "1", "--max-wait", "30", "--", "true"]);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(waiter.try_wait().unwrap(), None, "the waiter ran while the slot was held");
    kill(&read(&pid), "-KILL");
    assert_eq!(
        wait_exit(&mut waiter, Duration::from_secs(10)),
        Some(0),
        "the queue wait counted against --timeout"
    );
    let _ = holder.wait();

    let began = Instant::now();
    let out = q.output(&["run", "--timeout", "1", "--", "sleep", "30"]);
    assert_eq!(out.status.code(), Some(124));
    assert!(began.elapsed() < Duration::from_secs(10));
    assert!(String::from_utf8_lossy(&out.stderr).contains("timed out after 1s"));
}

#[test]
fn the_wait_timeout_exits_75_and_runs_nothing() {
    let q = Q::new();
    let pid = q.path("holder.pid");
    let mut holder = q.hold(&pid);
    let marker = q.path("ran");
    let began = Instant::now();
    let out = q.output(&["run", "--max-wait", "1", "--", "touch", marker.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(75));
    assert!(began.elapsed() < Duration::from_secs(10));
    assert!(!marker.exists(), "a job that never got a slot ran anyway");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("waiting (0s, reason: 1 heavy busy: holder"), "{err}");
    assert!(err.contains("gave up after 1s"), "{err}");
    kill(&read(&pid), "-KILL");
    let _ = holder.wait();
}

/// `kill -9` gives `th` no chance to clean up. The kernel drops its flock
/// anyway, and that alone must free the slot.
#[test]
fn a_sigkilled_holder_frees_its_slot() {
    let q = Q::new();
    let pid = q.path("holder.pid");
    let mut holder = q.hold(&pid);
    let mut waiter = q.spawn(&["run", "--max-wait", "30", "--", "true"]);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(waiter.try_wait().unwrap(), None, "the waiter ran while the slot was held");

    holder.kill().unwrap(); // SIGKILL to `th`, not to the job
    let _ = holder.wait();
    assert_eq!(
        wait_exit(&mut waiter, Duration::from_secs(10)),
        Some(0),
        "the dead holder's slot was never freed"
    );
    // The job itself is an orphan now — `th` could not forward a SIGKILL.
    kill(&read(&pid), "-KILL");
}

/// Mutation-checked: handing the job the slot's fd (so it is inherited across
/// exec) makes this fail — the orphaned `sleep` keeps the flock, and the next
/// job waits until `--max-wait` gives up.
#[test]
fn an_orphaned_child_does_not_pin_its_slot() {
    let q = Q::new();
    let gc = q.path("grandchild.pid");
    let script = format!("sleep 30 & echo $! > '{}'; exit 0", gc.display());
    let out = q.output(&["run", "--label", "leaver", "--", "sh", "-c", &script]);
    assert_eq!(out.status.code(), Some(0));
    wait_for("the grandchild pid", || !read(&gc).is_empty());

    // A pinned slot would hold this until --max-wait and exit 75 (the orphan
    // sleeps 30s, far longer).
    let next = q.output(&["run", "--max-wait", "10", "--", "true"]);
    let err = String::from_utf8_lossy(&next.stderr).into_owned();
    kill(&read(&gc), "-KILL");
    assert_eq!(next.status.code(), Some(0), "the orphan pinned the slot: {err}");
    assert!(!err.contains("waiting ("), "admission waited on the orphan: {err}");
}

#[test]
fn term_and_int_are_forwarded_to_the_job() {
    for (sig, name) in [("-TERM", "TERM"), ("-INT", "INT")] {
        let q = Q::new();
        let ready = q.path("ready");
        let script = format!("trap 'exit 7' {name}; echo up > '{}'; while :; do sleep 0.05; done", ready.display());
        let mut th = q.spawn(&["run", "--class", "light", "--", "sh", "-c", &script]);
        wait_for("the job to start", || ready.exists());
        kill(&th.id().to_string(), sig);
        assert_eq!(wait_exit(&mut th, Duration::from_secs(10)), Some(7), "{name} did not reach the job's trap");
    }
}

#[test]
fn status_reports_the_running_job_as_json() {
    let q = Q::new();
    let pid = q.path("holder.pid");
    let mut holder = q.hold(&pid);
    let out = q.output(&["status", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let running = v["running"].as_array().unwrap();
    assert_eq!(running.len(), 1, "{v}");
    assert_eq!(running[0]["label"], "holder");
    assert_eq!(running[0]["child_pid"].to_string(), read(&pid));
    let human = q.output(&["status"]);
    assert!(String::from_utf8_lossy(&human.stdout).contains("heavy 1/1"));
    kill(&read(&pid), "-KILL");
    let _ = holder.wait();
}

/// Two jobs naming the same shared resource never run at once, even with
/// free slots in their class.
#[test]
fn jobs_naming_one_lock_never_overlap() {
    let q = Q::new();
    let pid = q.path("first.pid");
    let script = format!("echo $$ > '{}'; exec sleep 30", pid.display());
    let mut first = q.spawn(&["run", "--class", "light", "--lock", "docker", "--label", "first", "--", "sh", "-c", &script]);
    wait_for("the first job", || !read(&pid).is_empty());
    let out = q.output(&["run", "--class", "light", "--lock", "docker", "--max-wait", "1", "--", "true"]);
    assert_eq!(out.status.code(), Some(75), "a second holder of the lock ran");
    assert!(String::from_utf8_lossy(&out.stderr).contains("lock docker held by first"));
    assert_eq!(q.output(&["run", "--class", "light", "--max-wait", "1", "--", "true"]).status.code(), Some(0));
    kill(&read(&pid), "-KILL");
    let _ = first.wait();
    assert_eq!(
        q.output(&["run", "--class", "light", "--lock", "docker", "--max-wait", "5", "--", "true"])
            .status
            .code(),
        Some(0)
    );
}
