//! `score-research` — agentic web-research bench dimension. Pearl `th-f4ac64`.
//!
//! Sibling to `score-cleanup`. Where cleanup grades sysadmin-style
//! filesystem hygiene, research grades the agent's ability to answer
//! questions that REQUIRE web search — fact lookups, identifying a title
//! from a fuzzy description, resolving a stale link, etc. This is the
//! dimension that fairly tests the new `web_search` tool (pearl
//! `th-2cc3f1`) added alongside.
//!
//! ## Task fixture layout
//!
//! ```text
//! tasks-real/
//!   research-hijack-year/
//!     README.md              # agent-facing instructions (the question + how to answer)
//!     manifest.toml          # bench-harness config (see [`ResearchManifest`])
//!     setup.sh               # OPTIONAL — populate $WORKSPACE if the task needs scaffolding
//!   research-*/...
//! ```
//!
//! `setup.sh` is optional for research tasks — most just need an empty
//! workspace plus the prompt. When present it runs with `WORKSPACE` env
//! set, same as cleanup.
//!
//! ## Scoring axes (v1)
//!
//! | axis                 | how it's measured                                                          |
//! |---------------------|----------------------------------------------------------------------------|
//! | `answer_correctness` | Fraction of `expected_keywords` present (case-insensitive) in the answer  |
//! | `cited_source`      | 1.0 if the answer contains a URL, 0.0 otherwise (anti-hallucination probe) |
//!
//! `min_correctness` (default 1.0) is a hard kill: if the keyword-match
//! fraction is below it, the task scores 0.0 regardless of other axes —
//! a half-right factual answer is worse than no answer at all on a
//! research probe.
//!
//! v1 deliberately omits `tool_efficiency` and `no_edits`. They're
//! planned (pearl `th-f4ac64` notes), but the first fixture proves the
//! pipeline end-to-end without them and they layer in cleanly later.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::score::{LanguageScore, Score};
use crate::score_cleanup::CoachCfg;

/// Per-task config bench reads from `manifest.toml` inside each
/// `research-*/` dir.
#[derive(Debug, Clone, Deserialize)]
pub struct ResearchManifest {
    pub task: ResearchTaskMeta,
    #[serde(default)]
    pub setup: Option<SetupCfg>,
    pub expect: ExpectCfg,
    #[serde(default)]
    pub weights: AxisWeights,
    /// Coach mode is shared with cleanup so a sweep across both
    /// dimensions can tune coaching consistently.
    #[serde(default)]
    pub coach: CoachCfg,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResearchTaskMeta {
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetupCfg {
    pub script: String,
    #[serde(default = "default_setup_timeout")]
    pub timeout_s: u64,
}

const fn default_setup_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpectCfg {
    /// Workspace-relative path where the agent must write its answer.
    /// Defaults to `.smooth/answer.txt`. Created by the agent — not the
    /// setup script.
    #[serde(default = "default_answer_path")]
    pub answer_path: String,
    /// Substrings (case-insensitive) that the answer MUST contain. ALL
    /// must be present for `answer_correctness == 1.0`; partial matches
    /// scale linearly.
    pub expected_keywords: Vec<String>,
    /// Threshold below which the task hard-kills to 0.0. Default 1.0 —
    /// every keyword must be present.
    #[serde(default = "default_min_correctness")]
    pub min_correctness: f64,
}

fn default_answer_path() -> String {
    ".smooth/answer.txt".to_string()
}

const fn default_min_correctness() -> f64 {
    1.0
}

/// Per-axis weights. v1 is two axes — keep weights summing to 1.0.
#[derive(Debug, Clone, Deserialize)]
pub struct AxisWeights {
    #[serde(default = "default_w_correctness")]
    pub answer_correctness: f64,
    #[serde(default = "default_w_cited")]
    pub cited_source: f64,
}

const fn default_w_correctness() -> f64 {
    0.85
}

const fn default_w_cited() -> f64 {
    0.15
}

impl Default for AxisWeights {
    fn default() -> Self {
        Self {
            answer_correctness: default_w_correctness(),
            cited_source: default_w_cited(),
        }
    }
}

impl AxisWeights {
    #[must_use]
    pub fn sum(&self) -> f64 {
        self.answer_correctness + self.cited_source
    }
}

/// Per-task scoring output.
#[derive(Debug, Clone, Serialize)]
pub struct ResearchTaskResult {
    pub task_id: String,
    pub description: String,
    pub answer_present: bool,
    pub answer_correctness: f64,
    pub matched_keywords: Vec<String>,
    pub missing_keywords: Vec<String>,
    pub cited_source: bool,
    pub weighted_score: f64,
    pub agent_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResearchScore {
    pub base: Score,
    pub by_task: Vec<ResearchTaskResult>,
}

/// Inputs the live driver hands to `score_one_task` after running the
/// agent. Kept minimal — research scoring is dominated by the answer
/// file's contents on disk, not by transcript artifacts.
#[derive(Debug, Clone, Default)]
pub struct AgentRunArtifacts {
    pub agent_error: Option<String>,
}

/// Read the agent's answer file. Returns `None` if the file doesn't
/// exist (counts as "no answer"), `Err` only for unexpected IO failures
/// like a permission error on a file that DOES exist.
///
/// # Errors
/// Returns Err if the file exists but can't be read.
pub fn read_answer(workspace: &Path, rel: &str) -> Result<Option<String>> {
    let p = workspace.join(rel);
    if !p.exists() {
        return Ok(None);
    }
    Ok(Some(std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?))
}

/// Compute the answer-correctness axis: fraction of keywords present
/// case-insensitively in the answer text. Returns `(score, matched,
/// missing)` so the renderer can show exactly which keywords missed.
#[must_use]
pub fn match_keywords(answer: &str, keywords: &[String]) -> (f64, Vec<String>, Vec<String>) {
    if keywords.is_empty() {
        return (1.0, Vec::new(), Vec::new());
    }
    let haystack = answer.to_lowercase();
    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for kw in keywords {
        let needle = kw.to_lowercase();
        if haystack.contains(&needle) {
            matched.push(kw.clone());
        } else {
            missing.push(kw.clone());
        }
    }
    let score = matched.len() as f64 / keywords.len() as f64;
    (score, matched, missing)
}

/// Detect whether the answer cites a URL. Very loose — looks for any
/// `http://` or `https://` substring. Enough to discriminate
/// "I made this up" from "I searched and copied a snippet".
#[must_use]
pub fn has_url_citation(answer: &str) -> bool {
    let lower = answer.to_lowercase();
    lower.contains("http://") || lower.contains("https://")
}

/// Pure scoring function. Given the manifest + the answer file's
/// contents (or `None` if missing), produce the per-task result.
#[must_use]
pub fn score_one_task(
    meta: &ResearchTaskMeta,
    expect: &ExpectCfg,
    weights: &AxisWeights,
    answer: Option<&str>,
    artifacts: &AgentRunArtifacts,
) -> ResearchTaskResult {
    let answer_present = answer.is_some();
    let answer_text = answer.unwrap_or("");
    let (correctness, matched, missing) = match_keywords(answer_text, &expect.expected_keywords);
    let cited = has_url_citation(answer_text);
    let cited_axis = if cited { 1.0 } else { 0.0 };

    let raw_weighted = correctness * weights.answer_correctness + cited_axis * weights.cited_source;

    // Hard kill: below the min_correctness threshold, the task is
    // worthless regardless of citation quality. A confidently-cited
    // wrong answer is worse than a clearly-empty answer.
    let weighted_score = if correctness >= expect.min_correctness && answer_present {
        raw_weighted
    } else {
        0.0
    };

    ResearchTaskResult {
        task_id: meta.id.clone(),
        description: meta.description.clone(),
        answer_present,
        answer_correctness: correctness,
        matched_keywords: matched,
        missing_keywords: missing,
        cited_source: cited,
        weighted_score,
        agent_error: artifacts.agent_error.clone(),
    }
}

/// Aggregate per-task results into a Score (mean of weighted, with
/// hard-kills represented as 0 in the mean). Same shape as
/// score_cleanup so the existing JSON pipeline accepts it.
#[must_use]
pub fn aggregate(per_task: &[ResearchTaskResult], smooth_version: String, commit_sha: String) -> Score {
    use std::collections::BTreeMap;
    let n = per_task.len();
    let (overall_pass_rate, tasks_attempted, tasks_green) = if n == 0 {
        (0.0, 0, 0)
    } else {
        let mean: f64 = per_task.iter().map(|t| t.weighted_score).sum::<f64>() / n as f64;
        let green = per_task.iter().filter(|t| t.weighted_score >= 0.5).count() as u32;
        (mean, n as u32, green)
    };
    let mut by_language: BTreeMap<String, LanguageScore> = BTreeMap::new();
    by_language.insert("research".to_string(), LanguageScore::from_counts(tasks_attempted, tasks_green));
    Score {
        smooth_version,
        commit_sha,
        ran_at: chrono::Utc::now(),
        overall_pass_rate,
        by_language,
        tasks_attempted,
        tasks_green,
        tasks_inconclusive: 0,
        cost_usd: 0.0,
        median_task_ms: 0,
        budget_usd_cap: 0.0,
        budget_usd_hit: false,
    }
}

#[must_use]
pub fn sweep_passed(per_task: &[ResearchTaskResult]) -> bool {
    if per_task.is_empty() {
        return false;
    }
    let mean: f64 = per_task.iter().map(|t| t.weighted_score).sum::<f64>() / per_task.len() as f64;
    mean >= 0.5
}

/// Discover `research-*` dirs under `tasks_dir`.
///
/// # Errors
/// Returns Err if `tasks_dir` doesn't exist or isn't readable.
pub fn discover_tasks(tasks_dir: &Path) -> Result<Vec<PathBuf>> {
    if !tasks_dir.exists() {
        return Err(anyhow!("tasks dir does not exist: {}", tasks_dir.display()));
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(tasks_dir).with_context(|| format!("read {}", tasks_dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("research-") {
            continue;
        }
        if !path.join("manifest.toml").exists() {
            continue;
        }
        out.push(path);
    }
    out.sort();
    Ok(out)
}

/// Load a research manifest, validating that weights aren't summed-to-zero
/// (an easy typo that silently kills scoring).
///
/// # Errors
/// Bubbles file IO + TOML parse errors with context, and rejects
/// zero-weight configs.
pub fn load_manifest(task_dir: &Path) -> Result<ResearchManifest> {
    let p = task_dir.join("manifest.toml");
    let contents = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    let m: ResearchManifest = toml::from_str(&contents).with_context(|| format!("parse {}", p.display()))?;
    if m.weights.sum() == 0.0 {
        return Err(anyhow!("manifest {} has weights summing to 0", p.display()));
    }
    Ok(m)
}

/// Run the optional setup.sh. No-op if `setup` is None.
///
/// # Errors
/// Returns Err if the script exists but fails to spawn, exits non-zero,
/// or times out. Missing scripts when `setup = None` are not errors.
pub fn run_setup(task_dir: &Path, setup: Option<&SetupCfg>, work_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(work_dir)?;
    let Some(cfg) = setup else {
        return Ok(());
    };
    let script = task_dir.join(&cfg.script);
    if !script.exists() {
        return Err(anyhow!("setup script not found: {}", script.display()));
    }
    let mut child = std::process::Command::new("bash")
        .arg(&script)
        .env("WORKSPACE", work_dir)
        .spawn()
        .with_context(|| format!("spawn setup {}", script.display()))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(cfg.timeout_s);
    loop {
        match child.try_wait()? {
            Some(status) => {
                if !status.success() {
                    return Err(anyhow!("setup {} exited {:?}", script.display(), status.code()));
                }
                return Ok(());
            }
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(anyhow!("setup {} timed out after {}s", script.display(), cfg.timeout_s));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> ResearchTaskMeta {
        ResearchTaskMeta {
            id: "research-t".into(),
            description: "test".into(),
        }
    }

    fn expect(keywords: Vec<&str>) -> ExpectCfg {
        ExpectCfg {
            answer_path: ".smooth/answer.txt".into(),
            expected_keywords: keywords.into_iter().map(String::from).collect(),
            min_correctness: 1.0,
        }
    }

    #[test]
    fn match_keywords_all_present_is_one() {
        let (score, matched, missing) = match_keywords("Hijack premiered in 2023 on Apple TV+.", &["2023".into(), "apple tv".into()]);
        assert_eq!(score, 1.0);
        assert_eq!(matched.len(), 2);
        assert!(missing.is_empty());
    }

    #[test]
    fn match_keywords_case_insensitive() {
        let (score, _, _) = match_keywords("HIJACK 2023", &["hijack".into(), "2023".into()]);
        assert_eq!(score, 1.0);
    }

    #[test]
    fn match_keywords_partial() {
        let (score, matched, missing) = match_keywords("Hijack premiered in 2023", &["2023".into(), "Disney+".into()]);
        assert_eq!(score, 0.5);
        assert_eq!(matched, vec!["2023".to_string()]);
        assert_eq!(missing, vec!["Disney+".to_string()]);
    }

    #[test]
    fn match_keywords_empty_keywords_is_one() {
        let (score, m, mi) = match_keywords("anything", &[]);
        assert_eq!(score, 1.0);
        assert!(m.is_empty());
        assert!(mi.is_empty());
    }

    #[test]
    fn url_citation_detection() {
        assert!(has_url_citation("see https://example.com for details"));
        assert!(has_url_citation("http://foo.bar"));
        assert!(has_url_citation("HTTPS://CAPS.WORK"));
        assert!(!has_url_citation("no url here"));
        assert!(!has_url_citation("example.com is not enough"));
    }

    #[test]
    fn perfect_answer_with_citation_scores_one() {
        let weights = AxisWeights::default();
        let r = score_one_task(
            &meta(),
            &expect(vec!["2023", "Apple TV"]),
            &weights,
            Some("Hijack premiered in 2023 on Apple TV+. https://tvmaze.com/shows/61245"),
            &AgentRunArtifacts::default(),
        );
        assert!((r.weighted_score - 1.0).abs() < 1e-9, "got {}", r.weighted_score);
        assert!(r.answer_present);
        assert!(r.cited_source);
    }

    #[test]
    fn missing_keyword_hard_kills_at_default_threshold() {
        let r = score_one_task(
            &meta(),
            &expect(vec!["2023", "Disney+"]),
            &AxisWeights::default(),
            Some("Hijack premiered in 2023 on Apple TV+. https://example.com"),
            &AgentRunArtifacts::default(),
        );
        // Default min_correctness = 1.0; 1-of-2 → 0.5 < 1.0 → hard kill.
        assert_eq!(r.weighted_score, 0.0);
        assert_eq!(r.missing_keywords, vec!["Disney+".to_string()]);
    }

    #[test]
    fn relaxed_threshold_passes_partial_correct() {
        let mut exp = expect(vec!["2023", "Disney+"]);
        exp.min_correctness = 0.5;
        let r = score_one_task(
            &meta(),
            &exp,
            &AxisWeights::default(),
            Some("Hijack premiered in 2023. https://example.com"),
            &AgentRunArtifacts::default(),
        );
        // correctness=0.5 ≥ threshold; weighted = 0.5*0.85 + 1*0.15 = 0.575
        assert!((r.weighted_score - 0.575).abs() < 1e-9, "got {}", r.weighted_score);
    }

    #[test]
    fn no_answer_file_hard_kills() {
        let r = score_one_task(&meta(), &expect(vec!["2023"]), &AxisWeights::default(), None, &AgentRunArtifacts::default());
        assert_eq!(r.weighted_score, 0.0);
        assert!(!r.answer_present);
        assert!(r.missing_keywords.contains(&"2023".to_string()));
    }

    #[test]
    fn missing_citation_costs_cited_axis_weight() {
        let r = score_one_task(
            &meta(),
            &expect(vec!["2023"]),
            &AxisWeights::default(),
            Some("It was 2023."),
            &AgentRunArtifacts::default(),
        );
        // correctness=1.0 ≥ threshold; weighted = 1*0.85 + 0*0.15 = 0.85
        assert!((r.weighted_score - 0.85).abs() < 1e-9, "got {}", r.weighted_score);
        assert!(!r.cited_source);
    }

    #[test]
    fn aggregate_with_no_tasks_is_zero() {
        let score = aggregate(&[], "v0".into(), "abc".into());
        assert_eq!(score.overall_pass_rate, 0.0);
        assert_eq!(score.tasks_attempted, 0);
    }

    #[test]
    fn aggregate_with_one_passing_task() {
        let r = ResearchTaskResult {
            task_id: "t".into(),
            description: "".into(),
            answer_present: true,
            answer_correctness: 1.0,
            matched_keywords: vec!["a".into()],
            missing_keywords: vec![],
            cited_source: true,
            weighted_score: 1.0,
            agent_error: None,
        };
        let score = aggregate(std::slice::from_ref(&r), "v0".into(), "abc".into());
        assert_eq!(score.overall_pass_rate, 1.0);
        assert_eq!(score.tasks_attempted, 1);
        assert_eq!(score.tasks_green, 1);
    }

    #[test]
    fn sweep_passed_requires_at_least_one_task() {
        assert!(!sweep_passed(&[]));
    }

    #[test]
    fn sweep_passed_at_mean_half() {
        let make = |s: f64| ResearchTaskResult {
            task_id: "t".into(),
            description: "".into(),
            answer_present: true,
            answer_correctness: s,
            matched_keywords: vec![],
            missing_keywords: vec![],
            cited_source: false,
            weighted_score: s,
            agent_error: None,
        };
        let tasks = vec![make(0.7), make(0.4)];
        assert!(sweep_passed(&tasks));
        let tasks_below = vec![make(0.4), make(0.4)];
        assert!(!sweep_passed(&tasks_below));
    }

    #[test]
    fn axis_weights_default_sums_to_one() {
        assert!((AxisWeights::default().sum() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn read_answer_handles_missing_file_as_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_answer(tmp.path(), ".smooth/answer.txt").unwrap().is_none());
    }

    #[test]
    fn read_answer_reads_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join(".smooth/answer.txt");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "the answer is 42").unwrap();
        assert_eq!(read_answer(tmp.path(), ".smooth/answer.txt").unwrap().as_deref(), Some("the answer is 42"));
    }

    #[test]
    fn discover_tasks_skips_non_research_prefixes() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["research-a", "research-b", "cleanup-x", "fix-y"] {
            let dir = tmp.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("manifest.toml"), "").unwrap();
        }
        let found = discover_tasks(tmp.path()).unwrap();
        assert_eq!(found.len(), 2);
        let names: Vec<_> = found.iter().filter_map(|p| p.file_name().and_then(|n| n.to_str())).collect();
        assert!(names.contains(&"research-a"));
        assert!(names.contains(&"research-b"));
    }

    #[test]
    fn load_manifest_rejects_zero_weights() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("research-z");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.toml"),
            r#"
[task]
id = "z"
description = "z"

[expect]
expected_keywords = ["x"]

[weights]
answer_correctness = 0.0
cited_source = 0.0
"#,
        )
        .unwrap();
        let err = load_manifest(&dir).unwrap_err();
        assert!(err.to_string().contains("summing to 0"), "got: {err}");
    }

    #[test]
    fn load_manifest_uses_defaults_for_omitted_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("research-defaults");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.toml"),
            r#"
[task]
id = "defaults"
description = "default test"

[expect]
expected_keywords = ["x"]
"#,
        )
        .unwrap();
        let m = load_manifest(&dir).unwrap();
        assert_eq!(m.expect.answer_path, ".smooth/answer.txt");
        assert_eq!(m.expect.min_correctness, 1.0);
        assert!(m.setup.is_none());
        assert!((m.weights.sum() - 1.0).abs() < 1e-9);
    }
}
