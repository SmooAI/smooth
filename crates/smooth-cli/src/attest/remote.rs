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
    format!(
        "set -u\n\
         {path}\n\
         cd {worktree} || exit {EXIT_PRECONDITION}\n\
         git fetch --force --quiet {origin} '+{r}:{r}' || exit {EXIT_PRECONDITION}\n\
         git checkout --detach --force {sha} >/dev/null 2>&1 || exit {EXIT_PRECONDITION}\n\
         {target}{extra}exec bash {script}\n",
        path = remote_path_prelude(),
        worktree = shell_quote(&cfg.worktree),
        origin = shell_quote(origin),
        // Quoted like every other interpolation. A check name comes from a file
        // name in the repo, and a file name can hold shell metacharacters.
        script = shell_quote(&format!("scripts/ci/{check}.sh")),
    )
}

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
            s.contains("bash 'scripts/ci/rust.sh'"),
            "the check path is quoted like every other interpolation"
        );
        assert!(s.contains("export CARGO_TARGET_DIR='/Volumes/smoo-ext/ci-attest/target'"));
        assert!(s.contains("/opt/homebrew/bin"), "a non-login ssh shell has no Homebrew on PATH (th-92b35a)");
    }

    #[test]
    fn every_host_side_failure_in_the_script_is_a_precondition() {
        let s = remote_script(&cfg(), "rust", "origin", "abc123");
        // cd, fetch and checkout — three ways the BOX can be wrong, none of them
        // a statement about the code.
        assert_eq!(s.matches(&format!("exit {EXIT_PRECONDITION}")).count(), 3);
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
