//! Harvest merged PRs from GitHub via the `gh` CLI for the
//! `score-replay` benchmark.
//!
//! The replay benchmark takes a real-world merged PR, checks out the
//! repo at its base SHA, asks the agent under test to reproduce the
//! human PR's behavior change from the title + body alone, and grades
//! by re-running the human-added tests. This module is the harvest
//! step: query `gh`, filter for PRs that have enough structure to be
//! a useful task (at least 3 files touched and at least one test
//! file in the diff), and return their metadata.
//!
//! Design notes:
//!
//! - We shell out to `gh` rather than re-implement REST + auth. `gh`
//!   already handles `GH_TOKEN`, `~/.config/gh/hosts.yml`, rate-limit
//!   back-off, and `--paginate`. Re-implementing that for a single
//!   benchmark would be malpractice. **Auth failures surface at the
//!   first call** so we don't fail mid-sweep with a confusing error.
//!
//! - The `gh` call site is behind an injectable `GhCli` trait so
//!   tests don't need a real `gh` binary or a network. The default
//!   implementation (`RealGh`) just `Command::new("gh")`s out. Tests
//!   inject a `StubGh` that returns canned JSON.
//!
//! - We use a single `gh pr list --json …` per harvest (cheap, ~5 KB
//!   per page) instead of one `gh pr view` per PR. The 5000/hr REST
//!   limit applies; one harvest of a few thousand PRs uses a handful
//!   of requests.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Metadata for one harvested PR.
///
/// Only the fields `score-replay` actually consumes downstream are
/// captured. `base_sha` is what we check out; `merge_sha` is what we
/// diff against to discover the human's test additions; `files` is
/// the per-path add/delete count; `test_files` is the subset of
/// `files` whose path matches a test-file heuristic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestedPR {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub base_sha: String,
    pub merge_sha: String,
    pub files: Vec<PrFile>,
    pub test_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFile {
    pub path: PathBuf,
    pub additions: u32,
    pub deletions: u32,
}

/// Raw shape of a `gh pr list --json` entry. Kept private so callers
/// can't accidentally depend on `gh`'s JSON shape — they consume the
/// curated `HarvestedPR` view above.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawPr {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(rename = "baseRefOid", default)]
    pub base_ref_oid: String,
    #[serde(rename = "mergeCommit", default)]
    pub merge_commit: Option<RawCommit>,
    #[serde(rename = "mergedAt", default)]
    pub merged_at: Option<String>,
    #[serde(default)]
    pub files: Vec<RawFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawCommit {
    pub oid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawFile {
    pub path: String,
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
}

/// Injectable shell-out boundary. The real implementation runs
/// `gh`; tests inject a stub that returns canned JSON.
///
/// Returns the JSON string `gh pr list --json …` would have printed.
/// Errors propagate so auth failures surface as a clear failure at
/// the very first call — the harvest helper translates them to a
/// user-facing message.
#[async_trait]
pub trait GhCli: Send + Sync {
    async fn pr_list_json(&self, repo: &str, since: NaiveDate, limit: usize) -> Result<String>;
}

/// Production `gh` shell-out.
///
/// Honors `SMOOTH_BENCH_GH_BIN` for tests that want to point at a
/// stub script in `tests/fixtures/` without going through the trait.
/// Most call sites should construct a `StubGh` for tests instead;
/// the env-var override is a back-door for end-to-end tests that
/// also want to exercise the real harvest fn.
pub struct RealGh;

#[async_trait]
impl GhCli for RealGh {
    async fn pr_list_json(&self, repo: &str, since: NaiveDate, limit: usize) -> Result<String> {
        let gh_bin = std::env::var("SMOOTH_BENCH_GH_BIN").unwrap_or_else(|_| "gh".into());
        // `gh pr list` doesn't have a native --since flag, but its
        // `--search` accepts the GitHub search query DSL. Restrict
        // to merged PRs touching the repo, merged on/after `since`.
        let search = format!("is:merged merged:>={since}");
        let output = tokio::process::Command::new(&gh_bin)
            .args([
                "pr",
                "list",
                "--repo",
                repo,
                "--state",
                "merged",
                "--search",
                &search,
                "--limit",
                &limit.to_string(),
                "--json",
                "number,title,body,baseRefOid,mergeCommit,mergedAt,files",
            ])
            .output()
            .await
            .with_context(|| format!("spawning `{gh_bin} pr list` (is gh installed and on PATH?)"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Distinguish auth from generic failures so the operator
            // sees an actionable message at the top of the run.
            if stderr.contains("authentication") || stderr.contains("not logged") || stderr.contains("HTTP 401") {
                return Err(anyhow!(
                    "gh auth failed — run `gh auth login` or set GH_TOKEN before invoking score-replay.\n\
                     gh stderr: {stderr}"
                ));
            }
            return Err(anyhow!("gh pr list failed ({:?}): {stderr}", output.status.code()));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Filter predicate: a PR is eligible for replay if it touched at
/// least three distinct files and at least one of those files looks
/// like a test file by path heuristic.
///
/// The 3-file floor is to make sure the task is meaningful — a
/// 1-file bug fix is the same shape as a typo correction and gives
/// the model almost no signal. The test-file requirement is the
/// scoring path: without a test file in the diff we have nothing to
/// re-run as the grader.
#[must_use]
pub fn is_eligible(pr: &RawPr) -> bool {
    if pr.files.len() < 3 {
        return false;
    }
    pr.files.iter().any(|f| is_test_path(&f.path))
}

/// Heuristic: does this path look like a test file? Conservative —
/// false negatives (missing a real test file) are OK; false positives
/// would let a non-test PR through and the grader would have nothing
/// to run.
///
/// Patterns:
/// - `tests/...` or `test/...` directory (top-level or nested)
/// - filename starts with `test_` (pytest)
/// - filename ends with `_test.py` / `_test.go` / `_test.rs`
/// - filename ends with `.test.ts`/`.test.tsx`/`.test.js`/`.test.jsx`/
///   `.spec.ts`/etc.
/// - filename ends with `Test.java` (JUnit)
#[must_use]
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    // Directory-based.
    for needle in ["/tests/", "/test/", "tests/", "test/"] {
        if lower.starts_with(needle) || lower.contains(needle) {
            return true;
        }
    }
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if name.starts_with("test_") {
        return true;
    }
    for suffix in ["_test.py", "_test.go", "_test.rs", "_tests.rs"] {
        if name.ends_with(suffix) {
            return true;
        }
    }
    for suffix in [
        ".test.ts",
        ".test.tsx",
        ".test.js",
        ".test.jsx",
        ".spec.ts",
        ".spec.tsx",
        ".spec.js",
        ".spec.jsx",
    ] {
        if name.ends_with(suffix) {
            return true;
        }
    }
    // Java JUnit: `FooTest.java` / `FooTests.java`.
    if name.ends_with("test.java") || name.ends_with("tests.java") {
        return true;
    }
    false
}

/// Convert a `RawPr` to the curated `HarvestedPR` shape. Returns
/// `None` if the PR is missing fields we need (e.g. no merge commit
/// — should be impossible for `--state merged` but we don't trust
/// `gh`'s JSON unconditionally).
#[must_use]
pub fn to_harvested(raw: RawPr) -> Option<HarvestedPR> {
    let merge_sha = raw.merge_commit.as_ref()?.oid.clone();
    if raw.base_ref_oid.is_empty() || merge_sha.is_empty() {
        return None;
    }
    let files: Vec<PrFile> = raw
        .files
        .iter()
        .map(|f| PrFile {
            path: PathBuf::from(&f.path),
            additions: f.additions,
            deletions: f.deletions,
        })
        .collect();
    let test_files: Vec<PathBuf> = raw.files.iter().filter(|f| is_test_path(&f.path)).map(|f| PathBuf::from(&f.path)).collect();
    Some(HarvestedPR {
        number: raw.number,
        title: raw.title,
        body: raw.body,
        base_sha: raw.base_ref_oid,
        merge_sha,
        files,
        test_files,
    })
}

/// Harvest eligible merged PRs from `repo` via the default `RealGh`
/// shell-out. Filters by [`is_eligible`] and caps at `limit`.
///
/// For tests / dependency injection, use [`harvest_prs_with`] with a
/// custom `GhCli`.
///
/// # Errors
/// Propagates any failure from the underlying `gh` call. JSON parse
/// errors include the offending byte range for debugging.
pub async fn harvest_prs(repo: &str, since: NaiveDate, limit: usize) -> Result<Vec<HarvestedPR>> {
    harvest_prs_with(&RealGh, repo, since, limit).await
}

/// `harvest_prs` with an injectable `gh` impl, for tests.
///
/// # Errors
/// As [`harvest_prs`].
pub async fn harvest_prs_with(gh: &dyn GhCli, repo: &str, since: NaiveDate, limit: usize) -> Result<Vec<HarvestedPR>> {
    // We over-fetch (limit * 4) because eligibility filtering may
    // throw out 50-75% of PRs (small typo fixes, doc-only PRs, etc).
    // Capped at 500 to keep the JSON response under a few hundred
    // KB; if `limit` is huge the caller can paginate by date.
    let fetch_cap = (limit.saturating_mul(4)).min(500).max(limit);
    let json = gh.pr_list_json(repo, since, fetch_cap).await?;
    let raw_list: Vec<RawPr> =
        serde_json::from_str(&json).with_context(|| format!("parsing gh pr list output (first 200 bytes: {})", &json.chars().take(200).collect::<String>()))?;
    let mut out: Vec<HarvestedPR> = raw_list.into_iter().filter(is_eligible).filter_map(to_harvested).collect();
    out.truncate(limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr_with_files(files: Vec<(&str, u32, u32)>) -> RawPr {
        RawPr {
            number: 1,
            title: "test".into(),
            body: String::new(),
            base_ref_oid: "abc".into(),
            merge_commit: Some(RawCommit { oid: "def".into() }),
            merged_at: Some("2026-05-28T00:00:00Z".into()),
            files: files
                .into_iter()
                .map(|(p, a, d)| RawFile {
                    path: p.into(),
                    additions: a,
                    deletions: d,
                })
                .collect(),
        }
    }

    #[test]
    fn is_test_path_matches_pytest_filename() {
        assert!(is_test_path("test_foo.py"));
        assert!(is_test_path("src/test_foo.py"));
    }

    #[test]
    fn is_test_path_matches_go_suffix() {
        assert!(is_test_path("pkg/foo_test.go"));
    }

    #[test]
    fn is_test_path_matches_jest_spec() {
        assert!(is_test_path("src/foo.test.ts"));
        assert!(is_test_path("src/foo.spec.tsx"));
    }

    #[test]
    fn is_test_path_matches_test_directory() {
        assert!(is_test_path("tests/integration/foo.py"));
        assert!(is_test_path("packages/x/test/foo.js"));
    }

    #[test]
    fn is_test_path_matches_java_junit() {
        assert!(is_test_path("src/test/java/com/example/FooTest.java"));
    }

    #[test]
    fn is_test_path_rejects_plain_source() {
        assert!(!is_test_path("src/lib.rs"));
        assert!(!is_test_path("packages/x/src/foo.ts"));
        assert!(!is_test_path("README.md"));
    }

    #[test]
    fn filters_pr_under_three_files() {
        let pr = pr_with_files(vec![("src/lib.rs", 10, 0), ("tests/foo.rs", 20, 0)]);
        assert!(!is_eligible(&pr));
    }

    #[test]
    fn filters_pr_no_test_file() {
        let pr = pr_with_files(vec![("src/a.rs", 1, 0), ("src/b.rs", 1, 0), ("src/c.rs", 1, 0)]);
        assert!(!is_eligible(&pr));
    }

    #[test]
    fn accepts_pr_with_test_and_threshold() {
        let pr = pr_with_files(vec![("src/lib.rs", 10, 0), ("src/helper.rs", 5, 0), ("tests/lib_test.rs", 20, 0)]);
        assert!(is_eligible(&pr));
    }

    #[test]
    fn to_harvested_extracts_test_files() {
        let pr = pr_with_files(vec![("src/a.rs", 1, 0), ("src/b.rs", 1, 0), ("tests/a_test.rs", 5, 0)]);
        let h = to_harvested(pr).unwrap();
        assert_eq!(h.test_files, vec![PathBuf::from("tests/a_test.rs")]);
        assert_eq!(h.files.len(), 3);
    }

    #[test]
    fn to_harvested_drops_missing_merge_sha() {
        let mut pr = pr_with_files(vec![("a", 1, 0); 3]);
        pr.merge_commit = None;
        assert!(to_harvested(pr).is_none());
    }

    // Stub `gh` impl for end-to-end harvest tests.
    struct StubGh {
        payload: String,
    }

    #[async_trait]
    impl GhCli for StubGh {
        async fn pr_list_json(&self, _repo: &str, _since: NaiveDate, _limit: usize) -> Result<String> {
            Ok(self.payload.clone())
        }
    }

    #[tokio::test]
    async fn harvest_filters_and_truncates() {
        // Two eligible, one ineligible (only 2 files), one ineligible (no test).
        let payload = serde_json::json!([
            {
                "number": 1,
                "title": "good A",
                "body": "fixes bug",
                "baseRefOid": "base1",
                "mergeCommit": { "oid": "merge1" },
                "mergedAt": "2026-05-01T00:00:00Z",
                "files": [
                    { "path": "src/a.rs", "additions": 5, "deletions": 0 },
                    { "path": "src/b.rs", "additions": 3, "deletions": 1 },
                    { "path": "tests/a_test.rs", "additions": 12, "deletions": 0 }
                ]
            },
            {
                "number": 2,
                "title": "no test",
                "body": "",
                "baseRefOid": "base2",
                "mergeCommit": { "oid": "merge2" },
                "mergedAt": "2026-05-01T00:00:00Z",
                "files": [
                    { "path": "src/a.rs", "additions": 5, "deletions": 0 },
                    { "path": "src/b.rs", "additions": 3, "deletions": 1 },
                    { "path": "src/c.rs", "additions": 1, "deletions": 0 }
                ]
            },
            {
                "number": 3,
                "title": "too small",
                "body": "",
                "baseRefOid": "base3",
                "mergeCommit": { "oid": "merge3" },
                "mergedAt": "2026-05-01T00:00:00Z",
                "files": [
                    { "path": "src/a.rs", "additions": 5, "deletions": 0 },
                    { "path": "tests/a_test.rs", "additions": 12, "deletions": 0 }
                ]
            },
            {
                "number": 4,
                "title": "good B",
                "body": "",
                "baseRefOid": "base4",
                "mergeCommit": { "oid": "merge4" },
                "mergedAt": "2026-05-01T00:00:00Z",
                "files": [
                    { "path": "src/a.py", "additions": 5, "deletions": 0 },
                    { "path": "src/b.py", "additions": 3, "deletions": 1 },
                    { "path": "tests/test_a.py", "additions": 12, "deletions": 0 }
                ]
            }
        ])
        .to_string();
        let gh = StubGh { payload };
        let since = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();

        let harvested = harvest_prs_with(&gh, "owner/repo", since, 10).await.unwrap();
        assert_eq!(harvested.len(), 2);
        assert_eq!(harvested[0].number, 1);
        assert_eq!(harvested[1].number, 4);

        // limit truncation
        let one = harvest_prs_with(&gh, "owner/repo", since, 1).await.unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].number, 1);
    }

    #[tokio::test]
    async fn harvest_returns_clear_error_on_bad_json() {
        struct BadGh;
        #[async_trait]
        impl GhCli for BadGh {
            async fn pr_list_json(&self, _repo: &str, _since: NaiveDate, _limit: usize) -> Result<String> {
                Ok("not json".into())
            }
        }
        let since = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let err = harvest_prs_with(&BadGh, "owner/repo", since, 10).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("parsing gh pr list output"), "{msg}");
    }

    #[tokio::test]
    async fn harvest_propagates_auth_failure() {
        struct AuthFail;
        #[async_trait]
        impl GhCli for AuthFail {
            async fn pr_list_json(&self, _repo: &str, _since: NaiveDate, _limit: usize) -> Result<String> {
                Err(anyhow!("gh auth failed — run `gh auth login` or set GH_TOKEN before invoking score-replay."))
            }
        }
        let since = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let err = harvest_prs_with(&AuthFail, "owner/repo", since, 10).await.unwrap_err();
        assert!(format!("{err:#}").contains("gh auth"));
    }
}
