//! What binary did we actually benchmark? (pearl th-cdf7b8)
//!
//! The Rust engine boots `th daemon` from `PATH`, and `th daemon` runs
//! the SEPARATE `smooth-daemon` binary, also from `PATH`. Neither is
//! rebuilt by `prepare_engine` — go/ts/python are, Rust falls through.
//! So the reference implementation, the one every other engine is
//! compared against and the one the published leaderboard uses, is
//! whatever happens to be installed.
//!
//! That is the same class of bug as th-11284c, where the bench booted a
//! five-week-old TypeScript bundle and attributed the result to the
//! model — but with a wider blast radius. It has already cost real time
//! once: a stale `~/.cargo/bin/smooth-daemon` served for hours during
//! the temperature debugging and was found by accident.
//!
//! Rebuilding automatically would be the wrong fix. A release build of
//! `th` takes minutes and installing binaries is not the bench's job.
//! But a number nobody can attribute to a commit is worse than a slow
//! one, so: record what ran, and say so when it looks stale.

use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;

/// Which upstream build a model alias actually resolved to.
///
/// Model ids are ALIASES that float. DeepSeek repointed
/// `deepseek-v4-flash` at V4-Flash-0731 on 2026-07-31 without changing
/// the id — the calling convention is identical and nothing downstream
/// notices. That is the same failure as benchmarking a stale binary,
/// pointed the other way: the published board says "deepseek-v4-flash"
/// while the thing behind the name changes underneath it, and two runs
/// a month apart are silently not comparable.
///
/// The provider hands us the answer for free. One ~5-token call returns
/// a `system_fingerprint` naming the exact served build, e.g.
/// `fp_a18b46594c_prod0820_fp8_kvcache_20260402`. Cheap enough to do
/// once per model per run; a run nobody can attribute to a build is
/// worth less than the tenth of a cent it costs to pin.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ModelBuild {
    pub model: String,
    /// The provider's build identifier, when it sends one. Absent for
    /// providers that do not — recorded as unknown rather than invented.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,
}

impl ModelBuild {
    /// Ask the gateway which build serves `model`, with the smallest
    /// call that gets an answer.
    ///
    /// # Errors
    /// Errors if the gateway is unreachable or rejects the request.
    pub async fn probe(gateway_url: &str, key: &str, model: &str) -> anyhow::Result<Self> {
        let url = format!("{}/chat/completions", gateway_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 1,
        });
        let text = reqwest::Client::new().post(&url).bearer_auth(key).json(&body).send().await?.text().await?;
        let v: serde_json::Value = serde_json::from_str(&text)?;
        Ok(Self {
            model: model.to_string(),
            system_fingerprint: v
                .get("system_fingerprint")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(ToString::to_string),
        })
    }

    /// One line for the run header.
    #[must_use]
    pub fn render(&self) -> String {
        match &self.system_fingerprint {
            Some(fp) => format!("  model build:   {} -> {fp}", self.model),
            None => format!("  model build:   {} -> (provider sends no fingerprint)", self.model),
        }
    }
}

/// The binaries a run actually executed.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Provenance {
    /// `th --version` output, which carries the build commit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub th_version: Option<String>,
    /// Where `th` resolved from — a dev install and a Homebrew install
    /// can both be on `PATH`, and the first one wins silently.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub th_path: Option<String>,
    /// The daemon binary `th daemon` will exec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_path: Option<String>,
    /// The checkout's HEAD, for comparison against `th_version`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_head: Option<String>,
    /// Set when the binary does not look like it was built from
    /// `repo_head`. Not fatal — you may be benchmarking an older build
    /// deliberately — but it must never be silent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl Provenance {
    /// Resolve what a Rust-engine run will actually execute.
    ///
    /// Every field is best-effort: a bench that refused to run because
    /// it could not identify a binary would be worse than one that runs
    /// and says "unknown".
    #[must_use]
    pub fn detect() -> Self {
        let mut p = Self {
            th_version: run("th", &["--version"]),
            th_path: which("th"),
            daemon_path: which("smooth-daemon"),
            repo_head: run("git", &["rev-parse", "--short=8", "HEAD"]),
            warning: None,
        };
        p.warning = p.staleness_warning();
        p
    }

    /// `th --version` prints the commit it was built from. When that
    /// commit is absent from the version string, the binary was built
    /// from something other than this checkout's HEAD.
    #[must_use]
    pub fn staleness_warning(&self) -> Option<String> {
        let version = self.th_version.as_deref()?;
        let head = self.repo_head.as_deref()?;
        // Compare on the shortest common prefix: `th --version` may
        // print a 7-char sha while `git` gives 8 (or vice versa).
        let n = head.len().min(7);
        let head_prefix = head.get(..n)?;
        if version.contains(head_prefix) {
            return None;
        }
        Some(format!(
            "the `th` on PATH ({}) was not built from this checkout (HEAD {head}) — \
             results are attributable to that binary, not to your working tree. \
             Run `pnpm install:th` if that is not what you meant.",
            version.trim()
        ))
    }

    /// One-line summary for the run header.
    #[must_use]
    pub fn render(&self) -> String {
        let unknown = "unknown".to_string();
        // `th --version` already starts with "th", so don't prefix it again.
        let mut out = format!(
            "  binaries:      {} ({})\n                 daemon {}",
            self.th_version.as_ref().unwrap_or(&unknown).trim(),
            self.th_path.as_ref().unwrap_or(&unknown),
            self.daemon_path.as_ref().unwrap_or(&unknown),
        );
        if let Some(w) = &self.warning {
            out.push_str(&format!("\n  ⚠ {w}"));
        }
        out
    }
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve a program the way the OS will, so a shadowing install is
/// visible. `which` is not on every PATH, so fall back to scanning.
fn which(program: &str) -> Option<String> {
    if let Some(found) = run("which", &[program]) {
        return Some(found);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
        .as_deref()
        .map(PathBuf::from)
        .map(|p| p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(version: &str, head: &str) -> Provenance {
        Provenance {
            th_version: Some(version.into()),
            repo_head: Some(head.into()),
            ..Provenance::default()
        }
    }

    #[test]
    fn a_binary_built_from_head_is_not_flagged() {
        assert!(p("th 0.26.5 (aee82721)", "aee82721").staleness_warning().is_none());
        // Differing sha lengths must still match.
        assert!(p("th 0.26.5 (aee8272)", "aee82721").staleness_warning().is_none());
    }

    #[test]
    fn a_stale_binary_is_flagged_with_what_to_do() {
        // The real incident: a smooth-daemon hours older than the tree.
        let w = p("th 0.26.5 (8de74ed3)", "aee82721").staleness_warning().expect("must warn");
        assert!(w.contains("8de74ed3"), "name the binary actually running: {w}");
        assert!(w.contains("aee82721"), "and what it was compared against: {w}");
        assert!(w.contains("install:th"), "say how to fix it: {w}");
    }

    #[test]
    fn unknown_provenance_warns_about_nothing_rather_than_guessing() {
        // A missing `th` or a non-git checkout must not manufacture a
        // warning — an unknown is not a staleness claim.
        assert!(Provenance::default().staleness_warning().is_none());
        assert!(p("th 1.0", "").staleness_warning().is_none(), "an empty HEAD cannot prove staleness");
    }

    #[test]
    fn a_model_build_renders_its_fingerprint() {
        let b = ModelBuild {
            model: "deepseek-v4-flash".into(),
            system_fingerprint: Some("fp_a18b46594c_prod0820".into()),
        };
        let t = b.render();
        assert!(t.contains("deepseek-v4-flash"), "{t}");
        assert!(t.contains("fp_a18b46594c_prod0820"), "{t}");
    }

    #[test]
    fn a_missing_fingerprint_says_so_rather_than_inventing_one() {
        // Not every provider sends one. "unknown" is a fact; a fabricated
        // build id would be worse than the gap it papers over.
        let b = ModelBuild {
            model: "some-model".into(),
            system_fingerprint: None,
        };
        assert!(b.render().contains("no fingerprint"), "{}", b.render());
        let json = serde_json::to_string(&b).expect("serialises");
        assert!(!json.contains("system_fingerprint"), "an absent build must be omitted, not null: {json}");
    }

    #[test]
    fn render_does_not_repeat_the_program_name() {
        // `th --version` prints "th 0.26.5 (sha)"; prefixing it with "th"
        // rendered "th th 0.26.5".
        let text = Provenance {
            th_version: Some("th 0.26.5 (abc1234)".into()),
            ..Provenance::default()
        }
        .render();
        assert!(!text.contains("th th"), "{text}");
        assert!(text.contains("th 0.26.5 (abc1234)"), "{text}");
    }

    #[test]
    fn render_never_panics_on_missing_fields() {
        let text = Provenance::default().render();
        assert!(text.contains("unknown"), "{text}");
        let flagged = Provenance {
            warning: Some("stale!".into()),
            ..Provenance::default()
        };
        assert!(flagged.render().contains("stale!"));
    }
}
