//! Running a check on another machine, and mapping what comes back.
//!
//! The delegating process keeps two things to itself: the GitHub credentials and
//! the decision. The remote host only ever gets a commit and a script to run — it
//! never posts a status, so a compromised or merely misconfigured build box cannot
//! credit anything.
//!
//! The commit travels as `refs/attest/<sha>`, a namespace no workflow triggers on
//! (`pull_request` fires on PR refs, `push` on `refs/heads/*`), so shipping code
//! to the build box costs nothing on Actions.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::Deserialize;

use super::env::{remote_path_prelude, Sys, EXIT_PRECONDITION};

/// Below this the remote's BUILD volume ([`Remote::build_volume`]) is close
/// enough to full that a cargo build will die on ENOSPC and report it as a
/// compile error. That is a fact about the box, so it is a 97, never a verdict.
const DEFAULT_MIN_FREE_GIB: u64 = 5;

const fn default_min_free_gib() -> u64 {
    DEFAULT_MIN_FREE_GIB
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    remote: Option<Remote>,
}

/// `[remote]` in `<repo>/.smooth/attest.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Remote {
    pub host: String,
    /// Which checks route here. Everything else stays local.
    #[serde(default)]
    pub checks: Vec<String>,
    /// A detached worktree on the host that the attest ref is checked out into.
    pub worktree: String,
    #[serde(default)]
    pub target_dir: Option<String>,
    /// Extra environment for the remote check, verbatim.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_min_free_gib")]
    pub min_free_gib: u64,
    /// Above this many entries in `<target_dir>/debug/deps` the target is moved
    /// aside and rebuilt before the check (th-86de4d, [`prune_script`]).
    #[serde(default = "default_max_target_deps")]
    pub max_target_deps: u64,
}

/// A bound on unbounded growth, not a tuned number: well above what one workspace
/// build leaves behind, far below the smoo-hub target that took over 110s to list.
/// Override per repo with `max_target_deps` in `.smooth/attest.toml`.
const DEFAULT_MAX_TARGET_DEPS: u64 = 60_000;

const fn default_max_target_deps() -> u64 {
    DEFAULT_MAX_TARGET_DEPS
}

pub fn config_path(root: &Path) -> std::path::PathBuf {
    root.join(".smooth/attest.toml")
}

pub fn load(root: &Path) -> Result<Option<Remote>> {
    let path = config_path(root);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: ConfigFile = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(parsed.remote)
}

/// The ref a delegated commit travels on. Namespaced per SHA so two concurrent
/// attests never fight over it, and deleted afterwards.
pub fn attest_ref(sha: &str) -> String {
    format!("refs/attest/{sha}")
}

/// `df -Pk` guarantees one record per filesystem — no wrapping on a long device
/// name, which is the whole reason for `-P`. Column 4 is available 1K-blocks.
fn parse_df_available_kb(out: &str) -> Option<u64> {
    out.lines().filter(|l| !l.trim().is_empty()).nth(1)?.split_whitespace().nth(3)?.parse().ok()
}

impl Remote {
    /// The directory whose volume the check fills: the cargo target when one is
    /// configured (tens of GiB of artifacts), else the worktree.
    ///
    /// th-279151: the guard used to probe `/`. On smoo-hub that is the internal
    /// disk — a family Mac's system volume, ~760MB free and not ours to clean —
    /// while the build lives on `/Volumes/smoo-ext` with 5.9TB free. So EVERY
    /// remote attest was refused as infrastructure and fell back to a local build.
    /// The question the guard asks is "will this build hit ENOSPC", and the answer
    /// lives on the volume the build writes to.
    pub fn build_volume(&self) -> &str {
        self.target_dir.as_deref().unwrap_or(&self.worktree)
    }

    /// Scratch space for the check, a sibling of the worktree (so the in-lock
    /// `git clean` never touches it) on the same volume the guard measured.
    /// Without it, rustc/linker/pnpm temp files land in the system volume's
    /// `/var/folders` — the full disk the guard no longer looks at.
    fn tmp_dir(&self) -> String {
        format!("{}.attest-tmp", self.worktree)
    }
}

/// Free space on the volume holding `path` on `host`, in GiB. `None` means the
/// probe itself did not answer (or the path does not exist there), which is
/// treated the same as a failing guard.
pub fn free_gib(sys: &Sys, host: &str, path: &str) -> Option<u64> {
    let probe = format!("df -Pk {}", shell_quote(path));
    let out = Command::new(&sys.ssh)
        // ConnectTimeout so an UNREACHABLE box fails in seconds — without it, ssh
        // waits out the full TCP handshake timeout, and the caller falls back to a
        // local run (or CI) only after a long, silent stall.
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host, &probe])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_df_available_kb(&String::from_utf8_lossy(&out.stdout)).map(|kb| kb / 1024 / 1024)
}

/// The script the remote shell runs. Every failure that is about the *host*
/// rather than the *commit* exits 97 on purpose.
pub fn remote_script(cfg: &Remote, check: &str, origin: &str, sha: &str) -> String {
    let target = cfg
        .target_dir
        .as_ref()
        .map_or_else(String::new, |d| format!("export CARGO_TARGET_DIR={}\n", shell_quote(d)));
    let extra = cfg.env.iter().fold(String::new(), |mut acc, (k, v)| {
        let _ = writeln!(acc, "export {k}={}", shell_quote(v));
        acc
    });
    let r = attest_ref(sha);
    let lock = shell_quote(&format!("{}.attest-lock", cfg.worktree));
    // th-983292: the host has ONE worktree and ONE target dir, but two agents
    // attest at once (measured live — two `th attest rust` against the same box).
    // Without a mutex their `git checkout`/`git clean`/cargo runs stomp each other:
    // one run's clean deletes the other's in-flight tree. A `mkdir` lock is the
    // portable mutex (macOS has no `flock`) — mkdir is atomic, so exactly one run
    // holds it. A crashed holder can't run its trap, so a lock older than any real
    // run (LOCK_STALE_MIN) is broken; a hard cap (LOCK_WAIT_SECS) means a waiter
    // treats an endless holder as a busy box (97), never an infinite hang
    // (th-7db71c, waiter side). The trap releases it on every ordinary exit — which
    // is why the check runs as `bash`, not `exec bash`: exec would replace the
    // shell and the trap would never fire.
    //
    // th-5123e5: inside the lock, `git clean -ffd` after `checkout --force` makes
    // the worktree match the SHA exactly. `checkout --force` resets tracked files
    // but leaves untracked leftovers (the incident: a stray `api-prime/tests/*.rs`
    // from an abandoned branch) that cargo would compile into a FALSE red. No `-x`:
    // the cargo cache is an external CARGO_TARGET_DIR, cheap to keep. Every failure
    // here is the BOX being wrong, not the commit, so each exits as a precondition.
    //
    // th-86de4d — the lock must never outlive, or be outlived by, the build:
    //  * The old `trap 'rmdir lock' EXIT INT TERM` REPLACED the default action of
    //    INT/TERM. A killed run (watchdog, ctrl-c) released the lock and kept
    //    building — found on smoo-hub as a 50-minute `bash -s` under PID 1 while the
    //    next run took the "free" lock and built into the same target dir. Now each
    //    signal trap EXITS, and the EXIT trap kills the check's whole process group.
    //  * The check runs as a background job under `set -m`, so it is its own process
    //    group (cargo's rustc children included) and `wait` returns the moment a
    //    signal lands instead of after the build. stdin is /dev/null: this shell is
    //    reading its own script from stdin, which the check must not consume.
    //  * The lock records the holder's pid and the check's process group. A waiter
    //    breaks it only when that holder is dead (and kills its orphaned group
    //    first); the age rule is kept for a lock with no recorded holder.
    let template = r#"set -u
@PATH@
lock=@LOCK@
child=
cleanup() {
  if [ -n "$child" ] && kill -0 -- "-$child" 2>/dev/null; then
    kill -TERM -- "-$child" 2>/dev/null; sleep 2; kill -KILL -- "-$child" 2>/dev/null
  fi
  rm -rf "$lock"
}
waited=0
while ! mkdir "$lock" 2>/dev/null; do
  holder=$(cat "$lock/pid" 2>/dev/null)
  if { [ -n "$holder" ] && ! kill -0 "$holder" 2>/dev/null; } || { [ -z "$holder" ] && [ -n "$(find "$lock" -maxdepth 0 -mmin +@STALE@ 2>/dev/null)" ]; }; then
    pg=$(cat "$lock/pgid" 2>/dev/null)
    if [ -n "$pg" ]; then kill -TERM -- "-$pg" 2>/dev/null; fi
    rm -rf "$lock"; continue
  fi
  waited=$((waited + 2))
  if [ "$waited" -ge @WAIT@ ]; then echo "attest: @WORKTREE_Q@ locked by another run for >@WAIT@s — box busy, not a verdict" >&2; exit @PRE@; fi
  sleep 2
done
echo $$ > "$lock/pid"
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
cd @WORKTREE@ || exit @PRE@
git fetch --force --quiet @ORIGIN@ '+@REF@:@REF@' || exit @PRE@
git checkout --detach --force @SHA@ >/dev/null 2>&1 || exit @PRE@
git clean -ffd --quiet || exit @PRE@
rm -rf @TMP@; mkdir -p @TMP@ || exit @PRE@
export TMPDIR=@TMP@
@TARGET@@PRUNE@@EXTRA@set -m
bash @SCRIPT@ </dev/null &
child=$!
set +m
echo "$child" > "$lock/pgid"
wait "$child"
rc=$?
child=
exit "$rc"
"#;
    template
        .replace("@PATH@", &remote_path_prelude())
        .replace("@LOCK@", &lock)
        .replace("@STALE@", &LOCK_STALE_MIN.to_string())
        .replace("@WAIT@", &LOCK_WAIT_SECS.to_string())
        .replace("@PRE@", &EXIT_PRECONDITION.to_string())
        .replace("@WORKTREE_Q@", &shell_quote(&cfg.worktree))
        .replace("@WORKTREE@", &shell_quote(&cfg.worktree))
        .replace("@ORIGIN@", &shell_quote(origin))
        .replace("@REF@", &r)
        .replace("@SHA@", sha)
        // th-279151: fresh per run (we hold the lock, so no peer is using it) and on
        // the build volume, never the system disk's /var/folders.
        .replace("@TMP@", &shell_quote(&cfg.tmp_dir()))
        .replace("@TARGET@", &target)
        .replace("@PRUNE@", &prune_script(cfg))
        .replace("@EXTRA@", &extra)
        // Quoted like every other interpolation. A check name comes from a file
        // name in the repo, and a file name can hold shell metacharacters.
        .replace("@SCRIPT@", &shell_quote(&format!("scripts/ci/{check}.sh")))
}

/// th-86de4d: cap the cargo target before building. Nothing ever pruned it —
/// smoo-hub's grew to 302GB — and rustc lists `target/debug/deps` on EVERY
/// invocation (`SearchPath::new`, sampled); at that size one listing took over
/// 110s, so a workspace clippy could not finish inside the deadline and every
/// rust attest timed out. Past `max_target_deps` entries the target is moved
/// aside (a rename: instant) and deleted in the background, so this run starts
/// cold instead of stalled. Only for a configured `target_dir` — never a default
/// target inside the worktree.
fn prune_script(cfg: &Remote) -> String {
    let Some(dir) = cfg.target_dir.as_deref() else {
        return String::new();
    };
    let t = shell_quote(dir);
    format!(
        "deps={t}/debug/deps\n\
         if [ -d \"$deps\" ] && [ \"$(ls -f \"$deps\" | wc -l)\" -gt {max} ]; then\n\
         \x20 echo \"attest: $deps holds over {max} entries — every rustc lists it at startup, so the target is moved aside and rebuilt (th-86de4d)\" >&2\n\
         \x20 trash={t}.trash-$$\n\
         \x20 mv {t} \"$trash\" && (nohup rm -rf \"$trash\" >/dev/null 2>&1 &)\n\
         fi\n",
        max = cfg.max_target_deps
    )
}

/// A run holding the worktree longer than this (minutes) is presumed dead — its
/// trap never fired — so a waiter breaks the lock. Comfortably longer than a warm
/// rust build, so it never breaks a lock a real run still holds.
const LOCK_STALE_MIN: u64 = 40;

/// The hard cap (seconds) a waiter will block for the lock before treating the
/// host as busy. Bounds the wait so a wedged holder is never an infinite hang.
const LOCK_WAIT_SECS: u64 = 3000;

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Run `check` on `cfg.host`. `Ok(code)` is the check's own exit status;
/// `Err(reason)` is an infrastructure problem — unreachable host, auth failure,
/// no disk — which is never a verdict on the commit.
pub fn execute(sys: &Sys, cfg: &Remote, check: &str, origin: &str, sha: &str) -> Result<i32, String> {
    let volume = cfg.build_volume();
    match free_gib(sys, &cfg.host, volume) {
        Some(gib) if gib < cfg.min_free_gib => {
            return Err(format!("{} has only {gib}GiB free on {volume} (needs {}GiB)", cfg.host, cfg.min_free_gib));
        }
        Some(_) => {}
        None => {
            return Err(format!(
                "could not read free space for {volume} on {} — treating the host as unusable",
                cfg.host
            ))
        }
    }

    let mut child = Command::new(&sys.ssh)
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", &cfg.host, "bash", "-s"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start ssh to {}: {e}", cfg.host))?;

    let script = remote_script(cfg, check, origin, sha);
    if let Some(mut stdin) = child.stdin.take() {
        // A closed pipe here means ssh already died; the exit status below says why.
        let _ = stdin.write_all(script.as_bytes());
    }

    let prefix = format!("[{}] ", cfg.host);
    let out = child.stdout.take().map(|s| stream(s, prefix.clone(), false));
    let err = child.stderr.take().map(|s| stream(s, prefix, true));

    // th-7db71c: bound the whole run. `ConnectTimeout` covers reaching the host,
    // but once connected the check can wedge — measured repeatedly: cargo finished,
    // yet rust.sh hung in a docker probe (th-c9057c) and `child.wait()` never
    // returned, hanging th with it. A watchdog kills the ssh after a deadline;
    // killing the local client drops the connection so sshd SIGHUPs the remote
    // check too. Off the happy path, so a fast check clears `done` and the
    // watchdog exits without touching anything.
    let deadline = deadline_secs();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog = {
        let (done, timed_out, pid) = (done.clone(), timed_out.clone(), child.id());
        std::thread::spawn(move || {
            let mut waited = 0u64;
            while waited < deadline {
                if done.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
                waited += 1; // 500ms steps → `deadline` is in half-seconds below
            }
            timed_out.store(true, std::sync::atomic::Ordering::Relaxed);
            // Portable kill by pid (no libc): TERM, then a hard KILL shortly after.
            // stderr silenced — the second kill usually races a process the first
            // already reaped ("No such process"), which is success, not an error.
            let kill = |args: &[&str]| {
                let _ = Command::new("kill").args(args).stdout(Stdio::null()).stderr(Stdio::null()).status();
            };
            let pid = pid.to_string();
            kill(&[&pid]);
            std::thread::sleep(std::time::Duration::from_secs(3));
            kill(&["-9", &pid]);
        })
    };

    let status = child.wait().map_err(|e| format!("ssh to {} failed: {e}", cfg.host))?;
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = watchdog.join();
    for h in [out, err].into_iter().flatten() {
        drop(h.join());
    }

    if timed_out.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(format!(
            "{} did not finish {check} within {}s — killed (the host is stuck)",
            cfg.host,
            deadline / 2
        ));
    }
    match status.code() {
        // ssh reserves 255 for its OWN failures — unreachable host, auth refused,
        // connection dropped. None of those say anything about the commit.
        Some(255) | None => Err(format!("ssh to {} failed (unreachable, auth, or dropped connection)", cfg.host)),
        Some(code) => Ok(code),
    }
}

/// The watchdog's budget, in half-second ticks (so the loop above counts in 500ms
/// steps). Default 45 minutes — comfortably longer than a warm rust build, so it
/// only ever fires on a genuinely stuck host. `SMOOTH_ATTEST_REMOTE_DEADLINE_SECS`
/// overrides it (tests set it low).
fn deadline_secs() -> u64 {
    std::env::var("SMOOTH_ATTEST_REMOTE_DEADLINE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2700)
        .saturating_mul(2)
}

fn stream(from: impl Read + Send + 'static, prefix: String, to_stderr: bool) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(from).lines().map_while(Result::ok) {
            if to_stderr {
                eprintln!("{prefix}{line}");
            } else {
                println!("{prefix}{line}");
            }
        }
    })
}

// Unix-gated — see the note on `env::tests`.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use crate::attest::env::test_script;

    fn cfg() -> Remote {
        Remote {
            host: "smoo-hub".into(),
            checks: vec!["rust".into()],
            worktree: "/Volumes/smoo-ext/ci-attest/smooai".into(),
            target_dir: Some("/Volumes/smoo-ext/ci-attest/target".into()),
            env: BTreeMap::new(),
            min_free_gib: DEFAULT_MIN_FREE_GIB,
            max_target_deps: DEFAULT_MAX_TARGET_DEPS,
        }
    }

    fn sys_with_ssh(path: std::path::PathBuf) -> Sys {
        Sys {
            ssh: path.into(),
            normalize_path: false,
            ..Sys::default()
        }
    }

    /// One stub covers both calls the runner makes: the `df` probe (argv ends in
    /// `df -Pk /`) and the check itself (argv ends in `bash -s`).
    fn ssh_stub(dir: &Path, free_kb: u64, check_exit: i32) -> std::path::PathBuf {
        test_script(
            dir,
            "ssh",
            &format!(
                r#"
case "$*" in
  *"df -Pk"*) printf 'Filesystem 1024-blocks Used Available Capacity Mounted\n/dev/disk1 100 1 {free_kb} 1%% /\n'; exit 0 ;;
esac
cat >/dev/null
exit {check_exit}
"#
            ),
        )
    }

    // ── config ──────────────────────────────────────────────────────────────

    #[test]
    fn parses_the_documented_config() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".smooth")).unwrap();
        std::fs::write(
            config_path(tmp.path()),
            r#"
[remote]
host = "smoo-hub"
checks = ["rust"]
worktree = "/Volumes/smoo-ext/ci-attest/smooai"
target_dir = "/Volumes/smoo-ext/ci-attest/target"
"#,
        )
        .unwrap();

        let r = load(tmp.path()).unwrap().unwrap();
        assert_eq!(r.host, "smoo-hub");
        assert_eq!(r.checks, vec!["rust".to_string()]);
        assert_eq!(r.target_dir.as_deref(), Some("/Volumes/smoo-ext/ci-attest/target"));
        assert_eq!(r.min_free_gib, DEFAULT_MIN_FREE_GIB, "the disk guard has a default");
    }

    #[test]
    fn no_config_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn a_malformed_config_is_reported_not_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".smooth")).unwrap();
        std::fs::write(config_path(tmp.path()), "[remote]\nchecks = 3\n").unwrap();
        assert!(load(tmp.path()).is_err());
    }

    // ── the remote script ───────────────────────────────────────────────────

    #[test]
    fn the_script_fetches_the_attest_ref_and_runs_the_check() {
        let s = remote_script(&cfg(), "rust", "git@github.com:SmooAI/smooai.git", "abc123");
        assert!(s.contains("refs/attest/abc123"));
        assert!(s.contains("git checkout --detach --force abc123"));
        assert!(
            s.contains("git clean -ffd"),
            "th-5123e5: the tree must be cleaned of untracked leftovers after checkout, or a stale box posts a false red"
        );
        assert!(
            s.find("git checkout").unwrap() < s.find("git clean").unwrap(),
            "clean runs AFTER the checkout — it removes what the checkout left behind"
        );
        assert!(
            s.contains("bash 'scripts/ci/rust.sh'"),
            "the check path is quoted like every other interpolation"
        );
        // th-983292: a mkdir mutex serializes concurrent attests on the one shared
        // worktree, and it MUST be taken before the checkout that would clobber a
        // peer's tree. Not `exec`, or the release trap never fires.
        assert!(s.contains("mkdir \"$lock\""), "concurrent runs serialize on a mkdir lock");
        assert!(
            s.contains("trap cleanup EXIT") && s.contains("rm -rf \"$lock\""),
            "the lock is released on exit"
        );
        assert!(
            s.find("mkdir \"$lock\"").unwrap() < s.find("git checkout").unwrap(),
            "the lock is held before the checkout it protects"
        );
        assert!(!s.contains("exec bash"), "exec would skip the release trap");
        assert!(s.contains("export CARGO_TARGET_DIR='/Volumes/smoo-ext/ci-attest/target'"));
        assert!(s.contains("/opt/homebrew/bin"), "a non-login ssh shell has no Homebrew on PATH (th-92b35a)");
    }

    #[test]
    fn every_host_side_failure_in_the_script_is_a_precondition() {
        let s = remote_script(&cfg(), "rust", "origin", "abc123");
        // Six ways the BOX can be wrong, none a statement about the code: the lock
        // wait timing out (th-983292), then cd, fetch, checkout, clean, and making
        // the scratch dir (th-279151).
        assert_eq!(s.matches(&format!("exit {EXIT_PRECONDITION}")).count(), 6);
    }

    /// th-279151: temp files belong on the build volume, not the system disk the
    /// guard no longer measures — fresh each run, set before the check starts, and
    /// outside the worktree so `git clean` never races it.
    #[test]
    fn the_check_gets_a_fresh_tmpdir_on_the_build_volume() {
        let s = remote_script(&cfg(), "rust", "origin", "abc");
        assert!(s.contains("export TMPDIR='/Volumes/smoo-ext/ci-attest/smooai.attest-tmp'"), "{s}");
        assert!(
            s.contains("rm -rf '/Volumes/smoo-ext/ci-attest/smooai.attest-tmp'"),
            "a stale tmp from a crashed run is cleared"
        );
        assert!(
            s.find("while ! mkdir \"$lock\"").unwrap() < s.find("rm -rf '/Volumes/smoo-ext/ci-attest/smooai.attest-tmp'").unwrap(),
            "only the lock holder may clear it"
        );
        assert!(s.find("export TMPDIR").unwrap() < s.find("bash 'scripts/ci/rust.sh'").unwrap());
    }

    #[test]
    fn the_build_volume_is_the_target_dir_else_the_worktree() {
        assert_eq!(cfg().build_volume(), "/Volumes/smoo-ext/ci-attest/target");
        let mut c = cfg();
        c.target_dir = None;
        assert_eq!(c.build_volume(), "/Volumes/smoo-ext/ci-attest/smooai");
    }

    /// th-279151: the regression itself. smoo-hub's `/` had 760MB free while the
    /// build volume had terabytes; probing `/` refused every remote attest.
    #[test]
    fn the_disk_guard_probes_the_build_volume_not_the_system_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("argv");
        let ssh = test_script(
            tmp.path(),
            "ssh",
            &format!(
                r#"
echo "$*" >> '{log}'
case "$*" in
  *"df -Pk"*) printf 'Filesystem 1024-blocks Used Available Capacity Mounted\n/dev/disk5s1 100 1 {free} 1%% /Volumes/smoo-ext\n'; exit 0 ;;
esac
cat >/dev/null
exit 0
"#,
                log = log.display(),
                free = 50u64 * 1024 * 1024
            ),
        );
        assert_eq!(execute(&sys_with_ssh(ssh), &cfg(), "rust", "origin", "abc"), Ok(0));
        let argv = std::fs::read_to_string(&log).unwrap();
        let probe = argv.lines().find(|l| l.contains("df -Pk")).unwrap();
        assert!(probe.ends_with("df -Pk '/Volumes/smoo-ext/ci-attest/target'"), "probed: {probe}");
    }

    #[test]
    fn the_lock_serializes_recovers_from_a_crash_and_never_hangs() {
        let s = remote_script(&cfg(), "rust", "origin", "abc");
        assert!(s.contains("mkdir \"$lock\""), "the mutex is an atomic mkdir (macOS has no flock)");
        assert!(
            s.contains(&format!("-mmin +{LOCK_STALE_MIN}")),
            "a lock older than any real run is broken — a crashed holder never ran its trap"
        );
        assert!(
            s.contains(&format!("-ge {LOCK_WAIT_SECS}")),
            "the wait is bounded — a wedged holder is a busy box (97), never an infinite hang"
        );
        // The lock is a SIBLING of the worktree, never inside it — otherwise the
        // in-lock `git clean -ffd` would delete the lock the run is holding.
        assert!(s.contains(".attest-lock"));
        assert!(!s.contains("smooai/.attest-lock"), "the lock lives beside the worktree, not within it");
    }

    /// th-86de4d: a signal trap that only releases the lock REPLACES the default
    /// action — the shell kept running (and building) with the lock gone. Each
    /// signal trap must exit, and the exit path must take the check's whole
    /// process group down before it frees the lock.
    #[test]
    fn signals_end_the_run_and_take_the_check_down_before_the_lock_is_freed() {
        let s = remote_script(&cfg(), "rust", "origin", "abc");
        assert!(!s.contains("trap 'rmdir"), "the old release-only trap is gone:\n{s}");
        for sig in ["trap 'exit 129' HUP", "trap 'exit 130' INT", "trap 'exit 143' TERM", "trap cleanup EXIT"] {
            assert!(s.contains(sig), "missing `{sig}`");
        }
        let cleanup = &s[s.find("cleanup() {").unwrap()..s.find("\n}\n").unwrap()];
        assert!(
            cleanup.find("kill -TERM -- \"-$child\"").unwrap() < cleanup.find("rm -rf \"$lock\"").unwrap(),
            "the group dies BEFORE the lock is released:\n{cleanup}"
        );
        assert!(
            s.contains("set -m\nbash 'scripts/ci/rust.sh' </dev/null &"),
            "the check is its own process group, and never reads this shell's script from stdin"
        );
        assert!(s.contains("wait \"$child\""));
    }

    /// th-86de4d: the waiter trusts a LIVE holder however long it runs, breaks a
    /// dead holder's lock at once, and kills that holder's orphaned build first.
    #[test]
    fn a_dead_holders_lock_is_broken_and_its_orphans_killed() {
        let s = remote_script(&cfg(), "rust", "origin", "abc");
        assert!(s.contains("echo $$ > \"$lock/pid\""), "the holder records itself");
        assert!(s.contains("echo \"$child\" > \"$lock/pgid\""), "and the check's process group");
        assert!(s.contains("! kill -0 \"$holder\""), "a waiter checks the holder is alive");
        let wait_loop = &s[s.find("while ! mkdir").unwrap()..s.find("\ndone\n").unwrap()];
        assert!(
            wait_loop.find("kill -TERM -- \"-$pg\"").unwrap() < wait_loop.find("rm -rf \"$lock\"").unwrap(),
            "orphans die before the lock is taken over:\n{wait_loop}"
        );
    }

    #[test]
    fn an_oversized_target_is_moved_aside_before_the_check() {
        let s = remote_script(&cfg(), "rust", "origin", "abc");
        assert!(s.contains(&format!("-gt {DEFAULT_MAX_TARGET_DEPS} ]")), "{s}");
        assert!(s.contains("deps='/Volumes/smoo-ext/ci-attest/target'/debug/deps"));
        assert!(
            s.contains("mv '/Volumes/smoo-ext/ci-attest/target' \"$trash\""),
            "a rename — instant — not an inline rm"
        );
        assert!(s.find("mv '/Volumes").unwrap() < s.find("bash 'scripts/ci/rust.sh'").unwrap());
        let mut c = cfg();
        c.target_dir = None;
        assert!(
            !remote_script(&c, "rust", "origin", "abc").contains("debug/deps"),
            "never prunes a target it does not own"
        );
    }

    /// th-86de4d, end to end: run the REAL generated script against a real git
    /// origin, SIGTERM the shell mid-check (what a killed ssh session delivers),
    /// and prove the check process is gone and the lock released.
    #[test]
    fn a_terminated_run_leaves_no_orphaned_check_and_no_lock() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        let origin = tmp.path().join("origin.git");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare"]);
        std::fs::create_dir_all(work.join("scripts/ci")).unwrap();
        git(&work, &["init", "-q"]);
        let pidfile = tmp.path().join("check.pid");
        std::fs::write(work.join("scripts/ci/slow.sh"), format!("echo $$ > '{}'\nsleep 60\n", pidfile.display())).unwrap();
        git(&work, &["add", "."]);
        git(&work, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "slow"]);
        let sha = String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&work).output().unwrap().stdout).unwrap();
        let sha = sha.trim();
        git(&work, &["push", "-q", origin.to_str().unwrap(), &format!("HEAD:refs/attest/{sha}")]);

        let c = Remote {
            host: "local".into(),
            checks: vec!["slow".into()],
            worktree: work.to_string_lossy().into_owned(),
            target_dir: None,
            env: BTreeMap::new(),
            min_free_gib: 0,
            max_target_deps: DEFAULT_MAX_TARGET_DEPS,
        };
        let script = remote_script(&c, "slow", origin.to_str().unwrap(), sha);
        let mut shell = Command::new("bash")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        shell.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();

        let began = std::time::Instant::now();
        while !pidfile.exists() {
            assert!(began.elapsed().as_secs() < 20, "the check never started");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let check_pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
        let lock = format!("{}.attest-lock", c.worktree);
        assert!(Path::new(&lock).exists(), "the run holds the lock while the check runs");

        let _ = Command::new("kill").args(["-TERM", &shell.id().to_string()]).status();
        let _ = shell.wait();

        let alive = || Command::new("kill").args(["-0", &check_pid]).stderr(Stdio::null()).status().unwrap().success();
        let deadline = std::time::Instant::now();
        while alive() && deadline.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!alive(), "the check outlived its killed shell — the orphan th-86de4d found on smoo-hub");
        assert!(!Path::new(&lock).exists(), "the lock is released once the run is gone");
    }

    /// The lock logic is hand-rolled shell; a stray token would break every remote
    /// attest silently. `bash -n` parses without running, catching a syntax slip.
    #[test]
    fn the_generated_script_is_valid_shell() {
        use std::io::Write as _;
        let s = remote_script(&cfg(), "rust", "git@github.com:SmooAI/smooai.git", "abc123");
        let mut child = Command::new("bash")
            .arg("-n")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "generated remote script is not valid bash:\n{s}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn quoting_survives_a_hostile_path() {
        let mut c = cfg();
        c.worktree = "/tmp/it's here".into();
        let s = remote_script(&c, "rust", "origin", "abc");
        assert!(s.contains(r"'/tmp/it'\''s here'"));
    }

    // ── disk guard ──────────────────────────────────────────────────────────

    #[test]
    fn parses_df_output() {
        let out = "Filesystem 1024-blocks     Used Available Capacity Mounted on\n/dev/disk3s1s1 971350180 9878280 52428800 17% /\n";
        assert_eq!(parse_df_available_kb(out), Some(52_428_800));
    }

    #[test]
    fn a_full_remote_volume_blocks_rather_than_fails() {
        let tmp = tempfile::tempdir().unwrap();
        // 1GiB free, guard wants 5.
        let sys = sys_with_ssh(ssh_stub(tmp.path(), 1024 * 1024, 0));
        let err = execute(&sys, &cfg(), "rust", "origin", "abc").unwrap_err();
        assert!(err.contains("1GiB free on /Volumes/smoo-ext/ci-attest/target"), "{err}");
    }

    #[test]
    fn enough_disk_lets_the_check_run() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = sys_with_ssh(ssh_stub(tmp.path(), 50 * 1024 * 1024, 0));
        assert_eq!(execute(&sys, &cfg(), "rust", "origin", "abc"), Ok(0));
    }

    #[test]
    fn an_unanswerable_disk_probe_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = sys_with_ssh(test_script(tmp.path(), "ssh", "exit 255"));
        assert!(execute(&sys, &cfg(), "rust", "origin", "abc").is_err());
    }

    // ── exit mapping ────────────────────────────────────────────────────────

    #[test]
    fn a_remote_failure_is_a_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = sys_with_ssh(ssh_stub(tmp.path(), 50 * 1024 * 1024, 1));
        assert_eq!(execute(&sys, &cfg(), "rust", "origin", "abc"), Ok(1));
    }

    #[test]
    fn a_remote_precondition_stays_a_precondition() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = sys_with_ssh(ssh_stub(tmp.path(), 50 * 1024 * 1024, EXIT_PRECONDITION));
        assert_eq!(execute(&sys, &cfg(), "rust", "origin", "abc"), Ok(EXIT_PRECONDITION));
    }

    #[test]
    fn ssh_exit_255_is_never_a_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        // Disk probe answers fine; the check invocation is what dies with 255.
        let sys = sys_with_ssh(ssh_stub(tmp.path(), 50 * 1024 * 1024, 255));
        let err = execute(&sys, &cfg(), "rust", "origin", "abc").unwrap_err();
        assert!(err.contains("ssh to smoo-hub failed"), "{err}");
    }

    #[test]
    fn a_missing_ssh_binary_blocks_rather_than_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = sys_with_ssh(tmp.path().join("no-such-ssh"));
        assert!(execute(&sys, &cfg(), "rust", "origin", "abc").is_err());
    }

    /// th-7db71c: once connected, a wedged check must not hang forever. The df
    /// probe answers, then the check invocation sleeps — the watchdog kills it at
    /// the deadline and execute returns Err (which the caller turns into a local
    /// run), rather than blocking on `child.wait()` indefinitely.
    #[test]
    fn a_stuck_remote_is_killed_at_the_deadline_not_left_to_hang() {
        let tmp = tempfile::tempdir().unwrap();
        // df answers so the disk guard passes; the check invocation then hangs.
        let ssh = test_script(
            tmp.path(),
            "ssh",
            // `exec sleep` so the stub's pid IS the sleep — a TERM to it dies at
            // once, like the real ssh client (a bash parent would defer the signal
            // until its `sleep` child returned, which is not how ssh behaves).
            "case \"$*\" in\n  *\"df -Pk\"*) printf 'Filesystem 1024-blocks Used Available Capacity Mounted\\n/dev/d 100 1 999999999 1%% /\\n'; exit 0 ;;\nesac\nexec sleep 30\n",
        );
        // 1s → the watchdog fires almost immediately instead of after the 30s sleep.
        std::env::set_var("SMOOTH_ATTEST_REMOTE_DEADLINE_SECS", "1");
        let sys = sys_with_ssh(ssh);
        let began = std::time::Instant::now();
        let result = execute(&sys, &cfg(), "rust", "origin", "abc");
        let secs = began.elapsed().as_secs();
        std::env::remove_var("SMOOTH_ATTEST_REMOTE_DEADLINE_SECS");
        let err = result.expect_err("a stuck remote must return Err, not hang");
        assert!(err.contains("did not finish"), "the error names the deadline kill: {err}");
        assert!(secs < 15, "killed near the ~1s deadline, not after the 30s sleep — took {secs}s");
    }
}
