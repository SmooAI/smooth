//! The agentic / workflow benchmark (pearl th-300d7d).
//!
//! The aider-polyglot bench measures whether the agent can *edit code*.
//! This one measures whether it takes the right **actions**: read state,
//! chain tools, mutate the right things, and — the part nobody tests —
//! decline to mutate the wrong things.
//!
//! ## Shape of a scenario
//!
//! Data-driven TOML (`agentic-scenarios.toml`, embedded at build time):
//! an `id`, a natural-language `prompt`, optional `setup` files seeded
//! into the workspace, and a `check` that is either
//!
//! - **deterministic** — assertions over the final workspace state.
//!   Preferred: exact, free, no model in the loop.
//! - **judge** — a rubric graded by a cheap model over the evidence
//!   (workspace + tool transcript + final answer). For open-ended goals.
//!
//! ## Safety
//!
//! Scenarios NEVER touch real services. Two rules, both structural:
//!
//! 1. Default isolation is the microVM ([`Isolation::MicroVm`]) with
//!    default-deny egress — the only hole is the LLM gateway. No
//!    scenario may add an egress rule for a real host.
//! 2. Every "external action" is simulated as a **JSON state file in the
//!    mounted workspace** (`watchlist.json`, `escalations.json`, …).
//!    The agent's writes land on the host filesystem through the bind
//!    mount, so the host can assert on them without any network at all.
//!    That is the whole mock: a file. No in-VM server, nothing to leak.
//!
//! A scenario that wanted to hit `smoo-hub` would be rejected in review;
//! there is deliberately no seam for it here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::canonical_driver::run_via_canonical;
use crate::engine::{Engine, Isolation, WorkspaceBooter};
use crate::judge::{judge, JudgeEvidence};

/// One file seeded into the scenario workspace before the agent runs.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupFile {
    /// Workspace-relative path. Must not escape the workspace.
    pub path: String,
    pub content: String,
}

/// One assertion over the final workspace state.
///
/// All populated fields must hold. `file` is workspace-relative;
/// `pointer` (a JSON pointer, e.g. `/0/title`) narrows the target to a
/// value inside a JSON file, otherwise the target is the whole file.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Assertion {
    pub file: String,
    #[serde(default)]
    pub pointer: Option<String>,
    /// Target must equal this exactly (trimmed).
    #[serde(default)]
    pub equals: Option<String>,
    /// Target must contain this substring.
    #[serde(default)]
    pub contains: Option<String>,
    /// Target must NOT contain this substring.
    #[serde(default)]
    pub not_contains: Option<String>,
    /// The file must not exist at all.
    #[serde(default)]
    pub missing: bool,
    /// The file must be byte-identical to what `setup` seeded — the
    /// assertion that makes "don't do the wrong thing" scenarios real.
    #[serde(default)]
    pub unchanged: bool,
}

/// How a scenario is scored.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Check {
    /// Assertions over the resulting workspace. Preferred.
    Deterministic { asserts: Vec<Assertion> },
    /// LLM-as-judge over a rubric. For open-ended goals only.
    Judge { rubric: String },
}

impl Check {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Deterministic { .. } => "deterministic",
            Self::Judge { .. } => "judge",
        }
    }
}

/// One agentic scenario.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub id: String,
    /// One-line human description (shown in the table header comment).
    #[serde(default)]
    pub description: String,
    /// The user's goal, in natural language — exactly what a person
    /// would type. No tool hints, no step lists.
    pub prompt: String,
    #[serde(default)]
    pub setup: Vec<SetupFile>,
    pub check: Check,
}

/// Raw TOML wrapper: `[[scenario]]` array.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioFile {
    /// Defaulted so an empty document fails our own "no scenarios"
    /// check with a readable message instead of serde's missing-field one.
    #[serde(default)]
    scenario: Vec<Scenario>,
}

/// Load + validate the scenario list baked into the binary.
///
/// # Errors
/// Errors when the embedded TOML fails to parse or validate — which in
/// practice only happens when an engineer edits it into an invalid
/// state, and the unit tests catch that before merge.
pub fn default_scenarios() -> Result<Vec<Scenario>> {
    parse_scenarios(include_str!("../agentic-scenarios.toml"))
}

/// Parse + validate a scenario TOML document.
///
/// Validation is the point: a scenario with a duplicate id, an empty
/// prompt, no assertions, or a setup path escaping the workspace is a
/// silently-broken benchmark, so it fails loud at load.
///
/// # Errors
/// Errors on malformed TOML or any validation failure.
pub fn parse_scenarios(toml_str: &str) -> Result<Vec<Scenario>> {
    let file: ScenarioFile = toml::from_str(toml_str).context("parsing agentic scenario TOML")?;
    anyhow::ensure!(!file.scenario.is_empty(), "scenario file contains no [[scenario]] entries");

    let mut seen = std::collections::HashSet::new();
    for s in &file.scenario {
        anyhow::ensure!(!s.id.trim().is_empty(), "scenario has an empty id");
        anyhow::ensure!(seen.insert(s.id.clone()), "duplicate scenario id: {}", s.id);
        anyhow::ensure!(!s.prompt.trim().is_empty(), "scenario {} has an empty prompt", s.id);
        for f in &s.setup {
            anyhow::ensure!(is_safe_relative(&f.path), "scenario {}: setup path {:?} escapes the workspace", s.id, f.path);
        }
        match &s.check {
            Check::Deterministic { asserts } => {
                anyhow::ensure!(!asserts.is_empty(), "scenario {} has a deterministic check with no assertions", s.id);
                for a in asserts {
                    anyhow::ensure!(is_safe_relative(&a.file), "scenario {}: assert path {:?} escapes the workspace", s.id, a.file);
                    anyhow::ensure!(
                        a.equals.is_some() || a.contains.is_some() || a.not_contains.is_some() || a.missing || a.unchanged,
                        "scenario {}: assertion on {:?} asserts nothing",
                        s.id,
                        a.file
                    );
                    if a.unchanged {
                        anyhow::ensure!(
                            s.setup.iter().any(|f| f.path == a.file),
                            "scenario {}: `unchanged` on {:?}, which setup never seeded",
                            s.id,
                            a.file
                        );
                    }
                }
            }
            Check::Judge { rubric } => anyhow::ensure!(!rubric.trim().is_empty(), "scenario {} has an empty judge rubric", s.id),
        }
    }
    Ok(file.scenario)
}

/// Reject absolute paths and any `..` component — a scenario must not be
/// able to seed or assert outside its own workspace.
fn is_safe_relative(p: &str) -> bool {
    let path = Path::new(p);
    !p.is_empty() && path.is_relative() && path.components().all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// The three outcomes a scenario can have.
///
/// `Inconclusive` exists so a judge failure (transport error, garbage
/// verdict) can never be recorded as a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Verdict {
    Pass,
    Fail,
    Inconclusive,
}

impl Verdict {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Inconclusive => "INCONCLUSIVE",
        }
    }
}

/// One scenario's result. Serialised as one JSON-lines record, carrying
/// the engine/model/isolation dimensions so runs are comparable.
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioOutcome {
    pub id: String,
    pub engine: String,
    pub model: String,
    pub isolation: String,
    pub check_kind: String,
    pub verdict: Verdict,
    /// Why — a failing assertion, the judge's sentence, or the transport
    /// error that made the run inconclusive.
    pub rationale: String,
    pub duration_ms: u64,
    pub cost_usd: f64,
    /// Names of the tools the agent actually called, in order.
    pub tools: Vec<String>,
    pub work_dir: PathBuf,
}

/// A whole agentic run.
#[derive(Debug, Clone, Serialize)]
pub struct AgenticRun {
    pub engine: String,
    pub model: String,
    pub isolation: String,
    pub judge_model: String,
    pub ran_at: chrono::DateTime<chrono::Utc>,
    pub results: Vec<ScenarioOutcome>,
}

impl AgenticRun {
    /// Scenarios that returned a real verdict (PASS or FAIL).
    #[must_use]
    pub fn conclusive(&self) -> usize {
        self.results.iter().filter(|r| r.verdict != Verdict::Inconclusive).count()
    }

    #[must_use]
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.verdict == Verdict::Pass).count()
    }

    #[must_use]
    pub fn inconclusive(&self) -> usize {
        self.results.iter().filter(|r| r.verdict == Verdict::Inconclusive).count()
    }

    /// Pass rate over **conclusive** scenarios only. An inconclusive
    /// scenario is missing data, not a failure — folding it into the
    /// denominator would let a broken judge quietly tank the number.
    /// 0 conclusive → 0.0 (never NaN; these get serialised and compared).
    #[must_use]
    #[allow(clippy::cast_precision_loss, reason = "scenario counts are single digits; f64 is exact well past that")]
    pub fn pass_rate(&self) -> f64 {
        let c = self.conclusive();
        if c == 0 {
            0.0
        } else {
            self.passed() as f64 / c as f64
        }
    }

    #[must_use]
    pub fn total_cost_usd(&self) -> f64 {
        self.results.iter().map(|r| r.cost_usd).sum()
    }

    /// One JSON object per line — the streaming record format, matching
    /// `EngineMatrixRun::to_jsonl`.
    ///
    /// # Errors
    /// Propagates a `serde_json` failure if a record can't be serialised.
    pub fn to_jsonl(&self) -> Result<String> {
        let mut out = String::new();
        for r in &self.results {
            out.push_str(&serde_json::to_string(r).context("serialising ScenarioOutcome")?);
            out.push('\n');
        }
        Ok(out)
    }

    /// Human per-scenario table, in the style of `Score::render_table`.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "wall-clock ms rendered to one decimal; precision beyond 2^52 ms is not a thing"
    )]
    pub fn render_table(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(out, "smooth-bench agentic — workflow/action benchmark");
        let _ = writeln!(out, "  engine/model:  {} / {}", self.engine, self.model);
        let _ = writeln!(out, "  isolation:     {}", self.isolation);
        let _ = writeln!(out, "  judge model:   {}", self.judge_model);
        let _ = writeln!(out, "  ran at:        {}", self.ran_at.to_rfc3339());
        let _ = writeln!(out);
        let _ = writeln!(out, "  scenario                  check          verdict        time     $cost  tools");
        for r in &self.results {
            let _ = writeln!(
                out,
                "  {id:<25} {kind:<14} {verdict:<14} {secs:>5.1}s  {cost:>7.4}  {tools}",
                id = r.id,
                kind = r.check_kind,
                verdict = r.verdict.as_str(),
                secs = r.duration_ms as f64 / 1000.0,
                cost = r.cost_usd,
                tools = if r.tools.is_empty() { "-".to_string() } else { r.tools.join(",") },
            );
            if !r.rationale.trim().is_empty() {
                let _ = writeln!(out, "      ↳ {}", r.rationale.trim());
            }
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "  pass rate: {:.1}%  ({}/{} conclusive){}",
            self.pass_rate() * 100.0,
            self.passed(),
            self.conclusive(),
            if self.inconclusive() > 0 {
                format!(", {} INCONCLUSIVE", self.inconclusive())
            } else {
                String::new()
            }
        );
        let _ = writeln!(out, "  total cost: ${:.4}", self.total_cost_usd());
        out
    }
}

/// Result of evaluating one assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertResult {
    pub ok: bool,
    pub detail: String,
}

/// Evaluate deterministic assertions against the final `workspace`
/// state. `seeded` maps workspace-relative path → the content `setup`
/// wrote, so `unchanged` can be checked exactly.
///
/// Pure over the filesystem: no LLM, no network. This is the whole
/// reason deterministic checks are preferred.
#[must_use]
pub fn evaluate(asserts: &[Assertion], workspace: &Path, seeded: &BTreeMap<String, String>) -> Vec<AssertResult> {
    asserts.iter().map(|a| evaluate_one(a, workspace, seeded)).collect()
}

fn evaluate_one(a: &Assertion, workspace: &Path, seeded: &BTreeMap<String, String>) -> AssertResult {
    let path = workspace.join(&a.file);
    let content = std::fs::read_to_string(&path).ok();

    if a.missing {
        return AssertResult {
            ok: content.is_none(),
            detail: if content.is_none() {
                format!("{} absent, as required", a.file)
            } else {
                format!("{} exists but must not", a.file)
            },
        };
    }

    let Some(content) = content else {
        return AssertResult {
            ok: false,
            detail: format!("{} does not exist", a.file),
        };
    };

    if a.unchanged {
        let seed = seeded.get(&a.file);
        let ok = seed.is_some_and(|s| *s == content);
        return AssertResult {
            ok,
            detail: if ok {
                format!("{} untouched, as required", a.file)
            } else {
                format!("{} was modified but must not be", a.file)
            },
        };
    }

    // Narrow to a JSON pointer when asked, otherwise the whole file.
    let target = match &a.pointer {
        Some(ptr) => {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
                return AssertResult {
                    ok: false,
                    detail: format!("{} is not valid JSON, so pointer {ptr} can't be read", a.file),
                };
            };
            match v.pointer(ptr) {
                Some(found) => found.as_str().map_or_else(|| found.to_string(), ToString::to_string),
                None => {
                    return AssertResult {
                        ok: false,
                        detail: format!("{}{ptr} not found", a.file),
                    }
                }
            }
        }
        None => content,
    };

    let mut failures = Vec::new();
    if let Some(want) = &a.equals {
        if target.trim() != want.trim() {
            failures.push(format!("expected {want:?}, got {:?}", crate::judge::truncate(target.trim(), 120)));
        }
    }
    if let Some(want) = &a.contains {
        if !target.contains(want.as_str()) {
            failures.push(format!("missing {want:?}"));
        }
    }
    if let Some(unwanted) = &a.not_contains {
        if target.contains(unwanted.as_str()) {
            failures.push(format!("contains {unwanted:?} but must not"));
        }
    }

    let where_ = a.pointer.as_ref().map_or_else(|| a.file.clone(), |p| format!("{}{p}", a.file));
    AssertResult {
        ok: failures.is_empty(),
        detail: if failures.is_empty() {
            format!("{where_} ok")
        } else {
            format!("{where_}: {}", failures.join("; "))
        },
    }
}

/// Knobs for one agentic run.
#[derive(Debug, Clone)]
pub struct AgenticOpts {
    pub engine: Engine,
    pub model: String,
    pub isolation: Isolation,
    pub judge_model: String,
    pub gateway_url: String,
    pub gateway_key: Option<String>,
    /// Root for scenario scratch dirs (each gets `<root>/<id>/work`).
    pub runs_root: PathBuf,
}

/// Run every scenario: seed the workspace, boot the engine rooted there,
/// drive one canonical turn with the scenario prompt, then score.
///
/// A boot or transport failure marks that scenario `INCONCLUSIVE` and the
/// run continues — one dead VM doesn't abort the suite.
///
/// # Errors
/// Errors only when the scratch dirs can't be created; everything else is
/// captured as a per-scenario verdict.
pub async fn run_agentic<B: WorkspaceBooter + ?Sized>(scenarios: &[Scenario], booter: &B, opts: &AgenticOpts) -> Result<AgenticRun> {
    let mut results = Vec::with_capacity(scenarios.len());
    for s in scenarios {
        eprintln!("agentic: {} …", s.id);
        results.push(run_one(s, booter, opts).await);
    }
    Ok(AgenticRun {
        engine: opts.engine.as_str().to_string(),
        model: opts.model.clone(),
        isolation: opts.isolation.as_str().to_string(),
        judge_model: opts.judge_model.clone(),
        ran_at: chrono::Utc::now(),
        results,
    })
}

/// Run + score one scenario. Never returns `Err` — every failure mode is
/// a verdict, because a suite that aborts halfway tells you nothing.
async fn run_one<B: WorkspaceBooter + ?Sized>(s: &Scenario, booter: &B, opts: &AgenticOpts) -> ScenarioOutcome {
    let base = opts.runs_root.join(&s.id);
    let work = base.join("work");
    // Logs sit OUTSIDE the workspace so VM output can't pollute the
    // scored state (or get read back by the agent).
    let logs = base.join("log");

    let mut outcome = ScenarioOutcome {
        id: s.id.clone(),
        engine: opts.engine.as_str().to_string(),
        model: opts.model.clone(),
        isolation: opts.isolation.as_str().to_string(),
        check_kind: s.check.kind().to_string(),
        verdict: Verdict::Inconclusive,
        rationale: String::new(),
        duration_ms: 0,
        cost_usd: 0.0,
        tools: Vec::new(),
        work_dir: work.clone(),
    };

    let seeded = match seed_workspace(s, &work) {
        Ok(m) => m,
        Err(e) => {
            outcome.rationale = format!("workspace seeding failed: {e:#}");
            return outcome;
        }
    };

    let t0 = Instant::now();
    let server = match booter.boot_workspace(opts.engine, &opts.model, &work, &logs).await {
        Ok(b) => b,
        Err(e) => {
            outcome.duration_ms = t0.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            outcome.rationale = format!("engine boot failed: {e:#}");
            return outcome;
        }
    };

    let driven = run_via_canonical(&server.url, &s.prompt, server.token.as_deref(), crate::turn_deadline()).await;
    // Tear the engine/VM down before scoring so nothing can race the
    // assertions against a still-writing agent.
    drop(server);
    outcome.duration_ms = t0.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    let turn = match driven {
        Ok(t) => t,
        Err(e) => {
            outcome.rationale = format!("turn failed: {e}");
            return outcome;
        }
    };
    outcome.cost_usd = turn.cost;
    outcome.tools = turn.tool_calls.iter().map(|t| t.name.clone()).collect();

    match &s.check {
        Check::Deterministic { asserts } => {
            let results = evaluate(asserts, &work, &seeded);
            let failed: Vec<&AssertResult> = results.iter().filter(|r| !r.ok).collect();
            outcome.verdict = if failed.is_empty() { Verdict::Pass } else { Verdict::Fail };
            outcome.rationale = if failed.is_empty() {
                format!("{} assertion(s) held", results.len())
            } else {
                failed.iter().map(|r| r.detail.clone()).collect::<Vec<_>>().join(" | ")
            };
        }
        Check::Judge { rubric } => {
            let evidence = JudgeEvidence {
                rubric: rubric.clone(),
                prompt: s.prompt.clone(),
                transcript: turn
                    .tool_calls
                    .iter()
                    .map(|t| format!("{} -> {}", t.name, if t.success { "ok" } else { "error" }))
                    .collect::<Vec<_>>()
                    .join("\n"),
                final_response: turn.text.clone(),
                workspace: dump_workspace(&work),
            };
            match judge(&opts.gateway_url, opts.gateway_key.as_deref(), &opts.judge_model, &evidence).await {
                Ok(v) => {
                    outcome.verdict = if v.passed { Verdict::Pass } else { Verdict::Fail };
                    outcome.rationale = v.reason;
                }
                // Never a silent PASS: a broken judge is missing data.
                Err(e) => outcome.rationale = format!("judge inconclusive: {e:#}"),
            }
        }
    }
    outcome
}

/// Create the workspace and write the scenario's setup files. Returns
/// path → content so `unchanged` assertions can compare exactly.
fn seed_workspace(s: &Scenario, work: &Path) -> Result<BTreeMap<String, String>> {
    // Fresh dir per run — a leftover from a previous run would make
    // assertions pass against stale state.
    if work.exists() {
        std::fs::remove_dir_all(work).with_context(|| format!("clearing {}", work.display()))?;
    }
    std::fs::create_dir_all(work).with_context(|| format!("mkdir {}", work.display()))?;
    let mut seeded = BTreeMap::new();
    for f in &s.setup {
        anyhow::ensure!(is_safe_relative(&f.path), "setup path {:?} escapes the workspace", f.path);
        let dst = work.join(&f.path);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        }
        std::fs::write(&dst, &f.content).with_context(|| format!("writing {}", dst.display()))?;
        seeded.insert(f.path.clone(), f.content.clone());
    }
    Ok(seeded)
}

/// Flatten the workspace into `path:\n<contents>` blocks for the judge.
/// Bounded per file and in total — evidence, not an archive.
fn dump_workspace(work: &Path) -> String {
    use std::fmt::Write;
    let mut files = Vec::new();
    collect_files(work, work, &mut files);
    files.sort();
    let mut out = String::new();
    for rel in files.iter().take(40) {
        let body = std::fs::read_to_string(work.join(rel)).unwrap_or_else(|e| format!("<unreadable: {e}>"));
        let _ = writeln!(out, "--- {}\n{}", rel.display(), crate::judge::truncate(&body, 4_000));
    }
    if out.is_empty() {
        "(the workspace is empty)".to_string()
    } else {
        out
    }
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_files(root, &p, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_path_buf());
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::float_cmp,
    reason = "unwrap is the idiom for test assertions; the float compares are exact-by-construction 0.0 guards"
)]
mod tests {
    use super::*;

    fn outcome(id: &str, verdict: Verdict, cost: f64) -> ScenarioOutcome {
        ScenarioOutcome {
            id: id.into(),
            engine: "rust".into(),
            model: "deepseek-v4-flash".into(),
            isolation: "microvm".into(),
            check_kind: "deterministic".into(),
            verdict,
            rationale: "because".into(),
            duration_ms: 1234,
            cost_usd: cost,
            tools: vec!["read_file".into()],
            work_dir: PathBuf::from("/tmp/x"),
        }
    }

    fn run_with(results: Vec<ScenarioOutcome>) -> AgenticRun {
        AgenticRun {
            engine: "rust".into(),
            model: "deepseek-v4-flash".into(),
            isolation: "microvm".into(),
            judge_model: "deepseek-v4-flash".into(),
            ran_at: chrono::Utc::now(),
            results,
        }
    }

    // ---- scenario parsing ------------------------------------------------

    #[test]
    fn embedded_scenarios_parse_and_validate() {
        let s = default_scenarios().unwrap();
        assert!(s.len() >= 3, "the suite ships at least 3 scenarios, got {}", s.len());
        // At least one judged scenario so the judge path is exercised.
        assert!(s.iter().any(|x| matches!(x.check, Check::Judge { .. })), "no judged scenario in the suite");
        // …and the rest deterministic — judged checks are the exception.
        assert!(s.iter().filter(|x| matches!(x.check, Check::Deterministic { .. })).count() >= 2);
        // Every scenario seeds something and has a real prompt.
        for x in &s {
            assert!(!x.prompt.trim().is_empty(), "{} has no prompt", x.id);
            assert!(!x.description.trim().is_empty(), "{} has no description", x.id);
        }
    }

    #[test]
    fn embedded_suite_has_a_negative_scenario() {
        // The "don't do the wrong thing" guard: at least one scenario
        // asserts a seeded file is untouched.
        let s = default_scenarios().unwrap();
        let has_unchanged = s.iter().any(|x| match &x.check {
            Check::Deterministic { asserts } => asserts.iter().any(|a| a.unchanged),
            Check::Judge { .. } => false,
        });
        assert!(has_unchanged, "no negative (must-not-mutate) scenario in the suite");
    }

    #[test]
    fn parses_a_minimal_deterministic_scenario() {
        let s = parse_scenarios(
            r#"
[[scenario]]
id = "x"
description = "d"
prompt = "do the thing"
[[scenario.setup]]
path = "state.json"
content = "[]"
[scenario.check]
kind = "deterministic"
[[scenario.check.asserts]]
file = "state.json"
contains = "hello"
"#,
        )
        .unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(
            s[0].setup,
            vec![SetupFile {
                path: "state.json".into(),
                content: "[]".into()
            }]
        );
        let Check::Deterministic { asserts } = &s[0].check else {
            panic!("expected deterministic")
        };
        assert_eq!(asserts[0].contains.as_deref(), Some("hello"));
    }

    #[test]
    fn parses_a_judge_scenario() {
        let s = parse_scenarios(
            r#"
[[scenario]]
id = "j"
prompt = "summarise"
[scenario.check]
kind = "judge"
rubric = "must write a summary"
"#,
        )
        .unwrap();
        assert!(matches!(&s[0].check, Check::Judge { rubric } if rubric == "must write a summary"));
        assert_eq!(s[0].check.kind(), "judge");
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = parse_scenarios(
            r#"
[[scenario]]
id = "x"
prompt = "p"
tyop = "silently ignored otherwise"
[scenario.check]
kind = "judge"
rubric = "r"
"#,
        )
        .unwrap_err();
        // `{:#}` walks the anyhow chain down to serde's message.
        let err = format!("{err:#}");
        assert!(err.contains("tyop") || err.contains("unknown field"), "{err}");
    }

    #[test]
    fn rejects_duplicate_ids_empty_prompts_and_empty_asserts() {
        let dup = parse_scenarios(
            r#"
[[scenario]]
id = "x"
prompt = "p"
[scenario.check]
kind = "judge"
rubric = "r"
[[scenario]]
id = "x"
prompt = "p"
[scenario.check]
kind = "judge"
rubric = "r"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(dup.contains("duplicate scenario id"), "{dup}");

        let empty_prompt = parse_scenarios("[[scenario]]\nid=\"x\"\nprompt=\"  \"\n[scenario.check]\nkind=\"judge\"\nrubric=\"r\"\n")
            .unwrap_err()
            .to_string();
        assert!(empty_prompt.contains("empty prompt"), "{empty_prompt}");

        let no_asserts = parse_scenarios("[[scenario]]\nid=\"x\"\nprompt=\"p\"\n[scenario.check]\nkind=\"deterministic\"\nasserts=[]\n")
            .unwrap_err()
            .to_string();
        assert!(no_asserts.contains("no assertions"), "{no_asserts}");

        let empty_rubric = parse_scenarios("[[scenario]]\nid=\"x\"\nprompt=\"p\"\n[scenario.check]\nkind=\"judge\"\nrubric=\" \"\n")
            .unwrap_err()
            .to_string();
        assert!(empty_rubric.contains("empty judge rubric"), "{empty_rubric}");

        // A deterministic check with no `asserts` key at all is a serde
        // missing-field error, which must still surface readably.
        let no_key = format!(
            "{:#}",
            parse_scenarios("[[scenario]]\nid=\"x\"\nprompt=\"p\"\n[scenario.check]\nkind=\"deterministic\"\n").unwrap_err()
        );
        assert!(no_key.contains("missing field") && no_key.contains("asserts"), "{no_key}");
    }

    #[test]
    fn rejects_an_assertion_that_asserts_nothing() {
        let err =
            parse_scenarios("[[scenario]]\nid=\"x\"\nprompt=\"p\"\n[scenario.check]\nkind=\"deterministic\"\n[[scenario.check.asserts]]\nfile=\"a.json\"\n")
                .unwrap_err()
                .to_string();
        assert!(err.contains("asserts nothing"), "{err}");
    }

    #[test]
    fn rejects_unchanged_on_a_file_setup_never_seeded() {
        let err = parse_scenarios(
            "[[scenario]]\nid=\"x\"\nprompt=\"p\"\n[scenario.check]\nkind=\"deterministic\"\n[[scenario.check.asserts]]\nfile=\"a.json\"\nunchanged=true\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("setup never seeded"), "{err}");
    }

    #[test]
    fn rejects_paths_that_escape_the_workspace() {
        for bad in ["/etc/passwd", "../../secrets", "a/../../b"] {
            let toml = format!(
                "[[scenario]]\nid=\"x\"\nprompt=\"p\"\n[[scenario.setup]]\npath={bad:?}\ncontent=\"\"\n[scenario.check]\nkind=\"judge\"\nrubric=\"r\"\n"
            );
            let err = parse_scenarios(&toml).unwrap_err().to_string();
            assert!(err.contains("escapes the workspace"), "{bad} should be rejected, got: {err}");
        }
        assert!(is_safe_relative("a/b/c.json"));
        assert!(!is_safe_relative(""));
    }

    #[test]
    fn rejects_an_empty_scenario_file() {
        assert!(parse_scenarios("").unwrap_err().to_string().contains("no [[scenario]] entries"));
    }

    // ---- deterministic evaluator -----------------------------------------

    fn ws(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (p, c) in files {
            let dst = d.path().join(p);
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::write(dst, c).unwrap();
        }
        d
    }

    #[test]
    fn contains_and_not_contains_over_whole_file() {
        let d = ws(&[("watchlist.json", r#"[{"title":"Severance"},{"title":"Andor"}]"#)]);
        let asserts = vec![
            Assertion {
                file: "watchlist.json".into(),
                contains: Some("Severance".into()),
                ..Default::default()
            },
            Assertion {
                file: "watchlist.json".into(),
                not_contains: Some("Friends".into()),
                ..Default::default()
            },
            Assertion {
                file: "watchlist.json".into(),
                contains: Some("Missing Show".into()),
                ..Default::default()
            },
        ];
        let r = evaluate(&asserts, d.path(), &BTreeMap::new());
        assert!(r[0].ok, "{:?}", r[0]);
        assert!(r[1].ok, "{:?}", r[1]);
        assert!(!r[2].ok);
        assert!(r[2].detail.contains("Missing Show"), "{:?}", r[2]);
    }

    #[test]
    fn json_pointer_narrows_the_target() {
        let d = ws(&[("state.json", r#"{"items":[{"id":"a","n":7}]}"#)]);
        let ok = Assertion {
            file: "state.json".into(),
            pointer: Some("/items/0/id".into()),
            equals: Some("a".into()),
            ..Default::default()
        };
        // Numbers stringify, so `equals` works for them too.
        let num = Assertion {
            file: "state.json".into(),
            pointer: Some("/items/0/n".into()),
            equals: Some("7".into()),
            ..Default::default()
        };
        let bad_ptr = Assertion {
            file: "state.json".into(),
            pointer: Some("/items/9/id".into()),
            equals: Some("a".into()),
            ..Default::default()
        };
        let r = evaluate(&[ok, num, bad_ptr], d.path(), &BTreeMap::new());
        assert!(r[0].ok, "{:?}", r[0]);
        assert!(r[1].ok, "{:?}", r[1]);
        assert!(!r[2].ok);
        assert!(r[2].detail.contains("not found"), "{:?}", r[2]);
    }

    #[test]
    fn pointer_into_non_json_fails_loud_not_silently() {
        let d = ws(&[("notes.md", "just prose")]);
        let a = Assertion {
            file: "notes.md".into(),
            pointer: Some("/x".into()),
            equals: Some("y".into()),
            ..Default::default()
        };
        let r = evaluate(&[a], d.path(), &BTreeMap::new());
        assert!(!r[0].ok);
        assert!(r[0].detail.contains("not valid JSON"), "{:?}", r[0]);
    }

    #[test]
    fn missing_file_fails_unless_missing_was_asserted() {
        let d = ws(&[]);
        let want_content = Assertion {
            file: "nope.json".into(),
            contains: Some("x".into()),
            ..Default::default()
        };
        let want_absent = Assertion {
            file: "nope.json".into(),
            missing: true,
            ..Default::default()
        };
        let r = evaluate(&[want_content, want_absent], d.path(), &BTreeMap::new());
        assert!(!r[0].ok);
        assert!(r[0].detail.contains("does not exist"));
        assert!(r[1].ok, "{:?}", r[1]);
    }

    #[test]
    fn missing_fails_when_the_agent_created_the_file() {
        let d = ws(&[("should-not-exist.json", "{}")]);
        let a = Assertion {
            file: "should-not-exist.json".into(),
            missing: true,
            ..Default::default()
        };
        let r = evaluate(&[a], d.path(), &BTreeMap::new());
        assert!(!r[0].ok);
        assert!(r[0].detail.contains("must not"), "{:?}", r[0]);
    }

    /// The negative-scenario guard: `unchanged` must catch ANY mutation,
    /// including a delete, and must not pass on a file that was never
    /// seeded.
    #[test]
    fn unchanged_detects_mutation_deletion_and_unseeded_files() {
        let seed = "[{\"id\":1}]";
        let mut seeded = BTreeMap::new();
        seeded.insert("customers.json".to_string(), seed.to_string());

        let a = Assertion {
            file: "customers.json".into(),
            unchanged: true,
            ..Default::default()
        };

        // Untouched → pass.
        let same = ws(&[("customers.json", seed)]);
        assert!(evaluate(std::slice::from_ref(&a), same.path(), &seeded)[0].ok);

        // Mutated → fail.
        let mutated = ws(&[("customers.json", "[]")]);
        let r = evaluate(std::slice::from_ref(&a), mutated.path(), &seeded);
        assert!(!r[0].ok);
        assert!(r[0].detail.contains("modified"), "{:?}", r[0]);

        // Deleted → fail (file gone reads as "does not exist").
        let gone = ws(&[]);
        assert!(!evaluate(std::slice::from_ref(&a), gone.path(), &seeded)[0].ok);

        // Present but never seeded → fail, not a vacuous pass.
        assert!(!evaluate(std::slice::from_ref(&a), same.path(), &BTreeMap::new())[0].ok);
    }

    #[test]
    fn equals_trims_whitespace_on_both_sides() {
        let d = ws(&[("answer.txt", "  yes\n")]);
        let a = Assertion {
            file: "answer.txt".into(),
            equals: Some("yes".into()),
            ..Default::default()
        };
        assert!(evaluate(&[a], d.path(), &BTreeMap::new())[0].ok);
    }

    #[test]
    fn all_populated_predicates_must_hold_together() {
        let d = ws(&[("f.txt", "alpha beta")]);
        let a = Assertion {
            file: "f.txt".into(),
            contains: Some("alpha".into()),
            not_contains: Some("beta".into()),
            ..Default::default()
        };
        let r = evaluate(&[a], d.path(), &BTreeMap::new());
        assert!(!r[0].ok, "one failing predicate fails the assertion");
        assert!(r[0].detail.contains("must not"), "{:?}", r[0]);
    }

    // ---- seeding ---------------------------------------------------------

    #[test]
    fn seeding_writes_files_and_wipes_stale_state() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        // A leftover from a previous run…
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("stale.txt"), "old").unwrap();

        let s = Scenario {
            id: "x".into(),
            description: String::new(),
            prompt: "p".into(),
            setup: vec![SetupFile {
                path: "nested/state.json".into(),
                content: "[]".into(),
            }],
            check: Check::Judge { rubric: "r".into() },
        };
        let seeded = seed_workspace(&s, &work).unwrap();

        assert_eq!(std::fs::read_to_string(work.join("nested/state.json")).unwrap(), "[]");
        assert!(!work.join("stale.txt").exists(), "stale state must be wiped before a run");
        assert_eq!(seeded.get("nested/state.json").unwrap(), "[]");
    }

    #[test]
    fn workspace_dump_lists_files_and_marks_empty() {
        let d = ws(&[("a.json", "{\"k\":1}"), ("sub/b.md", "hello")]);
        let dump = dump_workspace(d.path());
        assert!(dump.contains("--- a.json"), "{dump}");
        assert!(dump.contains("--- sub/b.md"), "{dump}");
        assert!(dump.contains("hello"));

        let empty = tempfile::tempdir().unwrap();
        assert_eq!(dump_workspace(empty.path()), "(the workspace is empty)");
    }

    // ---- aggregation -----------------------------------------------------

    #[test]
    fn pass_rate_ignores_inconclusive_in_the_denominator() {
        let run = run_with(vec![
            outcome("a", Verdict::Pass, 0.01),
            outcome("b", Verdict::Fail, 0.02),
            outcome("c", Verdict::Inconclusive, 0.0),
            outcome("d", Verdict::Pass, 0.03),
        ]);
        assert_eq!(run.conclusive(), 3);
        assert_eq!(run.passed(), 2);
        assert_eq!(run.inconclusive(), 1);
        assert!((run.pass_rate() - 2.0 / 3.0).abs() < 1e-9);
        assert!((run.total_cost_usd() - 0.06).abs() < 1e-9);
    }

    #[test]
    fn pass_rate_is_zero_not_nan_when_everything_is_inconclusive() {
        let run = run_with(vec![outcome("a", Verdict::Inconclusive, 0.0)]);
        assert_eq!(run.pass_rate(), 0.0);
        assert!(!run.pass_rate().is_nan());
    }

    #[test]
    fn pass_rate_of_an_empty_run_is_zero() {
        let run = run_with(vec![]);
        assert_eq!(run.pass_rate(), 0.0);
        assert_eq!(run.to_jsonl().unwrap(), "");
    }

    #[test]
    fn jsonl_carries_one_record_per_scenario_with_all_dimensions() {
        let run = run_with(vec![outcome("a", Verdict::Pass, 0.01), outcome("b", Verdict::Fail, 0.02)]);
        let jsonl = run.to_jsonl().unwrap();
        assert_eq!(jsonl.lines().count(), 2);
        for line in jsonl.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            for key in ["id", "engine", "model", "isolation", "check_kind", "verdict", "cost_usd", "tools"] {
                assert!(v.get(key).is_some(), "record missing {key}: {line}");
            }
        }
        // Verdicts serialise as the uppercase strings the table shows.
        assert!(jsonl.contains("\"verdict\":\"PASS\""), "{jsonl}");
        assert!(jsonl.contains("\"verdict\":\"FAIL\""), "{jsonl}");
    }

    #[test]
    fn table_shows_every_scenario_and_flags_inconclusive() {
        let run = run_with(vec![
            outcome("watchlist-add", Verdict::Pass, 0.01),
            outcome("judge-thing", Verdict::Inconclusive, 0.0),
        ]);
        let t = run.render_table();
        assert!(t.contains("watchlist-add"), "{t}");
        assert!(t.contains("judge-thing"), "{t}");
        assert!(t.contains("INCONCLUSIVE"), "{t}");
        assert!(t.contains("1 INCONCLUSIVE"), "summary counts it: {t}");
        assert!(t.contains("(1/1 conclusive)"), "{t}");
        assert!(t.contains("microvm"), "isolation dimension is on the table: {t}");
    }

    #[test]
    fn table_omits_the_inconclusive_note_when_there_are_none() {
        let run = run_with(vec![outcome("a", Verdict::Pass, 0.0)]);
        let t = run.render_table();
        assert!(!t.contains("INCONCLUSIVE"), "{t}");
    }
}
