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

/// Below this the remote's system volume is close enough to full that a cargo
/// build will die on ENOSPC and report it as a compile error. That is a fact
/// about the box, so it is a 97, never a verdict.
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

/// Free space on the remote's system volume, in GiB. `None` means the probe
/// itself did not answer, which is treated the same as a failing guard.
pub fn free_gib(sys: &Sys, host: &str) -> Option<u64> {
    let out = Command::new(&sys.ssh)
        .args(["-o", "BatchMode=yes", host, "df -Pk /"])
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
    format!(
        "set -u\n\
         {path}\n\
         lock={lock}\n\
         waited=0\n\
         while ! mkdir \"$lock\" 2>/dev/null; do\n\
         \x20 if [ -n \"$(find \"$lock\" -maxdepth 0 -mmin +{stale} 2>/dev/null)\" ]; then rmdir \"$lock\" 2>/dev/null; continue; fi\n\
         \x20 waited=$((waited + 2))\n\
         \x20 if [ \"$waited\" -ge {wait} ]; then echo \"attest: {worktree} locked by another run for >{wait}s — box busy, not a verdict\" >&2; exit {EXIT_PRECONDITION}; fi\n\
         \x20 sleep 2\n\
         done\n\
         trap 'rmdir \"$lock\" 2>/dev/null' EXIT INT TERM\n\
         cd {worktree} || exit {EXIT_PRECONDITION}\n\
         git fetch --force --quiet {origin} '+{r}:{r}' || exit {EXIT_PRECONDITION}\n\
         git checkout --detach --force {sha} >/dev/null 2>&1 || exit {EXIT_PRECONDITION}\n\
         git clean -ffd --quiet || exit {EXIT_PRECONDITION}\n\
         {target}{extra}bash {script}\n",
        path = remote_path_prelude(),
        worktree = shell_quote(&cfg.worktree),
        origin = shell_quote(origin),
        stale = LOCK_STALE_MIN,
        wait = LOCK_WAIT_SECS,
        // Quoted like every other interpolation. A check name comes from a file
        // name in the repo, and a file name can hold shell metacharacters.
        script = shell_quote(&format!("scripts/ci/{check}.sh")),
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
    match free_gib(sys, &cfg.host) {
        Some(gib) if gib < cfg.min_free_gib => {
            return Err(format!("{} has only {gib}GiB free on / (needs {}GiB)", cfg.host, cfg.min_free_gib));
        }
        Some(_) => {}
        None => return Err(format!("could not read free space on {} — treating the host as unusable", cfg.host)),
    }

    let mut child = Command::new(&sys.ssh)
        .args(["-o", "BatchMode=yes", &cfg.host, "bash", "-s"])
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

    let status = child.wait().map_err(|e| format!("ssh to {} failed: {e}", cfg.host))?;
    for h in [out, err].into_iter().flatten() {
        drop(h.join());
    }

    match status.code() {
        // ssh reserves 255 for its OWN failures — unreachable host, auth refused,
        // connection dropped. None of those say anything about the commit.
        Some(255) | None => Err(format!("ssh to {} failed (unreachable, auth, or dropped connection)", cfg.host)),
        Some(code) => Ok(code),
    }
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
        assert!(s.contains("trap 'rmdir \"$lock\"") && s.contains("EXIT"), "the lock is released on exit");
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
        // Five ways the BOX can be wrong, none a statement about the code: the lock
        // wait timing out (th-983292), then cd, fetch, checkout and clean.
        assert_eq!(s.matches(&format!("exit {EXIT_PRECONDITION}")).count(), 5);
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
        assert!(err.contains("1GiB free"), "{err}");
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
}
