//! Zero-friction context inference (th-c103c1).
//!
//! Starting a session should require nothing but pressing Start: the pearl,
//! the Jira key, the worktree, the project and a title are DISCOVERED from a
//! directory, never demanded. The same inference serves two callers:
//!
//! * `GET /api/flow/infer` — what the New Session dialog shows as read-only
//!   context (with a disclosure for explicit overrides).
//! * [`crate::engine::Engine::hook`] — adoption: a `claude` or `codex` started
//!   in a plain terminal posts a hook carrying its `cwd`, and the engine
//!   builds a session row out of exactly these facts.
//!
//! [`infer`] is a pure function over explicit facts so every branch is
//! testable without a repo on disk; [`gather`] is the thin impure shell that
//! runs `git` and `th pearls`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Git facts about a directory. Every field is independently absent — a
/// non-git directory has none, a detached HEAD has no `branch`, a bare repo
/// has no `toplevel`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitFacts {
    /// `git rev-parse --show-toplevel`: the worktree root.
    pub toplevel: Option<PathBuf>,
    /// `git rev-parse --path-format=absolute --git-common-dir`: the SHARED
    /// git dir, whose parent is the main checkout even inside a linked
    /// worktree (the pearls rule).
    pub common_dir: Option<PathBuf>,
    /// `git rev-parse --abbrev-ref HEAD`, `None` when detached or unborn.
    pub branch: Option<String>,
}

/// What the pearl store knows about the pearl for this directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PearlFacts {
    /// A pearl the store itself associates with this worktree/branch — it
    /// wins over anything parsed out of a name.
    pub id: Option<String>,
    pub title: Option<String>,
    /// Free text (title + description) scanned for a Jira key.
    pub text: Option<String>,
}

/// Where a pearl id came from — the UI says this out loud so a wrong guess is
/// visibly a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PearlSource {
    /// The pearl store confirmed the id, or named the pearl itself.
    Store,
    /// Parsed off the front of the branch name.
    Branch,
    /// Parsed off the front of the worktree directory name.
    Worktree,
}

impl PearlSource {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::Branch => "branch",
            Self::Worktree => "worktree",
        }
    }
}

/// Everything the New Session dialog would otherwise ask for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inferred {
    /// The directory the inference started from.
    pub cwd: String,
    /// Where a session would run: the worktree root, else `cwd` itself.
    pub worktree: String,
    /// The main checkout (git-common-dir's parent) — correct inside a linked
    /// worktree, which is the whole point.
    pub project: String,
    /// False outside a git repo: the caller may still start a session here,
    /// but adoption refuses it.
    pub is_git: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pearl_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pearl_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pearl_source: Option<PearlSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jira_key: Option<String>,
    /// Pearl title, else branch, else the directory name — never empty.
    pub title: String,
}

/// A pearl id: `xx-hhhhhh` at the START of `name`.
///
/// Branches are named `<pearl>-<slug>`, and anchoring keeps `fix-abc123` from
/// reading as a pearl.
#[must_use]
pub fn pearl_id_prefix(name: &str) -> Option<String> {
    let re = regex::Regex::new(r"^([a-z]{1,8}-[0-9a-f]{6})(?:-|$)").ok()?;
    re.captures(name).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
}

/// A pearl id as a whole dash-delimited run anywhere in `name`.
///
/// Worktree directories are `<repo>-<pearl>-<slug>`
/// (`smooth-th-c103c1-zero-friction`), so the id is never at the front — but
/// it is always dash-delimited.
#[must_use]
pub fn pearl_id_in(name: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?:^|-)([a-z]{1,8}-[0-9a-f]{6})(?:-|$)").ok()?;
    re.captures(name).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
}

/// Branch names that say nothing about the work: they never become a title.
const GENERIC_BRANCHES: &[&str] = &["main", "master", "trunk", "develop", "dev"];

/// The first Jira key in `text`. Case-sensitive on purpose: `SMOODEV-3125` is
/// a key, `zero-friction-2` is not.
#[must_use]
pub fn jira_key(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"\b([A-Z][A-Z0-9]{1,9}-[0-9]{1,6})\b").ok()?;
    re.captures(text).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
}

/// The last path component of `p` as a plain string (empty for `/`).
fn dir_name(p: &Path) -> String {
    p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Infer a session context from `cwd` and the facts gathered about it.
///
/// Resolution, each field independently:
/// * `worktree` — git toplevel, else `cwd`.
/// * `project` — git-common-dir's parent, else the worktree.
/// * `pearl_id` — the store's association, else the branch prefix, else the
///   worktree directory prefix.
/// * `jira_key` — the branch, else the worktree name, else the pearl's text.
/// * `title` — the pearl title, else the branch, else the directory name,
///   else `cwd` itself (never empty).
#[must_use]
pub fn infer(cwd: &Path, git: &GitFacts, pearl: &PearlFacts) -> Inferred {
    let worktree = git.toplevel.clone().unwrap_or_else(|| cwd.to_path_buf());
    let project = git
        .common_dir
        .as_deref()
        .and_then(Path::parent)
        .map_or_else(|| worktree.clone(), Path::to_path_buf);
    let wt_name = dir_name(&worktree);
    let branch = git.branch.as_deref().map(str::trim).filter(|b| !b.is_empty() && *b != "HEAD");

    let stored = pearl.id.as_deref().map(str::trim).filter(|p| !p.is_empty());
    let (pearl_id, pearl_source) = match stored {
        Some(id) => (Some(id.to_string()), Some(PearlSource::Store)),
        None => branch
            .and_then(pearl_id_prefix)
            .map(|id| (Some(id), Some(PearlSource::Branch)))
            .or_else(|| pearl_id_in(&wt_name).map(|id| (Some(id), Some(PearlSource::Worktree))))
            .unwrap_or((None, None)),
    };
    let pearl_title = pearl.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
    let jira_key = branch
        .and_then(jira_key)
        .or_else(|| jira_key(&wt_name))
        .or_else(|| pearl.text.as_deref().and_then(jira_key));

    let title = pearl_title
        .clone()
        .or_else(|| {
            branch
                .filter(|b| !GENERIC_BRANCHES.contains(&b.to_ascii_lowercase().as_str()))
                .map(str::to_string)
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| if wt_name.is_empty() { cwd.to_string_lossy().into_owned() } else { wt_name });

    Inferred {
        cwd: cwd.to_string_lossy().into_owned(),
        worktree: worktree.to_string_lossy().into_owned(),
        project: project.to_string_lossy().into_owned(),
        is_git: git.toplevel.is_some() || git.common_dir.is_some(),
        branch: branch.map(str::to_string),
        pearl_id,
        pearl_title,
        pearl_source,
        jira_key,
        title,
    }
}

/// A command runner: `(cwd, args) -> stdout`, `None` when it fails or is
/// missing. Injected so [`gather_with`] is testable without git or `th`.
pub type Run<'a> = &'a dyn Fn(&Path, &[&str]) -> Option<String>;

/// Gather the facts for `cwd` by shelling out to `git` and `th pearls`, then
/// [`infer`]. Never fails: a missing binary or a non-repo directory just
/// yields fewer facts.
#[must_use]
pub fn gather(cwd: &Path) -> Inferred {
    gather_with(cwd, &run_git, &run_th)
}

/// [`gather`] with injected runners.
#[must_use]
pub fn gather_with(cwd: &Path, git: Run<'_>, th: Run<'_>) -> Inferred {
    let facts = git_facts(cwd, git);
    let worktree = facts.toplevel.clone().unwrap_or_else(|| cwd.to_path_buf());
    let seed = infer(cwd, &facts, &PearlFacts::default());
    let pearl = pearl_facts(&worktree, seed.pearl_id.as_deref(), th);
    infer(cwd, &facts, &pearl)
}

/// The three `git rev-parse` reads, each independently optional.
fn git_facts(cwd: &Path, git: Run<'_>) -> GitFacts {
    let trimmed = |s: String| {
        let t = s.trim().to_string();
        (!t.is_empty()).then_some(t)
    };
    GitFacts {
        toplevel: git(cwd, &["rev-parse", "--show-toplevel"]).and_then(trimmed).map(PathBuf::from),
        common_dir: git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .and_then(trimmed)
            .map(PathBuf::from),
        branch: git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).and_then(trimmed),
    }
}

/// Resolve pearl facts: confirm `candidate` against the store, else ask the
/// store for the in-progress pearl of this worktree (only when there is
/// exactly ONE — an ambiguous answer is no answer).
fn pearl_facts(worktree: &Path, candidate: Option<&str>, th: Run<'_>) -> PearlFacts {
    if let Some(id) = candidate {
        let json = th(worktree, &["pearls", "show", id, "--handoff", "--json"])
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| pearl_of(&v));
        // A pearl that no longer exists must not erase the id parsed off the
        // branch — the worktree is still that pearl's worktree.
        return json.unwrap_or_default();
    }
    let Some(list) = th(worktree, &["pearls", "prime", "--in-progress", "--cwd", &worktree.to_string_lossy(), "--json"])
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    else {
        return PearlFacts::default();
    };
    let items = list.as_array().map(Vec::as_slice).unwrap_or_default();
    match items {
        [only] => pearl_of(only).unwrap_or_default(),
        _ => PearlFacts::default(),
    }
}

/// `{pearl: {id, title, description}}` → [`PearlFacts`].
fn pearl_of(v: &serde_json::Value) -> Option<PearlFacts> {
    let p = v.get("pearl")?;
    let id = p.get("id")?.as_str()?.to_string();
    let title = p.get("title").and_then(serde_json::Value::as_str).map(str::to_string);
    let desc = p.get("description").and_then(serde_json::Value::as_str).unwrap_or_default();
    Some(PearlFacts {
        id: Some(id),
        text: Some(format!("{} {desc}", title.clone().unwrap_or_default())),
        title,
    })
}

fn run(bin: &str, cwd: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(bin).args(args).current_dir(cwd).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    run("git", cwd, args)
}

fn run_th(cwd: &Path, args: &[&str]) -> Option<String> {
    let bin = std::env::var("SMOOTH_TH_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "th".to_string());
    run(&bin, cwd, args)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn facts(top: &str, common: &str, branch: Option<&str>) -> GitFacts {
        GitFacts {
            toplevel: (!top.is_empty()).then(|| PathBuf::from(top)),
            common_dir: (!common.is_empty()).then(|| PathBuf::from(common)),
            branch: branch.map(str::to_string),
        }
    }

    #[test]
    fn pearl_id_must_prefix_the_name() {
        assert_eq!(pearl_id_prefix("th-c103c1-zero-friction").as_deref(), Some("th-c103c1"));
        assert_eq!(pearl_id_prefix("th-c103c1").as_deref(), Some("th-c103c1"));
        // Anchored: a hex-looking tail is not a pearl.
        assert_eq!(pearl_id_prefix("fix-the-th-c103c1-thing"), None);
        // Not six hex digits.
        assert_eq!(pearl_id_prefix("th-zzzzzz"), None);
        assert_eq!(pearl_id_prefix("th-c103c"), None);
        // A boundary is required, so `th-c103c1x` is not a pearl id.
        assert_eq!(pearl_id_prefix("th-c103c1x"), None);
        assert_eq!(pearl_id_prefix(""), None);
    }

    #[test]
    fn pearl_id_is_found_mid_worktree_name() {
        assert_eq!(pearl_id_in("smooth-th-c103c1-zero-friction").as_deref(), Some("th-c103c1"));
        assert_eq!(pearl_id_in("smooth-th-c103c1").as_deref(), Some("th-c103c1"));
        assert_eq!(pearl_id_in("th-c103c1-zf").as_deref(), Some("th-c103c1"));
        assert_eq!(pearl_id_in("smooai-SMOODEV-3125-x"), None, "a Jira branch is not a pearl");
        assert_eq!(pearl_id_in("smooth"), None);
    }

    #[test]
    fn a_generic_branch_never_becomes_a_title() {
        for b in ["main", "MASTER", "develop"] {
            let i = infer(
                Path::new("/dev/smooth"),
                &facts("/dev/smooth", "/dev/smooth/.git", Some(b)),
                &PearlFacts::default(),
            );
            assert_eq!(i.title, "smooth", "{b} is not a title");
            assert_eq!(i.branch.as_deref(), Some(b), "…but it is still the branch");
        }
    }

    #[test]
    fn jira_keys_are_uppercase_and_anywhere() {
        assert_eq!(jira_key("SMOODEV-3125-companion").as_deref(), Some("SMOODEV-3125"));
        assert_eq!(jira_key("feature/SMOODEV-12").as_deref(), Some("SMOODEV-12"));
        assert_eq!(jira_key("smoodev-3125"), None, "lowercase is not a Jira key");
        assert_eq!(jira_key("th-c103c1-zero-friction"), None);
        assert_eq!(jira_key("no keys here"), None);
    }

    #[test]
    fn worktree_project_split_is_the_pearls_rule() {
        // A linked worktree: common dir lives under the MAIN checkout.
        let g = facts("/dev/smooth-th-c103c1-zf", "/dev/smooth/.git", Some("th-c103c1-zf"));
        let i = infer(Path::new("/dev/smooth-th-c103c1-zf/crates"), &g, &PearlFacts::default());
        assert_eq!(i.worktree, "/dev/smooth-th-c103c1-zf");
        assert_eq!(i.project, "/dev/smooth", "the MAIN checkout, not the linked worktree");
        assert!(i.is_git);
    }

    #[test]
    fn main_checkout_project_is_the_toplevel() {
        let g = facts("/dev/smooth", "/dev/smooth/.git", Some("main"));
        let i = infer(Path::new("/dev/smooth"), &g, &PearlFacts::default());
        assert_eq!(i.project, "/dev/smooth");
        assert_eq!(i.worktree, "/dev/smooth");
        assert_eq!(i.branch.as_deref(), Some("main"));
        assert_eq!(i.pearl_id, None);
        assert_eq!(i.title, "smooth", "`main` says nothing about the work → the directory name");
    }

    #[test]
    fn non_git_dir_still_infers_something_startable() {
        let i = infer(Path::new("/tmp/scratch"), &GitFacts::default(), &PearlFacts::default());
        assert!(!i.is_git);
        assert_eq!(i.worktree, "/tmp/scratch");
        assert_eq!(i.project, "/tmp/scratch");
        assert_eq!(i.branch, None);
        assert_eq!(i.pearl_id, None);
        assert_eq!(i.title, "scratch");
    }

    #[test]
    fn detached_head_has_no_branch_and_falls_back_to_the_dir() {
        let g = facts("/dev/smooth-th-abc123-x", "/dev/smooth/.git", Some("HEAD"));
        let i = infer(Path::new("/dev/smooth-th-abc123-x"), &g, &PearlFacts::default());
        assert_eq!(i.branch, None, "`HEAD` means detached, not a branch");
        assert_eq!(i.pearl_id.as_deref(), Some("th-abc123"), "the worktree name still carries it");
        assert_eq!(i.pearl_source, Some(PearlSource::Worktree));
        assert_eq!(i.title, "smooth-th-abc123-x");
    }

    #[test]
    fn bare_repo_has_no_toplevel_but_still_has_a_project() {
        let g = facts("", "/srv/repos/smooth.git", Some("main"));
        let i = infer(Path::new("/srv/repos/smooth.git"), &g, &PearlFacts::default());
        assert!(i.is_git);
        assert_eq!(i.worktree, "/srv/repos/smooth.git", "no toplevel → cwd");
        assert_eq!(i.project, "/srv/repos");
    }

    #[test]
    fn the_store_beats_the_branch_and_supplies_the_title() {
        let g = facts("/dev/smooth-th-aaa111-x", "/dev/smooth/.git", Some("th-bbb222-other"));
        let p = PearlFacts {
            id: Some("th-aaa111".into()),
            title: Some("Real pearl title".into()),
            text: Some("Real pearl title SMOODEV-42 in the body".into()),
        };
        let i = infer(Path::new("/dev/smooth-th-aaa111-x"), &g, &p);
        assert_eq!(i.pearl_id.as_deref(), Some("th-aaa111"));
        assert_eq!(i.pearl_source, Some(PearlSource::Store));
        assert_eq!(i.title, "Real pearl title");
        assert_eq!(i.jira_key.as_deref(), Some("SMOODEV-42"), "no key in the branch → the pearl's text");
    }

    #[test]
    fn a_deleted_pearl_keeps_the_id_and_loses_only_the_title() {
        let g = facts("/dev/smooth-th-dead01-x", "/dev/smooth/.git", Some("th-dead01-x"));
        let i = infer(Path::new("/dev/smooth-th-dead01-x"), &g, &PearlFacts::default());
        assert_eq!(i.pearl_id.as_deref(), Some("th-dead01"));
        assert_eq!(i.pearl_source, Some(PearlSource::Branch));
        assert_eq!(i.pearl_title, None);
        assert_eq!(i.title, "th-dead01-x", "the branch names the work when the pearl is gone");
    }

    #[test]
    fn jira_branch_without_a_pearl() {
        let g = facts("/dev/smooai-SMOODEV-3125-x", "/dev/smooai/.git/worktrees/x", Some("SMOODEV-3125-companion"));
        let i = infer(Path::new("/dev/smooai-SMOODEV-3125-x"), &g, &PearlFacts::default());
        assert_eq!(i.jira_key.as_deref(), Some("SMOODEV-3125"));
        assert_eq!(i.pearl_id, None);
        assert_eq!(i.title, "SMOODEV-3125-companion");
    }

    #[test]
    fn root_cwd_still_produces_a_title() {
        let i = infer(Path::new("/"), &GitFacts::default(), &PearlFacts::default());
        assert_eq!(i.title, "/", "an empty dir name falls back to the path itself");
    }

    #[test]
    fn gather_with_injected_runners_resolves_the_pearl() {
        let git = |_: &Path, args: &[&str]| match args {
            ["rev-parse", "--show-toplevel"] => Some("/dev/smooth-th-c103c1-zf\n".into()),
            ["rev-parse", "--path-format=absolute", "--git-common-dir"] => Some("/dev/smooth/.git\n".into()),
            ["rev-parse", "--abbrev-ref", "HEAD"] => Some("th-c103c1-zf\n".into()),
            _ => None,
        };
        let th = |_: &Path, args: &[&str]| {
            assert_eq!(args[0..3], ["pearls", "show", "th-c103c1"], "the branch id is confirmed against the store");
            Some(r#"{"pearl":{"id":"th-c103c1","title":"Zero friction","description":"see SMOODEV-9"}}"#.to_string())
        };
        let i = gather_with(Path::new("/dev/smooth-th-c103c1-zf"), &git, &th);
        assert_eq!(i.pearl_id.as_deref(), Some("th-c103c1"));
        assert_eq!(i.pearl_source, Some(PearlSource::Store));
        assert_eq!(i.title, "Zero friction");
        assert_eq!(i.jira_key.as_deref(), Some("SMOODEV-9"));
        assert_eq!(i.project, "/dev/smooth");
    }

    #[test]
    fn gather_falls_back_to_the_single_in_progress_pearl() {
        let git = |_: &Path, args: &[&str]| match args {
            ["rev-parse", "--show-toplevel"] => Some("/dev/smooth".into()),
            ["rev-parse", "--path-format=absolute", "--git-common-dir"] => Some("/dev/smooth/.git".into()),
            ["rev-parse", "--abbrev-ref", "HEAD"] => Some("main".into()),
            _ => None,
        };
        let th = |_: &Path, args: &[&str]| {
            assert_eq!(args[0..2], ["pearls", "prime"], "no id to confirm → ask what's in progress here");
            Some(r#"[{"pearl":{"id":"th-999999","title":"The one open pearl","description":""}}]"#.to_string())
        };
        let i = gather_with(Path::new("/dev/smooth"), &git, &th);
        assert_eq!(i.pearl_id.as_deref(), Some("th-999999"));
        assert_eq!(i.title, "The one open pearl");
    }

    #[test]
    fn gather_refuses_to_guess_between_several_in_progress_pearls() {
        let git = |_: &Path, args: &[&str]| match args {
            ["rev-parse", "--show-toplevel"] => Some("/dev/smooth".into()),
            _ => None,
        };
        let th = |_: &Path, _: &[&str]| Some(r#"[{"pearl":{"id":"th-111111","title":"A"}},{"pearl":{"id":"th-222222","title":"B"}}]"#.to_string());
        let i = gather_with(Path::new("/dev/smooth"), &git, &th);
        assert_eq!(i.pearl_id, None, "two candidates is not an answer");
    }

    #[test]
    fn gather_survives_missing_git_and_th() {
        let none = |_: &Path, _: &[&str]| None;
        let i = gather_with(Path::new("/tmp/nothing"), &none, &none);
        assert!(!i.is_git);
        assert_eq!(i.worktree, "/tmp/nothing");
        assert_eq!(i.title, "nothing");
    }

    #[test]
    fn gather_survives_garbage_from_th() {
        let git = |_: &Path, args: &[&str]| (args[1] == "--show-toplevel").then(|| "/dev/smooth-th-c103c1-zf".to_string());
        let th = |_: &Path, _: &[&str]| Some("Error: issue not found".to_string());
        let i = gather_with(Path::new("/dev/smooth-th-c103c1-zf"), &git, &th);
        assert_eq!(i.pearl_id.as_deref(), Some("th-c103c1"), "a store miss leaves the parsed id standing");
        assert_eq!(i.pearl_title, None);
    }

    #[test]
    fn inferred_round_trips_over_the_wire() {
        let i = infer(
            Path::new("/dev/smooth"),
            &facts("/dev/smooth", "/dev/smooth/.git", Some("main")),
            &PearlFacts::default(),
        );
        let json = serde_json::to_string(&i).unwrap();
        assert_eq!(serde_json::from_str::<Inferred>(&json).unwrap(), i);
        assert!(!json.contains("pearl_id"), "absent fields are omitted, not null");
    }
}
