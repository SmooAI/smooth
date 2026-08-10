//! The GitHub commit-status surface, spoken through the `gh` CLI.
//!
//! Shelling `gh api` rather than talking HTTP is deliberate: it is what the bash
//! did, so the auth path (`gh auth`, `GH_TOKEN`, enterprise hosts) is identical
//! and there is no second credential story to get wrong.

use std::ffi::OsString;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use super::env::Sys;

/// GitHub caps a status description at 140 characters and rejects longer ones.
const MAX_DESCRIPTION: usize = 140;

/// The prefix every attestation shares. `pr-checks.yml` reads statuses whose
/// context starts with this and skips the matching row.
pub const CONTEXT_PREFIX: &str = "ci-attest/";

pub struct Gh {
    bin: OsString,
    pub repo: String,
}

fn run(bin: &OsString, args: &[&str]) -> Result<String> {
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to run {}", bin.to_string_lossy()))?;
    if !out.status.success() {
        bail!(
            "{} {} failed: {}",
            bin.to_string_lossy(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Gh {
    pub fn detect(sys: &Sys) -> Result<Self> {
        let bin = sys.gh.clone();
        let repo = match std::env::var("CI_ATTEST_REPO").or_else(|_| std::env::var("SMOOTH_ATTEST_REPO")) {
            Ok(r) if !r.is_empty() => r,
            _ => run(&bin, &["repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"])
                .context("could not determine the GitHub repo — is `gh` logged in?")?,
        };
        Ok(Self { bin, repo })
    }

    /// Falls back to `main`, matching the bash. A lookup failure here must not
    /// abort the run: the branch name is only used to decide whether to refuse.
    pub fn default_branch(&self) -> String {
        run(&self.bin, &["repo", "view", "--json", "defaultBranchRef", "--jq", ".defaultBranchRef.name"])
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "main".to_string())
    }

    pub fn post_status(&self, sha: &str, check: &str, state: &str, description: &str) -> Result<()> {
        let path = format!("/repos/{}/statuses/{sha}", self.repo);
        let context = format!("{CONTEXT_PREFIX}{check}");
        let desc = truncate(description, MAX_DESCRIPTION);
        run(
            &self.bin,
            &[
                "api",
                "-X",
                "POST",
                &path,
                "-f",
                &format!("state={state}"),
                "-f",
                &format!("context={context}"),
                "-f",
                &format!("description={desc}"),
                "--silent",
            ],
        )
        .map(drop)
    }

    /// Every `ci-attest/*` status on `sha`, newest first, as `(check, state)`.
    pub fn attestations(&self, sha: &str) -> Result<Vec<(String, String)>> {
        let raw = run(&self.bin, &["api", &format!("/repos/{}/commits/{sha}/status", self.repo)])?;
        Ok(parse_statuses(&raw))
    }

    /// A workflow run for this commit that is still going. Its rows read the
    /// statuses once, ~20s in, so anything credited now arrives too late for it.
    pub fn run_in_flight(&self, sha: &str) -> Option<String> {
        let raw = run(&self.bin, &["run", "list", "--commit", sha, "--limit", "1", "--json", "databaseId,status"]).ok()?;
        let runs: Vec<serde_json::Value> = serde_json::from_str(&raw).ok()?;
        runs.iter()
            .find(|r| r.get("status").and_then(serde_json::Value::as_str) != Some("completed"))
            .and_then(|r| r.get("databaseId"))
            .map(ToString::to_string)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// GitHub lists statuses newest-first and keeps superseded ones, so the first
/// entry for a context is the live state. Empty output (no statuses at all) is
/// not an error — it is the normal state of a fresh commit.
fn parse_statuses(raw: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let mut seen: Vec<(String, String)> = Vec::new();
    let Some(list) = v.get("statuses").and_then(serde_json::Value::as_array) else {
        return seen;
    };
    for s in list {
        let (Some(ctx), Some(state)) = (
            s.get("context").and_then(serde_json::Value::as_str),
            s.get("state").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        let Some(check) = ctx.strip_prefix(CONTEXT_PREFIX) else {
            continue;
        };
        if seen.iter().any(|(c, _)| c == check) {
            continue;
        }
        seen.push((check.to_string(), state.to_string()));
    }
    seen
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_attestation_contexts() {
        let raw = r#"{"statuses":[
            {"context":"ci-attest/lint","state":"success"},
            {"context":"some-other/thing","state":"failure"},
            {"context":"ci-attest/rust","state":"failure"}
        ]}"#;
        assert_eq!(parse_statuses(raw), vec![("lint".into(), "success".into()), ("rust".into(), "failure".into())]);
    }

    #[test]
    fn the_newest_status_for_a_context_wins() {
        let raw = r#"{"statuses":[
            {"context":"ci-attest/lint","state":"success"},
            {"context":"ci-attest/lint","state":"failure"}
        ]}"#;
        assert_eq!(parse_statuses(raw), vec![("lint".into(), "success".into())]);
    }

    #[test]
    fn a_commit_with_no_statuses_is_not_an_error() {
        assert!(parse_statuses(r#"{"statuses":[]}"#).is_empty());
        assert!(parse_statuses("").is_empty());
    }

    #[test]
    fn descriptions_are_capped_at_githubs_limit() {
        let long = "x".repeat(200);
        assert_eq!(truncate(&long, MAX_DESCRIPTION).chars().count(), MAX_DESCRIPTION);
        assert_eq!(truncate("short", MAX_DESCRIPTION), "short");
    }

    #[test]
    fn truncation_does_not_split_a_multibyte_char() {
        let s = "é".repeat(200);
        assert_eq!(truncate(&s, MAX_DESCRIPTION).chars().count(), MAX_DESCRIPTION);
    }
}
