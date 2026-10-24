//! `th attest` — run a repo's CI checks locally (or on a build box) and credit
//! them on GitHub, so the workflow can skip what already ran.
//!
//! Ported from smooai's `scripts/ci/attest.sh`, which stays the reference for the
//! behaviour. **This module knows nothing about any particular repo's checks** —
//! the definitions live in that repo as `scripts/ci/<name>.sh`, and the file name
//! is the whole interface.
//!
//! # Three outcomes, not two
//!
//! | Local outcome     | Exit   | Posted      | CI does       |
//! | ----------------- | ------ | ----------- | ------------- |
//! | passed            | 0      | `success`   | skips the row |
//! | failed            | 1..    | `failure`   | runs the row  |
//! | **could not run** | **97** | **nothing** | runs the row  |
//!
//! The third row is the point. A status is a claim about the *commit*; "your
//! Docker daemon is off" is a claim about the *laptop*, and posting it as
//! `failure` red-flags the PR over a fact about someone's machine — which is
//! exactly what happened twice on a healthy tree, sending a 37-minute job to CI
//! anyway (th-514dc8). **A precondition you can repair, repair. One you cannot,
//! report as 97 — never as a verdict.**
//!
//! # Why this pushes for you
//!
//! Pushing starts the workflow, and every row reads the statuses **once**, ~20s
//! into the job. Push first and a five-minute local run loses that race every
//! time. So the order is **run the checks → push → credit**, and crediting is one
//! API call that lands seconds after the push. Run `th attest` INSTEAD of
//! `git push`, not after it.

mod env;
mod gh;
mod remote;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Args;

use crate::ci_queue;
use env::{Sys, EXIT_PRECONDITION};
use gh::Gh;

/// Exit code for a usage error — an unknown check, or no check named at all.
const EXIT_USAGE: i32 = 2;

/// Exit code when the working tree is dirty before the run, or a check dirtied
/// a tracked file during it (codegen drift). Distinct from a check FAILURE:
/// nothing was credited because what was tested would not match the credited
/// commit, not because the code is wrong.
const EXIT_DIRTY: i32 = 3;

#[derive(Args, Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "these are CLI flags, not state")]
pub struct AttestArgs {
    /// Checks to run. Each is a `scripts/ci/<name>.sh` in the repo root.
    #[arg(value_name = "CHECK")]
    pub checks: Vec<String>,

    /// Run every check the repo defines.
    #[arg(long)]
    pub all: bool,

    /// Show what is credited for HEAD right now and exit.
    #[arg(long)]
    pub status: bool,

    /// Never push the branch. Fails if HEAD is not already on a remote, since a
    /// status can only attach to a commit the remote has. (Remote delegation
    /// still publishes the internal `refs/attest/<sha>` transport ref, which
    /// triggers no workflows and is deleted afterwards.)
    #[arg(long)]
    pub no_push: bool,

    /// Run the named checks on this host instead of locally. Needs a
    /// `[remote] worktree` in `.smooth/attest.toml`.
    #[arg(long, value_name = "HOST")]
    pub remote: Option<String>,

    /// Run everything locally, ignoring any remote routing in the config.
    #[arg(long)]
    pub local: bool,

    /// Attest even though the working tree has uncommitted changes. Off by
    /// default, and deliberately: attest credits the HEAD commit, but the checks
    /// run against your working tree. A dirty tree means those differ, so a green
    /// here is a green a PR would not reproduce — and a check that regenerates a
    /// tracked file (codegen drift) would slip through as a pass. Pass this only
    /// when you know the dirt cannot change any check's outcome.
    #[arg(long)]
    pub allow_dirty: bool,

    /// Machine-readable summary on stdout.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    Pass,
    Fail,
    Blocked,
}

impl Outcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Blocked => "blocked",
        }
    }

    /// What gets posted. `None` is the whole third outcome: a check that verified
    /// nothing may not claim anything about the commit.
    const fn state(self) -> Option<&'static str> {
        match self {
            Self::Pass => Some("success"),
            Self::Fail => Some("failure"),
            Self::Blocked => None,
        }
    }
}

#[derive(Debug)]
struct CheckResult {
    name: String,
    outcome: Outcome,
    secs: u64,
    /// Where it ran, phrased for a status description: `locally` or a hostname.
    location: String,
    note: Option<String>,
}

impl CheckResult {
    fn description(&self) -> String {
        let place = if self.location == "locally" {
            "locally".to_string()
        } else {
            format!("on {}", self.location)
        };
        match self.outcome {
            Outcome::Pass => format!("passed {place} in {}s", self.secs),
            _ => format!("failed {place} after {}s", self.secs),
        }
    }
}

pub fn cmd(args: &AttestArgs) -> Result<()> {
    let sys = Sys::default();
    let root = repo_root()?;
    let code = run(args, &sys, &root)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn run(args: &AttestArgs, sys: &Sys, root: &Path) -> Result<i32> {
    let available = discover(root);
    let sha = git(root, &["rev-parse", "HEAD"])?;
    let gh = Gh::detect(sys)?;

    if args.status {
        return Ok(show_status(&gh, &sha, &available, args.json));
    }

    let Some(requested) = select(args, &available) else {
        return Ok(EXIT_USAGE);
    };

    // Attestation only means anything for a commit a PR will run checks on. A
    // status on the default branch credits (or worse, red-flags) shared history
    // no pr-checks run will ever consult — learned the hard way when a fresh
    // worktree, whose HEAD was still main's tip, flipped main's combined status
    // to `failure` over a missing node_modules.
    let default_branch = gh.default_branch();
    if git_ok(root, &["merge-base", "--is-ancestor", &sha, &format!("origin/{default_branch}")]) {
        eprintln!("✗ {sha} is already on origin/{default_branch} — nothing to attest.");
        eprintln!("  Attestation is for commits a PR will check. Commit to a branch first.");
        return Ok(1);
    }

    // A status can only attach to a commit the remote already has. Bash discovers
    // this after the run; checking up front costs nothing and saves the minutes.
    let pre_pushed = on_remote(root, &sha);
    if args.no_push && !pre_pushed {
        eprintln!("✗ {sha} is not on any remote branch, and --no-push was given.");
        return Ok(1);
    }

    // Attest credits the COMMIT, but the checks run against the working TREE. If
    // they differ, a pass here is a pass a PR (which sees only the commit) would
    // not reproduce — and a silent one, which is how a dirty tree once produced a
    // run that "passed everything and credited nothing" with no stated reason.
    // Refuse up front and name the files, unless the caller opts in.
    // Tracked changes only (`-uno`): a modified committed file is the integrity
    // risk — uncommitted work, or regenerated output that drifted from source.
    // Untracked scratch files are the caller's business and don't change what a
    // committed check runs against.
    let dirty_before = git(root, &["status", "--porcelain", "--untracked-files=no"])?;
    if !dirty_before.is_empty() && !args.allow_dirty {
        eprintln!("✗ working tree has uncommitted changes — attest credits the commit {sha}, not your working tree.");
        eprintln!("  The checks would run against files a PR never sees, so any credit could be a green that CI would not reproduce.");
        for line in dirty_before.lines().take(20) {
            eprintln!("    {line}");
        }
        eprintln!("  → commit or stash, then re-run — or pass --allow-dirty to credit HEAD as-is.");
        return Ok(EXIT_DIRTY);
    }

    let (local_checks, remote_plan) = route(args, root, &requested)?;

    env::apply_path(sys);

    let (results, published_attest_ref) = execute_all(sys, root, &sha, &local_checks, remote_plan.as_ref())?;

    for r in &results {
        report(r);
    }

    // A check that dirtied a tracked file regenerated committed output — the
    // classic codegen drift (a Zod source edited without re-running generation).
    // HEAD still holds the stale output, so `sha` would fail that very check in
    // CI: crediting it green would be a lie. Only reachable when the tree started
    // clean (a dirty start already returned above unless --allow-dirty).
    if !args.allow_dirty {
        let dirty_after = git(root, &["status", "--porcelain", "--untracked-files=no"])?;
        if !dirty_after.is_empty() {
            eprintln!("\n✗ a check modified tracked files — generated output is not committed, so {sha} would fail in CI. Nothing credited.");
            for line in dirty_after.lines().take(20) {
                eprintln!("    {line}");
            }
            eprintln!("  → regenerate and commit the drift the check exposed, then re-attest.");
            return Ok(EXIT_DIRTY);
        }
    }

    if !pre_pushed && !args.no_push {
        let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        println!("\n▸ push {branch} (so the checks can be credited)");
        git(root, &["push", "--set-upstream", "origin", &branch]).context("push failed — nothing was credited")?;
    }

    // Credit in one burst, immediately after the push. Real failures are posted
    // too: a red `ci-attest/<check>` is useful signal, and the workflow only ever
    // skips on success, so it can never be mistaken for a pass.
    for r in &results {
        if let Some(state) = r.outcome.state() {
            gh.post_status(&sha, &r.name, state, &r.description())?;
        }
    }

    if published_attest_ref {
        // Best effort: a leftover ref costs nothing but tidiness.
        let _ = git_ok(root, &["push", "origin", &format!(":{}", remote::attest_ref(&sha))]);
    }

    if pre_pushed {
        warn_if_run_in_flight(&gh, &sha);
    }

    summarise(&results, args.json, &sha);
    Ok(exit_code(&results))
}

/// Which checks the invocation asked for, or `None` after printing why it was a
/// usage error. Both refusals happen BEFORE anything runs — a typo must not
/// surface five minutes into a run.
fn select(args: &AttestArgs, available: &[String]) -> Option<Vec<String>> {
    let requested: Vec<String> = if args.all { available.to_vec() } else { args.checks.clone() };

    if requested.is_empty() {
        eprintln!("usage: th attest [--no-push] [--remote HOST] <check…> | --all | --status");
        if available.is_empty() {
            eprintln!("this repo defines no checks — add scripts/ci/<name>.sh");
        } else {
            eprintln!("checks in this repo: {}", available.join(" "));
        }
        return None;
    }

    for c in &requested {
        if !available.contains(c) {
            eprintln!("✗ no such check: {c}");
            eprintln!("  checks in this repo: {}", available.join(" "));
            return None;
        }
    }
    Some(requested)
}

/// Run everything and collect verdicts. Nothing is published from in here — the
/// caller pushes first and credits after, which is what removes the race with a
/// workflow run that reads the statuses once, ~20s in.
///
/// Returns the results and whether `refs/attest/<sha>` was published (so the
/// caller knows whether there is anything to clean up).
fn execute_all(
    sys: &Sys,
    root: &Path,
    sha: &str,
    local_checks: &[String],
    remote_plan: Option<&(remote::Remote, Vec<String>)>,
) -> Result<(Vec<CheckResult>, bool)> {
    // Only the machine that will actually run a check needs its preconditions.
    let blocked_all = if local_checks.is_empty() {
        None
    } else {
        env::ensure_docker(sys).err().or_else(|| env::ensure_node_modules(sys, root).err())
    };
    if let Some(reason) = &blocked_all {
        eprintln!("⊘ {reason}");
    }

    // Remote checks run CONCURRENTLY with the local ones — the whole reason to
    // have a build box is that its 38-minute row overlaps your 30-second lint.
    let mut published_attest_ref = false;
    let mut remote_handles = Vec::new();
    if let Some((cfg, checks)) = remote_plan.filter(|(_, c)| !c.is_empty()) {
        let origin = git(root, &["remote", "get-url", "origin"]).unwrap_or_else(|_| "origin".into());
        if !git_ok(root, &["push", "--force", "origin", &format!("HEAD:{}", remote::attest_ref(sha))]) {
            bail!("could not publish {} for remote execution", remote::attest_ref(sha));
        }
        published_attest_ref = true;
        for check in checks {
            // `root` too: an unreachable box falls back to a local run of the check.
            let (sys, cfg, check, origin, sha, root) = (sys.clone(), cfg.clone(), check.clone(), origin.clone(), sha.to_string(), root.to_path_buf());
            remote_handles.push(std::thread::spawn(move || run_remote(&sys, &root, &cfg, &check, &origin, &sha)));
        }
    }

    let mut results: Vec<CheckResult> = Vec::new();
    for check in local_checks {
        println!("\n▸ attest: {check}");
        results.push(blocked_all.as_ref().map_or_else(
            || run_local(sys, root, check),
            |reason| CheckResult {
                name: check.clone(),
                outcome: Outcome::Blocked,
                secs: 0,
                location: "locally".into(),
                note: Some(reason.clone()),
            },
        ));
    }

    for h in remote_handles {
        match h.join() {
            Ok(r) => results.push(r),
            Err(_) => bail!("a remote check thread panicked"),
        }
    }
    Ok((results, published_attest_ref))
}

/// `scripts/ci/<name>.sh` in the repo root **is** a check called `<name>`. Files
/// starting with `_` are shared helpers and `*.test.sh` are the checks' own
/// tests, so neither is a check.
fn discover(root: &Path) -> Vec<String> {
    let dir = root.join("scripts/ci");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let base = name.strip_suffix(".sh")?;
            if base.starts_with('_') || Path::new(base).extension().is_some_and(|x| x == "test") {
                return None;
            }
            // ponytail: the bash runner is itself `scripts/ci/attest.sh`, and
            // `--all` running it would recurse. Excluding it matches bash's own
            // `--all` filter.
            if base == "attest" {
                return None;
            }
            e.path().is_file().then(|| base.to_string())
        })
        .collect();
    names.sort();
    names
}

type Plan = (Vec<String>, Option<(remote::Remote, Vec<String>)>);

/// Split the requested checks into what runs here and what is delegated.
fn route(args: &AttestArgs, root: &Path, requested: &[String]) -> Result<Plan> {
    if args.local {
        return Ok((requested.to_vec(), None));
    }

    let host_override = args
        .remote
        .clone()
        .or_else(|| std::env::var("SMOOTH_ATTEST_REMOTE").ok().filter(|h| !h.is_empty()));

    let Some(mut cfg) = remote::load(root)? else {
        if let Some(host) = host_override {
            bail!(
                "--remote {host} needs a worktree on that host — add [remote] to {}",
                remote::config_path(root).display()
            );
        }
        return Ok((requested.to_vec(), None));
    };

    // An explicit host forces EVERY requested check remote; without one, the
    // config's own routing decides.
    let remote_checks = if let Some(host) = host_override {
        cfg.host = host;
        requested.to_vec()
    } else {
        requested.iter().filter(|c| cfg.checks.contains(c)).cloned().collect()
    };
    let local_checks = requested.iter().filter(|c| !remote_checks.contains(c)).cloned().collect();
    Ok((local_checks, Some((cfg, remote_checks))))
}

/// SMOODEV-3883: a failure distrusted for load used to go straight to CI, and a
/// block that said only "load 43 on 12 cores" sent agents to `ci:full` — every
/// row on paid runners. Now the block says what is causing it, and the check gets
/// one re-run once the load falls (bounded by `overload_retry_wait`), which is a
/// real verdict: a transient spike no longer costs a CI row.
fn run_local(sys: &Sys, root: &Path, check: &str) -> CheckResult {
    let (first, distrusted) = run_local_once(sys, root, check);
    if !distrusted {
        return first;
    }
    eprintln!("⊘ {check} failed at {} — not a verdict. Why the machine is busy:", env::load_summary(sys));
    diagnose(root);
    if sys.overload_retry_wait.is_zero() || !env::wait_until_quiet(sys) {
        return first;
    }
    eprintln!("· load is down to {} — re-running {check} once.", env::load_summary(sys));
    let (second, _) = run_local_once(sys, root, check);
    CheckResult {
        secs: first.secs + second.secs,
        ..second
    }
}

/// Print why this machine is too busy to attest. A repo supplies the diagnosis as
/// `scripts/ci/_pressure.sh` (smooai's names its long-running local stack, disk
/// against the queue's hold and the top processes, and ends with a remedy); a repo
/// without one just gets pointed at the queue. Bounded, and never fatal: the
/// diagnosis must not become one more reason a check could not run.
fn diagnose(root: &Path) {
    let script = root.join("scripts/ci/_pressure.sh");
    if !script.is_file() {
        eprintln!("  `th ci-queue status` shows what is holding the machine. Free it, then re-run only this check.");
        return;
    }
    let Ok(mut child) = Command::new("bash")
        .arg(&script)
        .current_dir(root)
        .env("CI_REPO_ROOT", root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Ok(Some(_)) | Err(_) => return,
        }
    }
    drop(child.kill());
    drop(child.wait());
}

/// The ci-queue class a check's row runs in. Heavy unless the script opts into
/// the light pool with a whole-line comment `# ci-queue: light` (anywhere in the
/// file; matched as `^\s*#\s*ci-queue:\s*light\s*$`). Only a comment line
/// counts: `echo "# ci-queue: light"` is a command, not a marker, so it stays
/// Heavy. A script that takes the cargo lock is a cargo build and is never
/// light, marker or not (SMOODEV-4018).
fn row_class(script_text: &str, takes_cargo_lock: bool) -> ci_queue::Class {
    let marked = script_text.lines().any(|line| {
        line.trim()
            .strip_prefix('#')
            .and_then(|rest| rest.trim_start().strip_prefix("ci-queue:"))
            .is_some_and(|value| value.trim() == "light")
    });
    if marked && !takes_cargo_lock {
        ci_queue::Class::Light
    } else {
        ci_queue::Class::Heavy
    }
}

/// Ask the repo whether `check` has anything to do, BEFORE it queues for a slot.
/// `scripts/ci/_<check>-untouched.mjs <root>` is the repo's own path filter: exit
/// 0 = nothing this change can affect (stdout says why), anything else, a missing
/// script or a hang = run the check. A web-only branch used to wait 45-50 minutes
/// for a heavy slot and the cargo lock to learn that `rust.sh` skips itself.
/// Bounded at 20s and never fatal: the filter must not become a reason not to run.
fn preflight_skip(sys: &Sys, root: &Path, check: &str) -> Option<String> {
    let filter = root.join(format!("scripts/ci/_{check}-untouched.mjs"));
    if !filter.is_file() {
        return None;
    }
    let mut child = Command::new(&sys.node)
        .arg(&filter)
        .arg(root)
        .current_dir(root)
        .env("CI_REPO_ROOT", root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut s);
        s
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                drop(child.kill());
                drop(child.wait());
                return None;
            }
        }
    };
    let reason = reader.join().unwrap_or_default();
    status.success().then(|| reason.trim().to_string())
}

/// One queued run of `check`, and whether its failure was distrusted for load.
fn run_local_once(sys: &Sys, root: &Path, check: &str) -> (CheckResult, bool) {
    if let Some(reason) = preflight_skip(sys, root, check) {
        // Credited like the script's own early `exit 0`, but without a slot, the
        // cargo lock or a history row.
        eprintln!("· {check}: skipped before queueing — {reason}");
        return (
            CheckResult {
                name: check.to_string(),
                outcome: Outcome::Pass,
                secs: 0,
                location: "locally".into(),
                note: Some(format!("skipped before queueing: {reason}")),
            },
            false,
        );
    }
    // SMOODEV-3355: every local check takes a slot in the machine-wide
    // `th ci-queue` first, so thirty agents attesting at once run two at a time
    // instead of thirty. Inside an already-queued job this admits instantly.
    // The slot is heavy unless the script opts into the light pool
    // (`row_class`, SMOODEV-4018).
    let blocked = |secs: u64, note: String| CheckResult {
        name: check.to_string(),
        outcome: Outcome::Blocked,
        secs,
        location: "locally".into(),
        note: Some(note),
    };
    let script = root.join(format!("scripts/ci/{check}.sh"));
    // A check that drives cargo holds the shared target's lock, so it takes
    // the queue's `cargo` lock — and must not run at background QoS, where it
    // would stall every other Rust build behind that lock (priority inversion,
    // SMOODEV-3355). The script text is the only signal: attest knows nothing
    // about a repo's checks.
    let text = std::fs::read_to_string(&script).unwrap_or_default();
    let locks = if text.contains("cargo") {
        vec![sys.queue.resolve_lock("cargo", root)]
    } else {
        Vec::new()
    };
    let class = row_class(&text, !locks.is_empty());
    // iOS checks boot hundreds of simulator processes: one at a time.
    let locks = ci_queue::with_ios_lock(locks, &format!("attest {check}"), &sys.queue.config);
    let qos = sys.queue.config.run.qos.effective(!locks.is_empty());
    let request = ci_queue::Request {
        class,
        label: format!("attest {check}"),
        cwd: root.to_path_buf(),
        max_wait: Duration::from_secs(sys.queue.config.run.max_wait_secs),
        locks,
        cmd_hash: None,
    };
    let mut admission = match sys.queue.admit(&request) {
        Ok(a) => a,
        // Waiting out a busy machine is not a verdict on the commit: nothing
        // is posted and CI runs the row.
        Err(ci_queue::AdmitError::WaitTimeout { waited, reason }) => {
            eprintln!("⊘ {check} waited {}s for a {} ci-queue slot ({reason}). Why:", waited.as_secs(), class.name());
            diagnose(root);
            return (
                blocked(
                    waited.as_secs(),
                    format!("no {} ci-queue slot freed in {}s ({reason})", class.name(), waited.as_secs()),
                ),
                false,
            );
        }
        Err(ci_queue::AdmitError::Queue(e)) => return (blocked(0, format!("the ci-queue is unusable ({e:#})")), false),
    };

    // th-b27ed0: sampled BEFORE the check, not after. "After" was right for a
    // laptop, where the load is eighteen other agents and the check merely ran
    // through it — but it is exactly backwards on a machine dedicated to
    // attesting, where the check IS the load: cargo on 10 cores takes smoo-hub to
    // 62, so every genuine failure would be swallowed as "machine too busy".
    // Ambient load at the start is what the check will actually contend with, and
    // it reads correctly on both machines. (Start = admission: the queue wait is
    // not what the check contends with.)
    let was_overloaded = env::overloaded(sys);
    let began = Instant::now();
    let argv = [OsString::from("bash"), script.into_os_string()];
    let progress = admission.progress();
    let run_cfg = &sys.queue.config.run;
    let cap = run_cfg.max_runtime(&format!("attest {check}"), class == ci_queue::Class::Heavy);
    let window = run_cfg.stall_window(&format!("attest {check}"));
    let watch = (!window.is_zero() || cap.is_some()).then(|| ci_queue::exec::Watch {
        window,
        others: admission.other_roots(),
        max_runtime: cap,
        threshold_pct: run_cfg.stall_cpu_pct,
        every: Duration::from_secs(run_cfg.watch_every_secs.max(1)),
        ticket: admission.ticket(),
        on_idle: admission.idle_publisher(),
    });
    let (ended, usage) = ci_queue::exec::run_measured(
        &ci_queue::exec::Spec {
            argv: &argv,
            cwd: root,
            env: vec![
                ("CI_REPO_ROOT".into(), root.as_os_str().to_owned()),
                (ci_queue::queue::NESTED_ENV.into(), admission.slot_name().into()),
                (ci_queue::watch::TICKET_ENV.into(), admission.ticket().to_string().into()),
            ],
            timeout: None,
            qos,
            kill_grace: Duration::from_secs(sys.queue.config.run.kill_grace_secs),
            null_stdin: true,
            sample_every: Some(Duration::from_millis(sys.queue.config.budget.sample_ms)),
        },
        |pid| admission.set_child(pid),
        progress,
        watch,
    );
    let ran = began.elapsed();
    admission.finish(ended, ran, usage);
    let secs = ran.as_secs();
    let code = match ended {
        ci_queue::exec::Ended::Exited(c) => Some(c),
        _ => None,
    };

    let stalled = ended == ci_queue::exec::Ended::Stalled;
    let (outcome, note) = if stalled {
        // The watchdog killed an idle job: infrastructure, never a verdict on the commit.
        (
            Outcome::Blocked,
            Some("stalled: its process tree sat idle until the queue's stall watchdog killed it — rerun".into()),
        )
    } else {
        classify(code, was_overloaded, || env::load_summary(sys))
    };
    let distrusted = was_overloaded && !stalled && outcome == Outcome::Blocked && code != Some(EXIT_PRECONDITION);
    (
        CheckResult {
            name: check.to_string(),
            outcome,
            secs,
            location: "locally".into(),
            note,
        },
        distrusted,
    )
}

fn run_remote(sys: &Sys, root: &Path, cfg: &remote::Remote, check: &str, origin: &str, sha: &str) -> CheckResult {
    let began = Instant::now();
    match remote::execute(sys, cfg, check, origin, sha) {
        Ok(code) => {
            // The delegating machine's load says nothing about the box that ran the
            // work, so overload distrust does not apply here.
            let (outcome, note) = classify(Some(code), false, String::new);
            CheckResult {
                name: check.to_string(),
                outcome,
                secs: began.elapsed().as_secs(),
                location: cfg.host.clone(),
                note,
            }
        }
        // The box is unreachable or unusable — infrastructure, never a verdict on the
        // commit. Blocking here hands the whole (often 38-minute) row back to CI —
        // the slow path this build box exists to avoid. Run the check HERE instead,
        // but LOUDLY on stderr: a local run has no warm cache and is far slower than
        // the box, and the developer should know why their attest just got long.
        // `ConnectTimeout` (remote.rs) makes an unreachable host surface in seconds,
        // so this is a fast fall-through, not a long stall. Result reads as local.
        //
        // th-279151: EXCEPT on an oversubscribed machine. The fallback was written
        // for a quiet laptop; on one at 2x its cores (30 agents is normal here) a
        // cold build of the row the box exists for runs for most of an hour, and
        // `classify` would then distrust any failure anyway. Measured: 0% of
        // `rust` rows attested in a month, every run of it having quietly become
        // this. Blocking hands the row to CI promptly, which is the better trade.
        Err(reason) => {
            eprintln!("\n⚠ {}: {reason}", cfg.host);
            if env::overloaded(sys) {
                let load = env::load_summary(sys);
                eprintln!("  → NOT running {check} here: this machine is at {load}. CI will run the row.");
                return CheckResult {
                    name: check.to_string(),
                    outcome: Outcome::Blocked,
                    secs: began.elapsed().as_secs(),
                    location: "locally".into(),
                    note: Some(format!("{} unusable ({reason}) and this machine is at {load} — left to CI", cfg.host)),
                };
            }
            eprintln!("  → running {check} locally instead — slower, no warm cache on this machine.");
            run_local(sys, root, check)
        }
    }
}

/// The single place an exit code becomes a verdict.
fn classify(code: Option<i32>, was_overloaded: bool, load: impl Fn() -> String) -> (Outcome, Option<String>) {
    match code {
        Some(0) => (Outcome::Pass, None),
        Some(EXIT_PRECONDITION) => (Outcome::Blocked, Some("a precondition was not met — nothing was checked".into())),
        // th-7e81db: a failure on an oversubscribed machine is not evidence about
        // the code. Failing into the blocked bucket is the safe direction: CI runs
        // the row and catches a real break anyway.
        _ if was_overloaded => (
            Outcome::Blocked,
            Some(format!(
                "failed, but this machine is at {} — a sub-second timeout there measures the scheduler, not the commit",
                load()
            )),
        ),
        _ => (Outcome::Fail, None),
    }
}

fn report(r: &CheckResult) {
    match r.outcome {
        Outcome::Pass => println!("✓ {} passed on {} ({}s)", r.name, r.location, r.secs),
        Outcome::Fail => eprintln!("✗ {} FAILED on {} ({}s)", r.name, r.location, r.secs),
        Outcome::Blocked => {
            eprintln!("⊘ {} could not run ({}s) — nothing checked, nothing credited", r.name, r.secs);
            if let Some(note) = &r.note {
                eprintln!("  {note}");
            }
        }
    }
}

fn exit_code(results: &[CheckResult]) -> i32 {
    if results.iter().any(|r| r.outcome == Outcome::Fail) {
        return 1;
    }
    // A check that could not run has verified nothing, so this run is not a
    // success even though nothing failed. Distinct code so a caller can tell the
    // two apart.
    if results.iter().any(|r| r.outcome == Outcome::Blocked) {
        return EXIT_PRECONDITION;
    }
    0
}

fn summarise(results: &[CheckResult], json: bool, sha: &str) {
    if json {
        let checks: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "outcome": r.outcome.label(),
                    "seconds": r.secs,
                    "location": r.location,
                    "posted": r.outcome.state(),
                    "note": r.note,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "sha": sha, "checks": checks, "exit": exit_code(results) }));
        return;
    }

    let credited: Vec<&str> = results.iter().filter(|r| r.outcome == Outcome::Pass).map(|r| r.name.as_str()).collect();
    println!("\nattested: {}", credited.join(" "));

    let blocked: Vec<&str> = results.iter().filter(|r| r.outcome == Outcome::Blocked).map(|r| r.name.as_str()).collect();
    if !blocked.is_empty() {
        eprintln!("not run: {} — CI will run these. Fix the precondition above to skip them.", blocked.join(" "));
    }

    let failed: Vec<&str> = results.iter().filter(|r| r.outcome == Outcome::Fail).map(|r| r.name.as_str()).collect();
    if !failed.is_empty() {
        eprintln!("failed: {}", failed.join(" "));
    }
}

fn show_status(gh: &Gh, sha: &str, available: &[String], json: bool) -> i32 {
    // Say so rather than rendering a lookup failure as "nothing is credited" —
    // those look identical and mean opposite things.
    let posted = gh.attestations(sha).unwrap_or_else(|e| {
        eprintln!("⚠ could not read commit statuses ({e}) — everything below will read as absent.");
        Vec::new()
    });
    let state_of = |name: &str| posted.iter().find(|(c, _)| c == name).map_or("absent", |(_, s)| s.as_str());

    if json {
        let checks: Vec<_> = available.iter().map(|c| serde_json::json!({ "name": c, "state": state_of(c) })).collect();
        println!("{}", serde_json::json!({ "sha": sha, "checks": checks }));
        return 0;
    }

    println!("commit  {sha}");
    if available.is_empty() {
        println!("(this repo defines no checks — add scripts/ci/<name>.sh)");
    }
    for c in available {
        let state = state_of(c);
        let mark = match state {
            "success" => "✓",
            "failure" | "error" => "✗",
            _ => "·",
        };
        println!("  {mark} {c:<16} {state}");
    }
    // The single most common way to read this output wrong.
    println!("\nAttestations attach to the PR head commit (headRefOid) — not the");
    println!("squash-merge commit that lands on the default branch. Checking a");
    println!("merged SHA will always show every check absent.");
    0
}

/// If the commit was ALREADY pushed before we started, a workflow run for it is
/// very likely past its attest step, and crediting now changes nothing for that
/// run. Say so, and hand over the exact command.
fn warn_if_run_in_flight(gh: &Gh, sha: &str) {
    let Some(run) = gh.run_in_flight(sha) else {
        return;
    };
    eprintln!();
    eprintln!("⚠ A workflow run for this commit was already going before these checks");
    eprintln!("  finished, so it read the statuses too early to see them. To use them:");
    eprintln!();
    eprintln!("      gh run cancel {run} && gh run rerun {run}");
    eprintln!();
    eprintln!("  Next time, run `th attest` INSTEAD of git push and this can't happen.");
}

// ── git plumbing ────────────────────────────────────────────────────────────

fn repo_root() -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!("not inside a git repository");
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_ok(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn on_remote(root: &Path, sha: &str) -> bool {
    git(root, &["branch", "-r", "--contains", sha]).is_ok_and(|s| !s.trim().is_empty())
}

// Unix-gated — see the note on `env::tests`.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::env::test_script;
    use super::*;
    use std::fs;
    use std::sync::Arc;

    /// Never reports pressure, so only slots decide admission here.
    struct NoPressure;
    impl ci_queue::pressure::Probe for NoPressure {
        fn read(&self, _: &[PathBuf]) -> ci_queue::pressure::Readings {
            ci_queue::pressure::Readings::default()
        }
    }

    /// A private ci-queue per fixture: never the machine's, never slow.
    fn test_queue(dir: &Path) -> ci_queue::Queue {
        let mut c = ci_queue::config::Config::default();
        c.run.poll_ms = 10;
        c.run.max_wait_secs = 1;
        c.run.note_every_secs = 3600;
        c.run.qos = ci_queue::config::Qos::Normal;
        // These cases are about slots and locks, not capacity.
        c.budget.enabled = false;
        ci_queue::Queue::at(dir.join("ci-queue"), c, Arc::new(NoPressure)).with_cargo_target(dir.join("cargo-target"))
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        calls: PathBuf,
        load: PathBuf,
        sys: Sys,
    }

    /// A throwaway repo whose HEAD is genuinely pushed, with `gh` and `uptime`
    /// replaced by stubs. Nothing here touches the network or the real repo.
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let root = tmp.path().join("repo");
        let calls = tmp.path().join("calls.txt");
        let load = tmp.path().join("load");
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        run_git(tmp.path(), &["init", "-q", "--bare", origin.to_str().unwrap()]);
        run_git(tmp.path(), &["clone", "-q", origin.to_str().unwrap(), root.to_str().unwrap()]);
        run_git(&root, &["config", "user.email", "t@t"]);
        run_git(&root, &["config", "user.name", "t"]);
        fs::create_dir_all(root.join("scripts/ci")).unwrap();
        fs::write(root.join("f"), "x\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-qm", "init"]);
        run_git(&root, &["push", "-q", "origin", "HEAD:main"]);
        run_git(&root, &["checkout", "-qb", "feature"]);
        run_git(&root, &["commit", "-q", "--allow-empty", "-m", "work"]);
        run_git(&root, &["push", "-q", "-u", "origin", "feature"]);
        run_git(&root, &["fetch", "-q", "origin"]);

        // The three outcomes, as check scripts.
        check(&root, "passing", "exit 0");
        check(&root, "failing", "exit 1");
        check(&root, "blocked", &format!("exit {EXIT_PRECONDITION}"));
        // Starts quiet and spikes the load itself, so the two sampling points
        // disagree by construction and only the correct one produces a red status.
        check(&root, "spiking", &format!("echo 999.00 > '{}'\nexit 1", load.display()));
        // Not checks: a helper and a check's own tests.
        fs::write(root.join("scripts/ci/_env.sh"), "#\n").unwrap();
        fs::write(root.join("scripts/ci/passing.test.sh"), "#\n").unwrap();

        let gh = test_script(
            &bin,
            "gh",
            &format!(
                r#"
echo "$*" >> '{calls}'
case "$*" in
  *"-X POST"*)            ;;
  *nameWithOwner*)        echo fake/repo ;;
  *defaultBranchRef*)     echo main ;;
  *"run list"*)           echo '[]' ;;
  *commits*)              echo '{{"statuses":[]}}' ;;
esac
exit 0
"#,
                calls = calls.display()
            ),
        );
        let uptime = test_script(
            &bin,
            "uptime",
            &format!(
                r#"
l=1.00
[ -f '{load}' ] && l=$(cat '{load}')
echo "21:30  up 49 mins, 17 users, load averages: $l 1.00 1.00"
"#,
                load = load.display()
            ),
        );

        let sys = Sys {
            gh: gh.into(),
            uptime: uptime.into(),
            // No container runtime and no package.json in this repo, so neither
            // precondition applies — these cases are about classification.
            docker: tmp.path().join("no-docker").into(),
            cores: 12,
            normalize_path: false,
            // No re-run unless a case asks for one, so overload cases stay fast.
            overload_retry_wait: Duration::ZERO,
            overload_poll: Duration::from_millis(50),
            queue: test_queue(tmp.path()),
            ..Sys::default()
        };

        Fixture {
            _tmp: tmp,
            root,
            calls,
            load,
            sys,
        }
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?} failed");
    }

    fn check(root: &Path, name: &str, body: &str) {
        test_script(&root.join("scripts/ci"), &format!("{name}.sh"), body);
    }

    impl Fixture {
        fn attest(&self, args: &[&str]) -> i32 {
            fs::write(&self.calls, "").unwrap();
            let parsed = parse(args);
            run(&parsed, &self.sys, &self.root).unwrap()
        }

        fn calls(&self) -> String {
            fs::read_to_string(&self.calls).unwrap_or_default()
        }

        /// How many statuses were POSTed for `check`.
        fn posted_for(&self, check: &str) -> usize {
            self.calls()
                .lines()
                .filter(|l| l.contains("statuses/") && l.contains(&format!("ci-attest/{check}")))
                .count()
        }

        fn posted_state(&self, check: &str) -> Option<String> {
            self.calls()
                .lines()
                .find(|l| l.contains("statuses/") && l.contains(&format!("ci-attest/{check}")))
                .and_then(|l| l.split_whitespace().find(|t| t.starts_with("state=")))
                .map(ToString::to_string)
        }
    }

    /// Args exactly as clap would build them, so the flags are covered too.
    fn parse(args: &[&str]) -> AttestArgs {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            inner: AttestArgs,
        }
        let mut line = vec!["attest"];
        line.extend_from_slice(args);
        Wrapper::parse_from(line).inner
    }

    // ── the regression this exists to prevent (th-514dc8) ───────────────────

    #[test]
    fn a_blocked_check_publishes_nothing() {
        let f = fixture();
        let code = f.attest(&["blocked"]);
        assert_eq!(f.posted_for("blocked"), 0, "nothing was checked, so nothing may be claimed");
        assert_eq!(code, EXIT_PRECONDITION);
    }

    // ── SMOODEV-3355: local checks go through the machine-wide ci-queue ─────

    /// A repo's `_<check>-untouched.mjs` filter says "nothing to do": the row is
    /// credited without ever queueing, so it never waits for a slot or the lock.
    #[test]
    fn an_untouched_row_is_credited_without_queueing() {
        let mut f = fixture();
        let bin = f._tmp.path().join("bin");
        f.sys.node = test_script(
            &bin,
            "node-stub",
            "[ \"$(basename \"$1\")\" = _passing-untouched.mjs ] && { echo 'no paths'; exit 0; }\nexit 1",
        )
        .into();
        fs::write(f.root.join("scripts/ci/_passing-untouched.mjs"), "//\n").unwrap();
        assert_eq!(f.attest(&["passing"]), 0);
        assert_eq!(f.posted_state("passing").as_deref(), Some("state=success"));
        assert!(f.sys.queue.history(10).is_empty(), "it must not have taken a queue slot");
    }

    /// Touched (filter exits 1), a hung or missing filter: the check runs.
    #[test]
    fn a_touched_row_or_no_filter_still_runs_queued() {
        let mut f = fixture();
        let bin = f._tmp.path().join("bin");
        f.sys.node = test_script(&bin, "node-stub", "exit 1").into();
        fs::write(f.root.join("scripts/ci/_passing-untouched.mjs"), "//\n").unwrap();
        assert_eq!(f.attest(&["passing"]), 0);
        assert_eq!(f.sys.queue.history(10).len(), 1, "a touched row queues");
        assert_eq!(f.attest(&["failing"]), 1, "no filter script, the check runs");
    }

    #[test]
    fn a_local_check_runs_in_a_heavy_queue_slot() {
        let f = fixture();
        assert_eq!(f.attest(&["passing"]), 0);
        let h = f.sys.queue.history(10);
        assert_eq!(h.len(), 1, "{h:?}");
        assert_eq!(
            (h[0].label.as_str(), h[0].class, h[0].exit),
            ("attest passing", Some(ci_queue::Class::Heavy), 0)
        );
    }

    /// A check whose script drives cargo takes the queue's cargo lock: with
    /// another job holding it, that check waits (here, out to the 1s cap) while
    /// a lock-free check runs.
    #[test]
    fn a_cargo_check_takes_the_cargo_lock() {
        let f = fixture();
        check(&f.root, "rusty", "cargo --version >/dev/null 2>&1 || true\nexit 0");
        let holder = f
            .sys
            .queue
            .admit(&ci_queue::Request {
                class: ci_queue::Class::Light,
                label: "someone else's cargo build".into(),
                cwd: f.root.clone(),
                max_wait: Duration::from_secs(1),
                locks: vec![f.sys.queue.resolve_lock("cargo", &f.root)],
                cmd_hash: None,
            })
            .unwrap();
        assert_eq!(f.attest(&["passing"]), 0, "a lock-free check must not wait on the cargo lock");
        assert_eq!(f.attest(&["rusty"]), EXIT_PRECONDITION, "the cargo check ran while the cargo lock was held");
        assert_eq!(f.posted_for("rusty"), 0);
        drop(holder);
        assert_eq!(f.attest(&["rusty"]), 0);
    }

    #[test]
    fn a_check_that_never_gets_a_slot_is_blocked_not_failed() {
        let f = fixture();
        let hogs: Vec<_> = (0..f.sys.queue.config.slots.heavy)
            .map(|i| {
                f.sys
                    .queue
                    .admit(&ci_queue::Request {
                        class: ci_queue::Class::Heavy,
                        label: format!("hog{i}"),
                        cwd: f.root.clone(),
                        max_wait: Duration::from_secs(1),
                        locks: Vec::new(),
                        cmd_hash: None,
                    })
                    .unwrap()
            })
            .collect();
        let code = f.attest(&["failing"]);
        assert_eq!(f.posted_for("failing"), 0, "a check that never ran may not post a verdict");
        assert_eq!(code, EXIT_PRECONDITION);
        drop(hogs);
    }

    // ── SMOODEV-4018: a marked script runs in the light pool ────────────────

    #[test]
    fn row_class_is_light_only_for_a_marked_script_without_cargo() {
        use ci_queue::Class::{Heavy, Light};
        assert_eq!(row_class("# ci-queue: light\nexit 0", false), Light);
        assert_eq!(
            row_class("#!/usr/bin/env bash\nset -e\n  #ci-queue:light  \nexit 0", false),
            Light,
            "spacing is free"
        );
        assert_eq!(row_class("exit 0", false), Heavy, "no marker is today's behaviour");
        assert_eq!(row_class("", false), Heavy);
        assert_eq!(row_class("# ci-queue: light\ncargo clippy", true), Heavy, "a cargo build is never light");
    }

    #[test]
    fn row_class_ignores_near_misses() {
        use ci_queue::Class::Heavy;
        for text in [
            "echo \"# ci-queue: light\"",
            "exit 0 # ci-queue: light",
            "# ci-queue: heavy",
            "# ci-queue: lightweight",
            "# ci-queue light",
            "# CI-QUEUE: LIGHT",
            "# not ci-queue: light",
        ] {
            assert_eq!(row_class(text, false), Heavy, "{text:?}");
        }
    }

    #[test]
    fn a_marked_check_runs_in_a_light_queue_slot() {
        let f = fixture();
        check(&f.root, "quick", "# ci-queue: light\nexit 0");
        assert_eq!(f.attest(&["quick"]), 0);
        let h = f.sys.queue.history(10);
        assert_eq!(h.len(), 1, "{h:?}");
        assert_eq!((h[0].label.as_str(), h[0].class, h[0].exit), ("attest quick", Some(ci_queue::Class::Light), 0));
    }

    /// The point of the marker: a seconds-long row is not stuck behind a full
    /// heavy pool, while an unmarked row still waits for a heavy slot.
    #[test]
    fn a_marked_check_runs_while_every_heavy_slot_is_held() {
        let f = fixture();
        check(&f.root, "quick", "# ci-queue: light\nexit 0");
        let hogs: Vec<_> = (0..f.sys.queue.config.slots.heavy)
            .map(|i| {
                f.sys
                    .queue
                    .admit(&ci_queue::Request {
                        class: ci_queue::Class::Heavy,
                        label: format!("hog{i}"),
                        cwd: f.root.clone(),
                        max_wait: Duration::from_secs(1),
                        locks: Vec::new(),
                        cmd_hash: None,
                    })
                    .unwrap()
            })
            .collect();
        assert_eq!(f.attest(&["quick"]), 0, "the light row must not wait on the heavy pool");
        assert_eq!(f.posted_state("quick").as_deref(), Some("state=success"));
        assert_eq!(f.attest(&["passing"]), EXIT_PRECONDITION, "an unmarked row still waits for a heavy slot");
        drop(hogs);
    }

    #[test]
    fn a_real_failure_still_publishes_red() {
        let f = fixture();
        let code = f.attest(&["failing"]);
        assert_eq!(f.posted_for("failing"), 1);
        assert_eq!(f.posted_state("failing").as_deref(), Some("state=failure"));
        assert_eq!(code, 1);
    }

    #[test]
    fn a_pass_publishes_green() {
        let f = fixture();
        let code = f.attest(&["passing"]);
        assert_eq!(f.posted_state("passing").as_deref(), Some("state=success"));
        assert_eq!(code, 0);
    }

    #[test]
    fn a_blocked_check_does_not_drag_a_passing_one_down() {
        let f = fixture();
        let code = f.attest(&["passing", "blocked"]);
        assert_eq!(f.posted_for("passing"), 1);
        assert_eq!(f.posted_for("blocked"), 0);
        assert_eq!(code, EXIT_PRECONDITION, "not a success even though nothing failed");
    }

    // ── overload distrust (th-7e81db) ───────────────────────────────────────

    #[test]
    fn an_ambient_overload_swallows_the_failure() {
        let f = fixture();
        fs::write(&f.load, "89.04").unwrap();
        let code = f.attest(&["failing"]);
        assert_eq!(f.posted_for("failing"), 0, "89 on 12 cores measures the scheduler, not the commit");
        assert_eq!(code, EXIT_PRECONDITION);
    }

    // ── SMOODEV-3883: a spike gets one re-run once it passes ────────────────

    /// The check calms the machine on its first run (as a finished spike would)
    /// and passes on the second: the re-run turns a would-be CI row into a credit.
    #[test]
    fn a_failure_under_a_passing_spike_is_rerun_and_credited() {
        let mut f = fixture();
        f.sys.overload_retry_wait = Duration::from_secs(5);
        let runs = f.root.join("runs");
        check(
            &f.root,
            "flaky",
            &format!(
                "[ -f '{runs}' ] && exit 0\ntouch '{runs}'\necho 1.00 > '{load}'\nexit 1",
                runs = runs.display(),
                load = f.load.display()
            ),
        );
        fs::write(&f.load, "89.04").unwrap();
        let code = f.attest(&["flaky"]);
        assert_eq!(f.posted_state("flaky").as_deref(), Some("state=success"));
        assert_eq!(code, 0);
    }

    /// The same failure on the quiet re-run is a real verdict.
    #[test]
    fn a_failure_that_repeats_once_quiet_is_red() {
        let mut f = fixture();
        f.sys.overload_retry_wait = Duration::from_secs(5);
        check(&f.root, "calming", &format!("echo 1.00 > '{}'\nexit 1", f.load.display()));
        fs::write(&f.load, "89.04").unwrap();
        let code = f.attest(&["calming"]);
        assert_eq!(f.posted_state("calming").as_deref(), Some("state=failure"));
        assert_eq!(code, 1);
    }

    /// Load that never falls stays blocked once the bounded wait runs out, and
    /// the check is not run a second time.
    #[test]
    fn load_that_never_falls_stays_blocked() {
        let mut f = fixture();
        f.sys.overload_retry_wait = Duration::from_millis(300);
        let runs = f.root.join("runs");
        check(&f.root, "counted", &format!("echo x >> '{}'\nexit 1", runs.display()));
        fs::write(&f.load, "89.04").unwrap();
        let code = f.attest(&["counted"]);
        assert_eq!(f.posted_for("counted"), 0);
        assert_eq!(code, EXIT_PRECONDITION);
        assert_eq!(fs::read_to_string(&runs).unwrap().lines().count(), 1);
    }

    /// A repo's own `_pressure.sh` runs when a check is distrusted for load.
    #[test]
    fn an_overload_block_runs_the_repos_diagnosis() {
        let f = fixture();
        let ran = f.root.join("diagnosed");
        fs::write(f.root.join("scripts/ci/_pressure.sh"), format!("touch '{}'\n", ran.display())).unwrap();
        fs::write(&f.load, "89.04").unwrap();
        f.attest(&["failing"]);
        assert!(ran.exists(), "the block must say why the machine is busy");
        // ...and a helper is never mistaken for a check.
        assert!(!discover(&f.root).contains(&"_pressure".to_string()));
    }

    #[test]
    fn busy_but_not_oversubscribed_is_still_a_verdict() {
        let f = fixture();
        fs::write(&f.load, "23.00").unwrap();
        let code = f.attest(&["failing"]);
        assert_eq!(f.posted_for("failing"), 1, "23 on 12 cores is busy, not oversubscribed");
        assert_eq!(code, 1);
    }

    /// th-b27ed0, the reason this port exists: on a machine dedicated to
    /// attesting, the check IS the load. Sampling afterwards classifies every
    /// genuine failure as "machine too busy" and silently swallows it. This case
    /// fails against an after-sampling implementation.
    #[test]
    fn a_check_that_spikes_the_load_itself_still_reports_failure() {
        let f = fixture();
        let code = f.attest(&["spiking"]);
        assert_eq!(f.posted_for("spiking"), 1);
        assert_eq!(code, 1);
    }

    // ── refusals ────────────────────────────────────────────────────────────

    #[test]
    fn a_default_branch_sha_is_refused_and_nothing_is_posted() {
        let f = fixture();
        run_git(&f.root, &["checkout", "-q", "main"]);
        let code = f.attest(&["passing"]);
        assert_eq!(code, 1);
        assert_eq!(f.posted_for("passing"), 0, "a status on main marks history no PR run consults");
    }

    #[test]
    fn no_push_with_an_unpushed_head_fails_without_running() {
        let f = fixture();
        // A marker the check would create if it ever ran.
        let ran = f.root.join("ran");
        check(&f.root, "passing", &format!("touch '{}'", ran.display()));
        run_git(&f.root, &["commit", "-q", "--allow-empty", "-m", "local only"]);

        let code = f.attest(&["--no-push", "passing"]);
        assert_eq!(code, 1);
        assert_eq!(f.posted_for("passing"), 0);
        assert!(!ran.exists(), "a commit that cannot be credited should not cost a run");
    }

    #[test]
    fn no_push_is_fine_when_head_is_already_pushed() {
        let f = fixture();
        assert_eq!(f.attest(&["--no-push", "passing"]), 0);
        assert_eq!(f.posted_for("passing"), 1);
    }

    #[test]
    fn an_unreachable_remote_falls_back_to_a_local_run() {
        let f = fixture();
        let stubdir = f.root.join("stubbin");
        std::fs::create_dir_all(&stubdir).unwrap();
        // An ssh that always fails: the box is unreachable, so even the df probe
        // can't answer and `remote::execute` returns Err.
        let ssh = test_script(&stubdir, "ssh", "exit 255");
        let mut sys = f.sys.clone();
        sys.ssh = ssh.into_os_string();
        let cfg = remote::Remote {
            host: "smoo-hub".into(),
            checks: vec!["passing".into()],
            worktree: "/nonexistent".into(),
            target_dir: None,
            env: std::collections::BTreeMap::new(),
            min_free_gib: 5,
            max_target_deps: 60_000,
        };
        // `passing` is `exit 0`; a fall-back LOCAL run of it must pass and report
        // that it ran locally — not block and leave the row to CI.
        let r = run_remote(&sys, &f.root, &cfg, "passing", "origin", "abc");
        assert_eq!(r.location, "locally", "an unreachable box falls back to a LOCAL run");
        assert_eq!(r.outcome, Outcome::Pass, "the local run of a passing check passes");
    }

    /// th-279151: on an oversubscribed machine the fallback would be a cold,
    /// near-hour build whose failures `classify` distrusts anyway — block and let
    /// CI run the row, and never start the check here.
    #[test]
    fn an_unusable_remote_on_an_overloaded_machine_blocks_instead_of_building_locally() {
        let f = fixture();
        let stubdir = f.root.join("stubbin");
        std::fs::create_dir_all(&stubdir).unwrap();
        let ran = f.root.join("ran");
        check(&f.root, "marked", &format!("touch '{}'", ran.display()));
        let mut sys = f.sys.clone();
        sys.ssh = test_script(&stubdir, "ssh", "exit 255").into_os_string();
        sys.uptime = test_script(&stubdir, "uptime", "echo 'load averages: 999.00 1 1'").into_os_string();
        sys.cores = 4;
        let cfg = remote::Remote {
            host: "smoo-hub".into(),
            checks: vec!["marked".into()],
            worktree: "/nonexistent".into(),
            target_dir: None,
            env: std::collections::BTreeMap::new(),
            min_free_gib: 5,
            max_target_deps: 60_000,
        };
        let r = run_remote(&sys, &f.root, &cfg, "marked", "origin", "abc");
        assert_eq!(r.outcome, Outcome::Blocked, "an overloaded fallback is left to CI");
        assert!(r.note.as_deref().unwrap_or_default().contains("left to CI"), "{:?}", r.note);
        assert!(!ran.exists(), "the check must not start on the overloaded machine");
    }

    // ── dirty working tree (the "passed everything, credited nothing" hole) ──

    #[test]
    fn a_dirty_tree_is_refused_and_never_runs_a_check() {
        let f = fixture();
        // A marker the check would create if it ever ran.
        let ran = f.root.join("ran");
        check(&f.root, "passing", &format!("touch '{}'", ran.display()));
        // Dirty a tracked file — the working tree no longer matches HEAD.
        fs::write(f.root.join("f"), "dirty\n").unwrap();

        let code = f.attest(&["passing"]);
        assert_eq!(code, EXIT_DIRTY, "a dirty tree stops the run before it starts");
        assert_eq!(f.posted_for("passing"), 0, "nothing is credited when the tree isn't the commit");
        assert!(!ran.exists(), "the check must not even run — attest credits the commit, not the tree");
    }

    #[test]
    fn allow_dirty_credits_despite_a_dirty_tree() {
        let f = fixture();
        fs::write(f.root.join("f"), "dirty\n").unwrap();
        let code = f.attest(&["--allow-dirty", "passing"]);
        assert_eq!(code, 0, "the escape hatch proceeds");
        assert_eq!(f.posted_state("passing").as_deref(), Some("state=success"));
    }

    #[test]
    fn a_check_that_dirties_a_tracked_file_is_not_credited() {
        let f = fixture();
        // Exits 0 but regenerates a committed file — the codegen-drift shape
        // (a Zod source edited without re-running generation). HEAD holds the
        // stale output, so a green here would be a lie.
        check(&f.root, "drifting", r#"echo changed > "$(git rev-parse --show-toplevel)/f""#);
        let code = f.attest(&["drifting"]);
        assert_eq!(code, EXIT_DIRTY, "a check that leaves the tree dirty exposes stale committed output");
        assert_eq!(f.posted_for("drifting"), 0, "the check passed but its output isn't committed — not credited");
    }

    #[test]
    fn an_unpushed_commit_is_pushed_before_crediting() {
        let f = fixture();
        run_git(&f.root, &["commit", "-q", "--allow-empty", "-m", "more work"]);
        let sha = git(&f.root, &["rev-parse", "HEAD"]).unwrap();

        assert_eq!(f.attest(&["passing"]), 0);
        assert!(on_remote(&f.root, &sha), "a status can only attach to a commit the remote has");
        assert_eq!(f.posted_for("passing"), 1);
    }

    // ── discovery + validation ──────────────────────────────────────────────

    #[test]
    fn discovery_skips_helpers_and_check_tests() {
        let f = fixture();
        assert_eq!(
            discover(&f.root),
            vec!["blocked", "failing", "passing", "spiking"],
            "_env.sh is a helper and passing.test.sh is a test, neither is a check"
        );
    }

    #[test]
    fn a_typo_is_caught_before_anything_runs() {
        let f = fixture();
        let ran = f.root.join("ran");
        check(&f.root, "passing", &format!("touch '{}'", ran.display()));

        let code = f.attest(&["passing", "lnit"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(!ran.exists(), "a typo must not surface five minutes into a run");
        assert_eq!(f.posted_for("passing"), 0);
    }

    #[test]
    fn no_checks_named_is_a_usage_error() {
        let f = fixture();
        assert_eq!(f.attest(&[]), EXIT_USAGE);
    }

    #[test]
    fn all_runs_every_discovered_check() {
        let f = fixture();
        let code = f.attest(&["--all"]);
        assert_eq!(f.posted_for("passing"), 1);
        assert_eq!(f.posted_for("failing"), 1);
        assert_eq!(f.posted_for("blocked"), 0);
        assert_eq!(code, 1, "a failure outranks a block");
    }

    // ── routing ─────────────────────────────────────────────────────────────

    fn with_remote_config(root: &Path, body: &str) {
        fs::create_dir_all(root.join(".smooth")).unwrap();
        fs::write(remote::config_path(root), body).unwrap();
    }

    #[test]
    fn configured_checks_route_to_the_remote_and_the_rest_stay_local() {
        let f = fixture();
        with_remote_config(&f.root, "[remote]\nhost = \"smoo-hub\"\nchecks = [\"failing\"]\nworktree = \"/w\"\n");
        let (local, plan) = route(&parse(&["passing", "failing"]), &f.root, &["passing".into(), "failing".into()]).unwrap();
        assert_eq!(local, vec!["passing".to_string()]);
        assert_eq!(plan.unwrap().1, vec!["failing".to_string()]);
    }

    #[test]
    fn local_flag_overrides_the_config() {
        let f = fixture();
        with_remote_config(&f.root, "[remote]\nhost = \"smoo-hub\"\nchecks = [\"failing\"]\nworktree = \"/w\"\n");
        let (local, plan) = route(&parse(&["--local", "failing"]), &f.root, &["failing".into()]).unwrap();
        assert_eq!(local, vec!["failing".to_string()]);
        assert!(plan.is_none());
    }

    #[test]
    fn an_explicit_host_forces_every_named_check_remote() {
        let f = fixture();
        with_remote_config(&f.root, "[remote]\nhost = \"smoo-hub\"\nchecks = []\nworktree = \"/w\"\n");
        let (local, plan) = route(&parse(&["--remote", "other-box", "passing"]), &f.root, &["passing".into()]).unwrap();
        assert!(local.is_empty());
        let (cfg, checks) = plan.unwrap();
        assert_eq!(cfg.host, "other-box");
        assert_eq!(checks, vec!["passing".to_string()]);
    }

    #[test]
    fn remote_without_a_configured_worktree_is_an_error() {
        let f = fixture();
        assert!(route(&parse(&["--remote", "box", "passing"]), &f.root, &["passing".into()]).is_err());
    }

    #[test]
    fn no_config_means_everything_runs_locally() {
        let f = fixture();
        let (local, plan) = route(&parse(&["passing"]), &f.root, &["passing".into()]).unwrap();
        assert_eq!(local, vec!["passing".to_string()]);
        assert!(plan.is_none());
    }

    // ── classification, in isolation ────────────────────────────────────────

    #[test]
    fn classification_covers_every_exit() {
        let load = || "load 99 on 12 cores".to_string();
        assert_eq!(classify(Some(0), false, load).0, Outcome::Pass);
        assert_eq!(classify(Some(1), false, load).0, Outcome::Fail);
        assert_eq!(classify(Some(96), false, load).0, Outcome::Fail);
        assert_eq!(classify(Some(98), false, load).0, Outcome::Fail);
        assert_eq!(classify(Some(EXIT_PRECONDITION), false, load).0, Outcome::Blocked);
        // Killed by a signal — no code at all.
        assert_eq!(classify(None, false, load).0, Outcome::Fail);
    }

    #[test]
    fn a_pass_on_an_overloaded_machine_is_still_a_pass() {
        let load = || "load 99 on 12 cores".to_string();
        assert_eq!(classify(Some(0), true, load).0, Outcome::Pass, "passing under load is evidence, not noise");
        assert_eq!(classify(Some(1), true, load).0, Outcome::Blocked);
        // The overload must be named, or the developer cannot act on it.
        assert!(classify(Some(1), true, load).1.unwrap().contains("load 99 on 12 cores"));
    }

    #[test]
    fn a_precondition_outranks_overload_distrust() {
        let load = || "load 99 on 12 cores".to_string();
        let (outcome, note) = classify(Some(EXIT_PRECONDITION), true, load);
        assert_eq!(outcome, Outcome::Blocked);
        assert!(note.unwrap().contains("precondition"), "the real reason beats the guess");
    }

    // ── descriptions + exit codes ───────────────────────────────────────────

    #[test]
    fn descriptions_name_where_and_how_long() {
        let local = CheckResult {
            name: "lint".into(),
            outcome: Outcome::Pass,
            secs: 12,
            location: "locally".into(),
            note: None,
        };
        assert_eq!(local.description(), "passed locally in 12s");

        let delegated = CheckResult {
            name: "rust".into(),
            outcome: Outcome::Pass,
            secs: 214,
            location: "smoo-hub".into(),
            note: None,
        };
        assert_eq!(delegated.description(), "passed on smoo-hub in 214s");

        let failed = CheckResult {
            outcome: Outcome::Fail,
            ..delegated
        };
        assert_eq!(failed.description(), "failed on smoo-hub after 214s");
    }

    #[test]
    fn blocked_never_yields_a_state_to_post() {
        assert_eq!(Outcome::Blocked.state(), None);
        assert_eq!(Outcome::Pass.state(), Some("success"));
        assert_eq!(Outcome::Fail.state(), Some("failure"));
    }

    #[test]
    fn a_failure_outranks_a_block_which_outranks_a_pass() {
        let r = |outcome| CheckResult {
            name: "x".into(),
            outcome,
            secs: 0,
            location: "locally".into(),
            note: None,
        };
        assert_eq!(exit_code(&[r(Outcome::Pass)]), 0);
        assert_eq!(exit_code(&[r(Outcome::Pass), r(Outcome::Blocked)]), EXIT_PRECONDITION);
        assert_eq!(exit_code(&[r(Outcome::Blocked), r(Outcome::Fail)]), 1);
        assert_eq!(exit_code(&[]), 0);
    }

    // ── --status ────────────────────────────────────────────────────────────

    #[test]
    fn status_reports_without_running_anything() {
        let f = fixture();
        let ran = f.root.join("ran");
        check(&f.root, "passing", &format!("touch '{}'", ran.display()));
        assert_eq!(f.attest(&["--status"]), 0);
        assert!(!ran.exists());
        assert_eq!(f.posted_for("passing"), 0, "--status is read-only");
    }

    #[test]
    fn status_json_lists_every_discovered_check_as_absent_when_nothing_is_credited() {
        let f = fixture();
        assert_eq!(f.attest(&["--status", "--json"]), 0);
        // The gh stub answers with an empty status list, so every check is absent.
        let gh = Gh::detect(&f.sys).unwrap();
        let sha = git(&f.root, &["rev-parse", "HEAD"]).unwrap();
        assert!(gh.attestations(&sha).unwrap().is_empty());
    }

    // ── preconditions blocking a local run ──────────────────────────────────

    #[test]
    fn an_unrepairable_local_precondition_blocks_without_posting() {
        let mut f = fixture();
        // A repo with a package.json, no node_modules, and installing disabled.
        fs::write(f.root.join("package.json"), "{}").unwrap();
        f.sys.no_install = true;

        let code = f.attest(&["passing"]);
        assert_eq!(code, EXIT_PRECONDITION);
        assert_eq!(f.posted_for("passing"), 0, "the machine was wrong, not the commit");
    }

    #[test]
    fn a_repaired_precondition_lets_the_run_continue() {
        let mut f = fixture();
        fs::write(f.root.join("package.json"), "{}").unwrap();
        let bin = f.root.parent().unwrap().join("bin");
        f.sys.pnpm = test_script(&bin, "pnpm", &format!("mkdir -p '{}/node_modules'", f.root.display())).into();

        assert_eq!(f.attest(&["passing"]), 0);
        assert_eq!(f.posted_for("passing"), 1);
    }
}
