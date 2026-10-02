//! The git plumbing behind structured diffs: worktree snapshots through a
//! throwaway index, tree-to-tree diffs, and single-hunk `git apply`.
//!
//! **Snapshots never touch the user's state.** The user's index is COPIED to
//! a temp file (so git's stat cache makes `add -A` cheap), `GIT_INDEX_FILE`
//! points git at the copy, and `git write-tree` turns it into a tree id. The
//! real index, the stash, refs and the worktree are never written; the only
//! side effect is new objects in the object store (blobs for changed files,
//! trees for changed directories), which git's normal gc prunes once nothing
//! references them (after `gc.pruneExpire`, two weeks by default).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};

/// An untracked file bigger than this is left out of a snapshot (and so out
/// of the diff): hashing a stray multi-GB artifact into the object store on
/// every turn is not a review tool's call to make.
pub const SNAPSHOT_MAX_UNTRACKED_BYTES: u64 = 20 * 1024 * 1024;

/// The most `git diff` output read before the rest is dropped (and the diff
/// marked truncated).
pub const MAX_PATCH_BYTES: usize = 64 * 1024 * 1024;

/// Refs tried in order for the `branch` base (same list as the old Mac
/// `DiffPlan`).
pub const DEFAULT_BRANCH_CANDIDATES: [&str; 5] = ["origin/HEAD", "origin/main", "origin/master", "main", "master"];

/// A `git` command in `cwd` with the caller's git environment scrubbed, so a
/// daemon launched from inside some other repo's hook can't redirect us.
pub fn git_cmd(cwd: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_PREFIX")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    c
}

/// Run git, return trimmed stdout, or an error carrying stderr.
///
/// # Errors
/// When git cannot run or exits non-zero.
pub fn run(cwd: &Path, args: &[&str]) -> Result<String> {
    run_env(cwd, args, &[])
}

fn run_env(cwd: &Path, args: &[&str], env: &[(&str, &Path)]) -> Result<String> {
    let mut c = git_cmd(cwd);
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().with_context(|| format!("git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// True when `dir` is inside a git worktree. A cheap ancestor walk for a
/// `.git` entry first, so a non-repo costs no process spawn.
#[must_use]
pub fn is_repo(dir: &Path) -> bool {
    dir.ancestors().any(|d| d.join(".git").exists())
}

/// The toplevel of the worktree containing `dir`.
///
/// # Errors
/// Outside a repo.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(run(dir, &["rev-parse", "--show-toplevel"])?))
}

/// The tree id of git's empty tree for this repo's hash algorithm.
///
/// # Errors
/// When git refuses.
pub fn empty_tree(dir: &Path) -> Result<String> {
    let mut c = git_cmd(dir);
    c.args(["hash-object", "-t", "tree", "--stdin"]).stdin(Stdio::piped()).stdout(Stdio::piped());
    let child = c.spawn().context("git hash-object")?;
    // Dropping stdin sends EOF: the empty tree is the hash of nothing.
    let out = child.wait_with_output().context("git hash-object")?;
    if !out.status.success() {
        bail!("git hash-object failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `HEAD`'s tree, or `None` in a repo with no commits yet.
#[must_use]
pub fn head_tree(dir: &Path) -> Option<String> {
    run(dir, &["rev-parse", "--verify", "--quiet", "HEAD^{tree}"]).ok().filter(|s| !s.is_empty())
}

/// The merge base of HEAD with the first default-branch candidate that has
/// one: `(ref, commit)`.
#[must_use]
pub fn branch_base(dir: &Path) -> Option<(String, String)> {
    for r in DEFAULT_BRANCH_CANDIDATES {
        if let Ok(sha) = run(dir, &["merge-base", "HEAD", r]) {
            if !sha.is_empty() {
                return Some((r.to_string(), sha));
            }
        }
    }
    None
}

/// The user's index file for this worktree (linked worktrees have their own).
fn index_path(top: &Path) -> Result<PathBuf> {
    let p = PathBuf::from(run(top, &["rev-parse", "--git-path", "index"])?);
    Ok(if p.is_absolute() { p } else { top.join(p) })
}

/// Untracked, not-ignored files over the size cap — excluded from snapshots.
fn oversized_untracked(top: &Path) -> Vec<String> {
    let Ok(out) = git_cmd(top).args(["ls-files", "--others", "--exclude-standard", "-z"]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
        .filter(|p| std::fs::metadata(top.join(p)).is_ok_and(|m| m.len() > SNAPSHOT_MAX_UNTRACKED_BYTES))
        .map(str::to_string)
        .collect()
}

/// Snapshot the worktree containing `dir` — tracked AND untracked files,
/// ignored files excluded — as a tree id, without touching the user's index.
///
/// # Errors
/// Outside a repo, or when git refuses.
pub fn snapshot_tree(dir: &Path) -> Result<String> {
    let top = toplevel(dir)?;
    let tmp = tempfile::Builder::new().prefix("smoothflow-snap-").tempdir().context("snapshot temp dir")?;
    let tmp_index = tmp.path().join("index");
    let real = index_path(&top)?;
    if real.exists() {
        // A copy, never the file: git rewrites the index it is pointed at.
        std::fs::copy(&real, &tmp_index).with_context(|| format!("copy {}", real.display()))?;
    }
    let excluded: Vec<String> = oversized_untracked(&top).into_iter().map(|p| format!(":(exclude,literal){p}")).collect();
    let mut add: Vec<&str> = vec!["add", "--all", "--", "."];
    add.extend(excluded.iter().map(String::as_str));
    run_env(&top, &add, &[("GIT_INDEX_FILE", tmp_index.as_path())])?;
    let tree = run_env(&top, &["write-tree"], &[("GIT_INDEX_FILE", tmp_index.as_path())])?;
    if tree.is_empty() {
        bail!("git write-tree printed nothing");
    }
    Ok(tree)
}

/// The tree of the user's index, without writing the index (a copy is
/// written instead — `write-tree` updates the cache-tree of the file it reads).
///
/// # Errors
/// Outside a repo, or when git refuses.
pub fn index_tree(dir: &Path) -> Result<String> {
    let top = toplevel(dir)?;
    let tmp = tempfile::Builder::new().prefix("smoothflow-idx-").tempdir().context("index temp dir")?;
    let tmp_index = tmp.path().join("index");
    let real = index_path(&top)?;
    if !real.exists() {
        return head_tree(&top).map_or_else(|| empty_tree(&top), Ok);
    }
    std::fs::copy(&real, &tmp_index).with_context(|| format!("copy {}", real.display()))?;
    run_env(&top, &["write-tree"], &[("GIT_INDEX_FILE", tmp_index.as_path())])
}

/// True when `id` names an object this repo still has.
#[must_use]
pub fn object_exists(dir: &Path, id: &str) -> bool {
    run(dir, &["cat-file", "-e", id]).is_ok()
}

/// `git diff` between two trees or commits.
///
/// Unified, rename-aware, no color, no external diff or textconv, fixed
/// `a/`/`b/` prefixes regardless of the user's config. Returns the output (lossy UTF-8) and whether it was cut
/// at [`MAX_PATCH_BYTES`].
///
/// # Errors
/// When git cannot run or refuses.
pub fn diff_trees(dir: &Path, from: &str, to: &str) -> Result<(String, bool)> {
    let mut child = git_cmd(dir)
        .args([
            "-c",
            "core.quotepath=off",
            "-c",
            "diff.noprefix=false",
            "-c",
            "diff.mnemonicPrefix=false",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            "--unified=3",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            from,
            to,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("git diff")?;
    let mut buf = Vec::new();
    let mut truncated = false;
    if let Some(out) = child.stdout.take() {
        let limit = u64::try_from(MAX_PATCH_BYTES).unwrap_or(u64::MAX);
        out.take(limit + 1).read_to_end(&mut buf).context("read git diff")?;
        if buf.len() > MAX_PATCH_BYTES {
            truncated = true;
            buf.truncate(MAX_PATCH_BYTES);
            // Cut at the last complete file so no half hunk is parsed.
            if let Some(i) = find_last(&buf, b"\ndiff --git ") {
                buf.truncate(i + 1);
            }
            let _ = child.kill();
        }
    }
    let status = child.wait().context("git diff")?;
    if !truncated && !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        return Err(anyhow!("git diff {from} {to} failed: {}", err.trim()));
    }
    Ok((String::from_utf8_lossy(&buf).into_owned(), truncated))
}

fn find_last(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// Where a single-hunk patch is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyTarget {
    /// Reverse the hunk in the worktree files (revert).
    WorktreeReverse,
    /// Apply the hunk to the index (stage).
    Index,
    /// Reverse the hunk in the index (unstage).
    IndexReverse,
}

/// `git apply` one generated patch. Checks first (`--check`), so a hunk that
/// no longer applies is refused with nothing written.
///
/// # Errors
/// `stale:` prefixed when the hunk no longer applies.
pub fn apply(dir: &Path, patch: &str, target: ApplyTarget) -> Result<()> {
    let top = toplevel(dir)?;
    let flags: &[&str] = match target {
        ApplyTarget::WorktreeReverse => &["--reverse"],
        ApplyTarget::Index => &["--cached"],
        ApplyTarget::IndexReverse => &["--cached", "--reverse"],
    };
    for check in [true, false] {
        let mut c = git_cmd(&top);
        c.arg("apply").args(flags).arg("--whitespace=nowarn");
        if check {
            c.arg("--check");
        }
        c.arg("-").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().context("git apply")?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write as _;
            stdin.write_all(patch.as_bytes()).context("write patch")?;
        }
        let out = child.wait_with_output().context("git apply")?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if check {
                bail!("stale: the hunk no longer applies ({err})");
            }
            bail!("git apply failed: {err}");
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
pub mod tests {
    use super::*;

    /// A temp repo with one commit of `a.txt` (`one\ntwo\nthree\n`).
    pub fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
            // Windows runners default to autocrlf=true; the tests compare bytes.
            vec!["config", "core.autocrlf", "false"],
        ] {
            run(p, &args).unwrap();
        }
        std::fs::write(p.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        run(p, &["add", "a.txt"]).unwrap();
        run(p, &["commit", "-q", "-m", "init"]).unwrap();
        d
    }

    fn index_bytes(p: &Path) -> Vec<u8> {
        std::fs::read(p.join(".git/index")).unwrap()
    }

    #[test]
    fn snapshot_includes_untracked_and_never_touches_index_or_stash() {
        let d = repo();
        let p = d.path();
        std::fs::write(p.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(p.join("new.txt"), "fresh\n").unwrap();
        std::fs::write(p.join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(p.join("ignored.log"), "noise\n").unwrap();
        // A partially staged file and a stash entry the snapshot must leave alone.
        std::fs::write(p.join("staged.txt"), "s\n").unwrap();
        run(p, &["add", "staged.txt"]).unwrap();
        let before = index_bytes(p);
        let stash_before = run(p, &["stash", "list"]).unwrap();
        let tree = snapshot_tree(p).unwrap();
        assert_eq!(index_bytes(p), before, "the user's index is byte-identical");
        assert_eq!(run(p, &["stash", "list"]).unwrap(), stash_before);
        assert_eq!(run(p, &["status", "--porcelain"]).unwrap().lines().count(), 4, "worktree untouched");
        let names = run(p, &["ls-tree", "-r", "--name-only", &tree]).unwrap();
        assert!(names.contains("new.txt") && names.contains("a.txt") && names.contains("staged.txt"), "{names}");
        assert!(!names.contains("ignored.log"), "ignored files stay out: {names}");
        assert_eq!(run(p, &["show", &format!("{tree}:a.txt")]).unwrap(), "one\nTWO\nthree");
        // Snapshotting twice with no change is the same tree.
        assert_eq!(snapshot_tree(p).unwrap(), tree);
        assert_eq!(index_bytes(p), before);
    }

    #[test]
    fn snapshot_in_an_empty_repo_and_a_subdir() {
        let d = tempfile::tempdir().unwrap();
        run(d.path(), &["init", "-q"]).unwrap();
        assert!(head_tree(d.path()).is_none());
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/x.txt"), "x\n").unwrap();
        let tree = snapshot_tree(&d.path().join("sub")).unwrap();
        assert!(run(d.path(), &["ls-tree", "-r", "--name-only", &tree]).unwrap().contains("sub/x.txt"));
        let empty = empty_tree(d.path()).unwrap();
        assert_eq!(empty.len(), 40);
        assert!(is_repo(&d.path().join("sub")));
        assert!(!is_repo(Path::new("/")));
    }

    #[test]
    fn index_tree_reads_without_writing() {
        let d = repo();
        let p = d.path();
        std::fs::write(p.join("b.txt"), "b\n").unwrap();
        run(p, &["add", "b.txt"]).unwrap();
        let before = index_bytes(p);
        let t = index_tree(p).unwrap();
        assert_eq!(index_bytes(p), before);
        assert!(run(p, &["ls-tree", "--name-only", &t]).unwrap().contains("b.txt"));
    }

    #[test]
    fn diff_trees_is_config_proof() {
        let d = repo();
        let p = d.path();
        run(p, &["config", "diff.noprefix", "true"]).unwrap();
        run(p, &["config", "color.diff", "always"]).unwrap();
        std::fs::write(p.join("a.txt"), "one\n2\nthree\n").unwrap();
        let head = head_tree(p).unwrap();
        let now = snapshot_tree(p).unwrap();
        let (out, truncated) = diff_trees(p, &head, &now).unwrap();
        assert!(!truncated);
        assert!(out.contains("--- a/a.txt\n+++ b/a.txt\n"), "{out}");
        assert!(!out.contains('\u{1b}'), "no color: {out}");
    }

    #[test]
    fn branch_base_falls_back_through_candidates() {
        let d = repo();
        let (r, sha) = branch_base(d.path()).unwrap();
        assert_eq!(r, "main");
        assert_eq!(sha.len(), 40);
    }
}
