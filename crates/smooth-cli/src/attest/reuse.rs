//! Credit reuse (SMOODEV-4160): a pass is a fact about a check's INPUTS, not
//! about a commit SHA.
//!
//! A credit is still posted per SHA — that is what the workflow reads — but a
//! check whose inputs did not change since it last passed does not need to run
//! again to earn one. Borrowed from dagger, which caches a check's verdict
//! against the content of what it reads, with a TTL, rather than against the
//! commit that happened to carry it. Before this, a docs fix, an amended message
//! or a rebase over unrelated `main` commits re-ran every row from scratch.
//!
//! # When a pass carries over
//!
//! Every pass is recorded in a local ledger as `(repo, check, sha, base)`, where
//! `base` is the commit's merge-base with the default branch. For a new HEAD, a
//! recorded pass of the same check is reused when ALL of these hold:
//!
//! 1. it is younger than the max age (dagger's TTL — the toolchain and the
//!    machine drift even when the tree does not);
//! 2. both recorded commits still exist locally;
//! 3. nothing under `scripts/ci/` differs, between the two heads or between the
//!    two bases — a change to the checks changes every verdict;
//! 4. every other changed path (head vs head, plus base vs base, because the
//!    scoped rows pick what to run from the diff against the base) is one the
//!    repo says the check does not read.
//!
//! The repo answers 4, because only the repo knows what its checks read. `th`
//! runs `scripts/ci/_rows-touched.sh` with the changed paths on stdin, one per
//! line, and reads one `<check> touched|untouched` line per check it knows. A
//! check the hook does not name, a hook that is missing, fails, or takes longer
//! than ten seconds: no reuse, the check runs. Every doubt resolves to running.
//! An empty change set (the same tree, or the same commit) needs no hook.
//!
//! The trust model is the one attestation already has: a status says "someone
//! ran this and it passed", and reuse says "…on inputs identical to these".

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

/// The repo's answer to "which checks read these paths". See the module docs.
pub const HOOK: &str = "scripts/ci/_rows-touched.sh";

/// A change here is a change to the checks themselves: nothing carries over it.
const CHECK_DEFINITIONS: &str = "scripts/ci/";

/// How many recent passes of a check are considered, newest first.
const CANDIDATES: usize = 20;

const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// A pass found to carry over to the new HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reused {
    pub sha: String,
    /// How long the original run took, for the status description.
    pub secs: u64,
}

/// One recorded pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pass {
    pub sha: String,
    pub base: String,
    pub secs: u64,
    pub at_ms: u64,
}

/// The local record of passes, in SQLite (WAL, busy timeout) like the queue's
/// history: every `th attest` on the machine writes it concurrently. It is
/// advisory — a ledger that cannot be read means a check runs, never that one
/// is skipped.
#[derive(Clone, Debug)]
pub struct Ledger {
    path: PathBuf,
    max_age: Duration,
}

impl Ledger {
    pub fn new(path: PathBuf, max_age: Duration) -> Self {
        Self { path, max_age }
    }

    /// `~/.smooth/attest/credits.db`, or `SMOOTH_ATTEST_LEDGER`.
    pub fn default_path() -> PathBuf {
        if let Some(p) = std::env::var_os("SMOOTH_ATTEST_LEDGER").filter(|p| !p.is_empty()) {
            return PathBuf::from(p);
        }
        dirs_next::home_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(".smooth")
            .join("attest")
            .join("credits.db")
    }

    fn open(&self) -> Result<Connection> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let c = Connection::open(&self.path).with_context(|| format!("opening {}", self.path.display()))?;
        c.busy_timeout(Duration::from_secs(5))?;
        c.pragma_update(None, "journal_mode", "WAL")?;
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS passes (
                repo TEXT NOT NULL,
                check_name TEXT NOT NULL,
                sha TEXT NOT NULL,
                base TEXT NOT NULL,
                secs INTEGER NOT NULL,
                at_ms INTEGER NOT NULL,
                PRIMARY KEY (repo, check_name, sha)
            );
            CREATE INDEX IF NOT EXISTS passes_by_check ON passes(repo, check_name, at_ms);",
        )?;
        Ok(c)
    }

    /// Record a pass. A re-pass of the same commit refreshes its age.
    pub fn record(&self, repo: &str, check: &str, pass: &Pass) -> Result<()> {
        let c = self.open()?;
        c.execute(
            "INSERT INTO passes (repo, check_name, sha, base, secs, at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (repo, check_name, sha) DO UPDATE SET base = excluded.base, secs = excluded.secs, at_ms = excluded.at_ms",
            params![repo, check, pass.sha, pass.base, to_i64(pass.secs), to_i64(pass.at_ms)],
        )?;
        // Rows past the max age can never be reused again.
        let cutoff = now_ms().saturating_sub(millis(self.max_age));
        c.execute("DELETE FROM passes WHERE at_ms < ?1", params![to_i64(cutoff)])?;
        Ok(())
    }

    /// Recent passes of `check` in `repo` still inside the max age, newest first.
    pub fn recent(&self, repo: &str, check: &str) -> Result<Vec<Pass>> {
        let c = self.open()?;
        let cutoff = now_ms().saturating_sub(millis(self.max_age));
        let mut stmt = c.prepare(
            "SELECT sha, base, secs, at_ms FROM passes
             WHERE repo = ?1 AND check_name = ?2 AND at_ms >= ?3
             ORDER BY at_ms DESC LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![repo, check, to_i64(cutoff), to_i64(CANDIDATES as u64)], |r| {
            Ok(Pass {
                sha: r.get(0)?,
                base: r.get(1)?,
                secs: from_i64(r.get(2)?),
                at_ms: from_i64(r.get(3)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// The ledger key for a repo: its shared git dir, so every worktree of one
/// clone shares passes (they share the object store the diffs need, too).
pub fn repo_key(root: &Path) -> Option<String> {
    let out = git(root, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let p = PathBuf::from(out.trim());
    Some(p.canonicalize().unwrap_or(p).to_string_lossy().into_owned())
}

/// `head`'s merge-base with `origin/<default_branch>`, if there is one.
pub fn merge_base(root: &Path, head: &str, default_branch: &str) -> Option<String> {
    git(root, &["merge-base", head, &format!("origin/{default_branch}")]).map(|s| s.trim().to_string())
}

/// Finds passes that carry over to one HEAD. Every check of a run asks about
/// mostly the same earlier commits, so the hook's answer is kept per change set:
/// one hook run per distinct diff, not one per check per candidate.
pub struct Finder<'a> {
    ledger: &'a Ledger,
    root: &'a Path,
    repo: &'a str,
    head: &'a str,
    base: &'a str,
    hook_out: RefCell<HashMap<Vec<String>, Option<String>>>,
}

impl<'a> Finder<'a> {
    pub fn new(ledger: &'a Ledger, root: &'a Path, repo: &'a str, head: &'a str, base: &'a str) -> Self {
        Self {
            ledger,
            root,
            repo,
            head,
            base,
            hook_out: RefCell::new(HashMap::new()),
        }
    }

    /// The most recent recorded pass of `check` that carries over to this HEAD.
    pub fn find(&self, check: &str) -> Option<Reused> {
        let passes = match self.ledger.recent(self.repo, check) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("· credit ledger unreadable ({e:#}) — running {check}");
                return None;
            }
        };
        passes.into_iter().find_map(|p| {
            let changed = changed_between(self.root, &p, self.head, self.base)?;
            self.carries(check, &changed).then_some(Reused { sha: p.sha, secs: p.secs })
        })
    }

    fn carries(&self, check: &str, changed: &[String]) -> bool {
        if let Some(verdict) = carries_without_hook(changed) {
            return verdict;
        }
        let out = self
            .hook_out
            .borrow_mut()
            .entry(changed.to_vec())
            .or_insert_with(|| run_hook(self.root, changed))
            .clone();
        out.is_some_and(|out| parse_verdict(&out, check) == Some(false))
    }
}

/// Every path that differs between the recorded pass and the new commit: head
/// against head, plus base against base. `None` when either diff cannot be
/// computed (a commit gone from the object store, say).
fn changed_between(root: &Path, pass: &Pass, head: &str, base: &str) -> Option<Vec<String>> {
    let mut files = diff_names(root, &pass.sha, head)?;
    if pass.base != base {
        files.extend(diff_names(root, &pass.base, base)?);
    }
    files.sort();
    files.dedup();
    Some(files)
}

fn diff_names(root: &Path, from: &str, to: &str) -> Option<Vec<String>> {
    if from == to {
        return Some(Vec::new());
    }
    let out = git(root, &["diff", "--no-renames", "--name-only", from, to, "--"])?;
    Some(out.lines().filter(|l| !l.is_empty()).map(ToString::to_string).collect())
}

/// The verdicts that need no hook: nothing changed (carries), or the checks
/// themselves changed (never carries). `None` means ask the hook.
fn carries_without_hook(changed: &[String]) -> Option<bool> {
    if changed.is_empty() {
        return Some(true);
    }
    if changed.iter().any(|f| f.starts_with(CHECK_DEFINITIONS)) {
        return Some(false);
    }
    None
}

/// Whether a pass of `check` carries across `changed`, asking the hook directly.
#[cfg(test)]
fn carries(root: &Path, check: &str, changed: &[String]) -> bool {
    carries_without_hook(changed).unwrap_or_else(|| run_hook(root, changed).is_some_and(|out| parse_verdict(&out, check) == Some(false)))
}

/// The hook's stdout for `changed`, or `None` when there is no usable answer:
/// no hook, it could not start, it failed, or it outlived the timeout.
fn run_hook(root: &Path, changed: &[String]) -> Option<String> {
    let hook = root.join(HOOK);
    if !hook.is_file() {
        return None;
    }
    let Ok(mut child) = Command::new("bash")
        .arg(&hook)
        .current_dir(root)
        .env("CI_REPO_ROOT", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return None;
    };
    if let Some(mut stdin) = child.stdin.take() {
        // On its own thread: a hook that never reads would otherwise block this
        // write once the list outgrows the pipe buffer, before the timeout below
        // could fire. A hook that exits early is caught by its exit code.
        let input = format!("{}\n", changed.join("\n"));
        std::thread::spawn(move || drop(stdin.write_all(input.as_bytes())));
    }
    let deadline = Instant::now() + HOOK_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                drop(child.kill());
                drop(child.wait());
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The hook's verdict for `check`: `Some(true)` touched, `Some(false)` untouched,
/// `None` when it does not name the check (or names it ambiguously).
pub fn parse_verdict(out: &str, check: &str) -> Option<bool> {
    let mut verdict = None;
    for line in out.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some(check) {
            continue;
        }
        let this = match (parts.next(), parts.next()) {
            (Some("touched"), None) => true,
            (Some("untouched"), None) => false,
            _ => return None,
        };
        if verdict.is_some_and(|v| v != this) {
            return None;
        }
        verdict = Some(this);
    }
    verdict
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(root).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn from_i64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn pass(sha: &str, at_ms: u64) -> Pass {
        Pass {
            sha: sha.into(),
            base: "b".into(),
            secs: 7,
            at_ms,
        }
    }

    #[test]
    fn verdict_reads_touched_and_untouched() {
        let out = "lint touched\nrust untouched\n";
        assert_eq!(parse_verdict(out, "lint"), Some(true));
        assert_eq!(parse_verdict(out, "rust"), Some(false));
    }

    #[test]
    fn an_unnamed_check_has_no_verdict() {
        assert_eq!(parse_verdict("lint untouched\n", "rust"), None);
        assert_eq!(parse_verdict("", "rust"), None);
    }

    #[test]
    fn a_prefix_is_not_a_name() {
        assert_eq!(parse_verdict("rust-extra untouched\n", "rust"), None);
    }

    #[test]
    fn garbled_or_contradictory_lines_have_no_verdict() {
        assert_eq!(parse_verdict("rust maybe\n", "rust"), None);
        assert_eq!(parse_verdict("rust untouched please\n", "rust"), None);
        assert_eq!(parse_verdict("rust untouched\nrust touched\n", "rust"), None);
    }

    #[test]
    fn a_repeated_agreeing_line_is_fine() {
        assert_eq!(parse_verdict("rust untouched\nrust untouched\n", "rust"), Some(false));
    }

    #[test]
    fn ledger_returns_recent_passes_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let l = Ledger::new(tmp.path().join("c.db"), Duration::from_secs(3600));
        let now = now_ms();
        l.record("r", "lint", &pass("old", now - 2000)).unwrap();
        l.record("r", "lint", &pass("new", now - 1000)).unwrap();
        l.record("r", "rust", &pass("other-check", now)).unwrap();
        l.record("other-repo", "lint", &pass("other-repo", now)).unwrap();
        let got: Vec<String> = l.recent("r", "lint").unwrap().into_iter().map(|p| p.sha).collect();
        assert_eq!(got, ["new", "old"]);
    }

    #[test]
    fn ledger_forgets_passes_past_the_max_age() {
        let tmp = tempfile::tempdir().unwrap();
        let l = Ledger::new(tmp.path().join("c.db"), Duration::from_secs(60));
        let now = now_ms();
        l.record("r", "lint", &pass("stale", now - 120_000)).unwrap();
        l.record("r", "lint", &pass("fresh", now)).unwrap();
        let got: Vec<String> = l.recent("r", "lint").unwrap().into_iter().map(|p| p.sha).collect();
        assert_eq!(got, ["fresh"]);
    }

    #[test]
    fn a_re_pass_refreshes_rather_than_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let l = Ledger::new(tmp.path().join("c.db"), Duration::from_secs(3600));
        let now = now_ms();
        l.record("r", "lint", &pass("s", now - 5000)).unwrap();
        l.record("r", "lint", &pass("s", now)).unwrap();
        let got = l.recent("r", "lint").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].at_ms, now);
    }

    #[test]
    fn an_unopenable_ledger_errors_instead_of_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        // A directory where the db file should be.
        let path = tmp.path().join("c.db");
        std::fs::create_dir_all(&path).unwrap();
        let l = Ledger::new(path, Duration::from_secs(60));
        assert!(l.recent("r", "lint").is_err());
    }

    #[test]
    fn no_change_carries_without_a_hook() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(carries(tmp.path(), "lint", &[]));
    }

    #[test]
    fn a_change_without_a_hook_never_carries() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!carries(tmp.path(), "lint", &["docs/a.md".into()]));
    }

    #[cfg(unix)]
    mod with_hook {
        use super::super::*;
        use crate::attest::env::test_script;

        fn repo_with_hook(body: &str) -> tempfile::TempDir {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(tmp.path().join("scripts/ci")).unwrap();
            test_script(&tmp.path().join("scripts/ci"), "_rows-touched.sh", body);
            tmp
        }

        #[test]
        fn the_hook_sees_every_changed_path_on_stdin() {
            let tmp = repo_with_hook(r#"grep -qx 'docs/a.md' && echo "rust untouched""#);
            assert!(carries(tmp.path(), "rust", &["docs/a.md".into()]));
        }

        #[test]
        fn untouched_carries_and_touched_does_not() {
            let tmp = repo_with_hook("cat >/dev/null; echo 'rust untouched'; echo 'lint touched'");
            assert!(carries(tmp.path(), "rust", &["apps/web/x.ts".into()]));
            assert!(!carries(tmp.path(), "lint", &["apps/web/x.ts".into()]));
        }

        #[test]
        fn a_check_definition_change_never_carries_whatever_the_hook_says() {
            let tmp = repo_with_hook("cat >/dev/null; echo 'rust untouched'");
            assert!(!carries(tmp.path(), "rust", &["scripts/ci/_affected.mjs".into()]));
        }

        #[test]
        fn a_hook_that_never_reads_a_huge_list_times_out_instead_of_hanging() {
            let tmp = repo_with_hook("sleep 30");
            let changed: Vec<String> = (0..20_000).map(|i| format!("docs/file-{i}.md")).collect();
            let began = Instant::now();
            assert!(!carries(tmp.path(), "rust", &changed));
            assert!(began.elapsed() < HOOK_TIMEOUT + Duration::from_secs(5));
        }

        #[test]
        fn a_failing_hook_never_carries() {
            let tmp = repo_with_hook("cat >/dev/null; echo 'rust untouched'; exit 1");
            assert!(!carries(tmp.path(), "rust", &["docs/a.md".into()]));
        }

        #[test]
        fn the_hook_runs_once_per_change_set_across_checks() {
            let tmp = repo_with_hook("cat >/dev/null; echo x >> \"$CI_REPO_ROOT/hook.runs\"; echo 'rust untouched'; echo 'lint touched'");
            let ledger = Ledger::new(tmp.path().join("c.db"), Duration::from_secs(60));
            let f = Finder::new(&ledger, tmp.path(), "r", "h", "b");
            let changed = vec!["docs/a.md".to_string()];
            assert!(f.carries("rust", &changed));
            assert!(!f.carries("lint", &changed));
            assert!(f.carries("rust", &changed));
            let runs = std::fs::read_to_string(tmp.path().join("hook.runs")).unwrap();
            assert_eq!(runs.lines().count(), 1);
        }

        #[test]
        fn a_hook_that_does_not_name_the_check_never_carries() {
            let tmp = repo_with_hook("cat >/dev/null; echo 'lint untouched'");
            assert!(!carries(tmp.path(), "rust", &["docs/a.md".into()]));
        }
    }
}
