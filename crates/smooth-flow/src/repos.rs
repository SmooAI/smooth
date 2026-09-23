//! The repo index behind the New Session directory picker (th-145e6b).
//!
//! Starting a session means choosing a directory, and nearly always that
//! directory is a git checkout somewhere under `$HOME`. The dialog used to
//! offer only the focused session's worktree plus a hidden free-text path.
//! This module finds every git repo and worktree under a root, so the picker
//! can search them by typing.
//!
//! **Fast like `fd`.** The walk uses the `ignore` crate's parallel walker,
//! the engine inside `fd`, with the pruning that makes a whole-home walk cheap:
//! - it stops descending at a repo root (a repo's own tree is not searched for
//!   more repos);
//! - it skips hidden directories, and build and dependency trees
//!   (`node_modules`, `target`, …);
//! - at the top level it skips `~/Library`, media folders and cloud-synced
//!   folders, where walking would make macOS download files.
//!
//! The result lives in flow.db's `repos` table, so a restarted daemon
//! answers from the last scan at once while a fresh one runs.
//!
//! **Worktrees are first-class.** A linked worktree (`.git` is a file) is
//! listed under its own path with the main checkout it belongs to, because
//! that is where agents actually work. The branch comes from reading `HEAD`,
//! never by running `git`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// One git checkout the picker can offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    /// Absolute path of the working tree.
    pub path: String,
    /// The directory's name, the part people type.
    pub name: String,
    /// The checked-out branch, `None` when `HEAD` is detached or unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// For a linked worktree: the main checkout it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    /// Unix seconds of the last checkout, commit or stage (the git index's
    /// mtime, else `HEAD`'s). The recency signal for an empty query.
    #[serde(default)]
    pub touched: i64,
}

/// How deep below the root a repo can be found. `~/dev/org/repo` is 3, and
/// 6 leaves room for deeper layouts without walking forever.
pub const MAX_DEPTH: usize = 6;

/// Directory names never descended into, at any depth: dependency, build
/// and tool trees that are huge and never hold a checkout someone works in.
const SKIP_ANYWHERE: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "Pods",
    "DerivedData",
    "__pycache__",
    "venv",
    "site-packages",
    "bower_components",
];

/// Top-level entries of `$HOME` never descended into. The media and
/// `Library` trees are huge and hold no checkouts. The cloud folders would
/// make macOS pull down every placeholder file the walk touched.
const SKIP_AT_ROOT: &[&str] = &["Library", "Applications", "Movies", "Music", "Pictures", "Public", "OrbStack", "Dropbox", "mnt"];

/// Whether the walk should not descend into `path` (a directory at `depth`
/// below the root).
fn skip_dir(path: &Path, depth: usize) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return true };
    if depth == 0 {
        return false;
    }
    if name.starts_with('.') || SKIP_ANYWHERE.contains(&name) {
        return true;
    }
    depth == 1 && (SKIP_AT_ROOT.contains(&name) || name.contains("Google Drive") || name.contains("OneDrive") || name.contains("iCloud"))
}

/// `Some` when `dir` is a git working tree: a `.git` directory (a main
/// checkout) or a `.git` file pointing at a worktree's git dir.
#[must_use]
pub fn read_repo(dir: &Path) -> Option<Repo> {
    let dot_git = dir.join(".git");
    let meta = std::fs::symlink_metadata(&dot_git).ok()?;
    let (git_dir, main) = if meta.is_dir() {
        (dot_git, None)
    } else if meta.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
        let git_dir = if Path::new(target).is_absolute() {
            PathBuf::from(target)
        } else {
            dir.join(target)
        };
        (git_dir.clone(), main_checkout_of(&git_dir))
    } else {
        return None;
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok();
    let branch = head.as_deref().and_then(|h| h.trim().strip_prefix("ref: refs/heads/")).map(str::to_string);
    let touched = [git_dir.join("index"), git_dir.join("HEAD")]
        .iter()
        .find_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    Some(Repo {
        path: dir.to_string_lossy().into_owned(),
        name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        branch,
        main: main.map(|m| m.to_string_lossy().into_owned()),
        touched,
    })
}

/// A linked worktree's git dir is `<main>/.git/worktrees/<name>`; its main
/// checkout is `<main>`. A submodule's (`<super>/.git/modules/<name>`) is not
/// a worktree, so it has no main.
fn main_checkout_of(git_dir: &Path) -> Option<PathBuf> {
    let worktrees = git_dir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let dot_git = worktrees.parent()?;
    (dot_git.file_name()? == ".git").then(|| dot_git.parent().map(Path::to_path_buf))?
}

/// Every git working tree under `root`, sorted by path. `root` itself is
/// never one, so a dotfiles repo in `$HOME` does not hide everything else.
#[must_use]
pub fn scan(root: &Path, max_depth: usize) -> Vec<Repo> {
    let found = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(2, 8));
    ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .follow_links(false)
        .max_depth(Some(max_depth))
        .threads(threads)
        .filter_entry(|e| !(e.file_type().is_some_and(|t| t.is_dir()) && skip_dir(e.path(), e.depth())))
        .build_parallel()
        .run(|| {
            Box::new(|entry| {
                let Ok(entry) = entry else { return ignore::WalkState::Continue };
                if entry.depth() == 0 || !entry.file_type().is_some_and(|t| t.is_dir()) {
                    return ignore::WalkState::Continue;
                }
                match read_repo(entry.path()) {
                    Some(repo) => {
                        found.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(repo);
                        // A repo's own tree is not searched for more repos.
                        ignore::WalkState::Skip
                    }
                    None => ignore::WalkState::Continue,
                }
            })
        });
    let mut repos = found.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
    repos.sort_by(|a, b| a.path.cmp(&b.path));
    repos
}

/// How well `needle` (lowercase) matches `hay` (lowercase) as a
/// subsequence: `None` when it doesn't, else a score that favors contiguous
/// runs.
fn subsequence(hay: &str, needle: &str) -> Option<i64> {
    let mut score = 0;
    let mut run = 0;
    let mut chars = hay.chars();
    for n in needle.chars() {
        let mut hit = false;
        for h in chars.by_ref() {
            if h == n {
                run += 1;
                score += run;
                hit = true;
                break;
            }
            run = 0;
        }
        if !hit {
            return None;
        }
    }
    Some(score)
}

/// Score one repo against one lowercase query token, `None` for no match.
fn token_score(repo: &Repo, name: &str, path: &str, token: &str) -> Option<i64> {
    if name == token {
        Some(1000)
    } else if name.starts_with(token) {
        Some(600)
    } else if name.contains(token) {
        Some(400)
    } else if path.contains(token) {
        Some(200)
    } else {
        // A worktree's branch is often what someone remembers.
        let branch = repo.branch.as_deref().unwrap_or_default().to_lowercase();
        if branch.contains(token) {
            return Some(150);
        }
        subsequence(path, token).map(|s| s.min(100))
    }
}

/// The picker's answer: repos matching every whitespace-separated token of
/// `query` (case-insensitive), best first, at most `limit`.
///
/// The ranking: an exact name, then a name prefix, then a name substring,
/// then a path substring, then the branch, then a fuzzy subsequence of the
/// path. Paths in `preferred` (where the fleet already works) and recently
/// touched checkouts break ties. An empty query is simply `preferred`
/// first, then most recently touched.
#[must_use]
pub fn rank<'a>(repos: &'a [Repo], query: &str, preferred: &HashSet<String>, limit: usize) -> Vec<&'a Repo> {
    let tokens: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut scored: Vec<(i64, &Repo)> = repos
        .iter()
        .filter_map(|r| {
            let name = r.name.to_lowercase();
            let path = r.path.to_lowercase();
            let mut total = 0;
            for t in &tokens {
                total += token_score(r, &name, &path, t)?;
            }
            if preferred.contains(&r.path) {
                total += 300;
            }
            Some((total, r))
        })
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then(b.touched.cmp(&a.touched)).then(a.path.cmp(&b.path)));
    scored.into_iter().take(limit).map(|(_, r)| r).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn git_repo(dir: &Path, branch: &str) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
    }

    fn worktree(main: &Path, dir: &Path, name: &str, branch: &str) {
        let gd = main.join(".git/worktrees").join(name);
        std::fs::create_dir_all(&gd).unwrap();
        std::fs::write(gd.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(".git"), format!("gitdir: {}\n", gd.display())).unwrap();
    }

    fn names(v: &[&Repo]) -> Vec<String> {
        v.iter().map(|r| r.name.clone()).collect()
    }

    #[test]
    fn scan_finds_repos_and_worktrees_and_prunes_what_it_should() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        git_repo(&home.join("dev/smooai/smooth"), "main");
        worktree(
            &home.join("dev/smooai/smooth"),
            &home.join("dev/smooai/smooth-th-1-fix"),
            "smooth-th-1-fix",
            "th-1-fix",
        );
        git_repo(&home.join("dev/smooai/smooai"), "main");
        // Inside a repo: not a separate hit, the walk stops at the root.
        git_repo(&home.join("dev/smooai/smooth/vendored/inner"), "main");
        // Pruned trees.
        git_repo(&home.join("dev/app/node_modules/dep"), "main");
        git_repo(&home.join(".hidden/repo"), "main");
        git_repo(&home.join("Library/Caches/repo"), "main");
        git_repo(&home.join("me - Google Drive/repo"), "main");
        // Too deep.
        git_repo(&home.join("a/b/c/d/e/f/g/deep"), "main");
        // A detached HEAD.
        std::fs::create_dir_all(home.join("dev/detached/.git")).unwrap();
        std::fs::write(home.join("dev/detached/.git/HEAD"), "0123456789abcdef\n").unwrap();
        // The root itself is never a hit (a dotfiles repo in $HOME).
        git_repo(home, "dotfiles");

        let repos = scan(home, MAX_DEPTH);
        let found: Vec<&str> = repos.iter().map(|r| r.path.strip_prefix(&*home.to_string_lossy()).unwrap()).collect();
        assert_eq!(
            found,
            vec!["/dev/detached", "/dev/smooai/smooai", "/dev/smooai/smooth", "/dev/smooai/smooth-th-1-fix"]
        );

        let wt = repos.iter().find(|r| r.name == "smooth-th-1-fix").unwrap();
        assert_eq!(wt.branch.as_deref(), Some("th-1-fix"));
        assert_eq!(wt.main.as_deref(), Some(&*home.join("dev/smooai/smooth").to_string_lossy()));
        let main = repos.iter().find(|r| r.name == "smooth").unwrap();
        assert_eq!((main.branch.as_deref(), main.main.as_deref()), (Some("main"), None));
        assert_eq!(repos.iter().find(|r| r.name == "detached").unwrap().branch, None);
        assert!(main.touched > 0, "HEAD's mtime is the recency fallback");
    }

    #[test]
    fn a_submodule_git_dir_is_not_a_worktree() {
        assert_eq!(main_checkout_of(Path::new("/r/.git/modules/sub")), None);
        assert_eq!(main_checkout_of(Path::new("/r/.git/worktrees/wt")), Some(PathBuf::from("/r")));
        assert_eq!(main_checkout_of(Path::new("/r/elsewhere/worktrees/wt")), None);
    }

    fn repo(path: &str, branch: &str, touched: i64) -> Repo {
        Repo {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            branch: Some(branch.into()),
            main: None,
            touched,
        }
    }

    #[test]
    fn rank_prefers_names_then_paths_then_fuzzy_and_needs_every_token() {
        let repos = vec![
            repo("/h/dev/smooai/smooth", "main", 10),
            repo("/h/dev/smooai/smooai", "main", 20),
            repo("/h/dev/smooai/smooth-th-1-relay", "th-1-relay", 30),
            repo("/h/dev/other/smoothie", "main", 40),
            repo("/h/dev/refs/cmux", "main", 5),
        ];
        let none = HashSet::new();
        assert_eq!(
            names(&rank(&repos, "smooth", &none, 10))[..3],
            ["smooth", "smoothie", "smooth-th-1-relay"],
            "exact, then prefix by recency"
        );
        assert_eq!(names(&rank(&repos, "relay", &none, 10)), ["smooth-th-1-relay"]);
        assert_eq!(names(&rank(&repos, "refs", &none, 10)), ["cmux"], "a path segment");
        assert_eq!(names(&rank(&repos, "SMOOAI smooth", &none, 10))[0], "smooth", "every token, case-insensitive");
        assert!(rank(&repos, "smooth nonsense", &none, 10).is_empty());
        assert_eq!(names(&rank(&repos, "dvcmu", &none, 10)), ["cmux"], "fuzzy subsequence of the path");
        assert_eq!(rank(&repos, "", &none, 2).len(), 2, "limit");
        assert_eq!(names(&rank(&repos, "", &none, 10))[0], "smoothie", "empty query: most recent first");
        let fleet: HashSet<String> = ["/h/dev/refs/cmux".to_string()].into();
        assert_eq!(names(&rank(&repos, "", &fleet, 10))[0], "cmux", "where the fleet works comes first");
    }
}
