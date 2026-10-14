//! `th harness budget --report` — where a machine's Claude Code input tokens
//! actually go, from the transcripts Claude Code already writes
//! (SMOODEV-3891).
//!
//! Every model call resends the whole context, so spend is the sum of context
//! size over calls. The report buckets main-thread calls by the context they
//! carried, summarises the compactions (`compact_boundary` entries: tokens
//! before, the first call's context after), and estimates what a reset at the
//! configured budget would have saved. Read-only; reads only
//! `~/.claude/projects/*/*.jsonl` (or `$CLAUDE_CONFIG_DIR/projects`).

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::harness_compact::Budgets;

/// Context a session is assumed to resume at after a reset, for the estimate.
pub const RESUME_TOKENS: u64 = 30_000;
/// Bucket upper bounds (exclusive); the last bucket is open-ended.
const EDGES: [u64; 6] = [100_000, 200_000, 300_000, 500_000, 750_000, u64::MAX];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Bucket {
    pub label: String,
    pub calls: u64,
    pub input_tokens: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Compactions {
    pub count: u64,
    pub before_median: u64,
    pub after_median: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub days: u32,
    pub sessions: u64,
    pub calls: u64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub buckets: Vec<Bucket>,
    pub auto_compactions: Compactions,
    pub manual_compactions: Compactions,
    /// Input carried by calls whose context was already over their budget.
    pub input_over_budget: u64,
    /// Estimated input had every session reset at its budget (to [`RESUME_TOKENS`]).
    pub input_at_budget: u64,
    /// The global budget the estimate used (per-model overrides applied per call).
    pub budget: u64,
    /// Estimated input had every session taken the `/th-clear` suggestion
    /// the first time it was made (a reset at the suggest line).
    pub input_at_suggest: u64,
    /// The global suggest line that estimate used.
    pub suggest: u64,
}

/// One main-thread call or compaction boundary, as parsed from a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Call {
        id: String,
        context: u64,
        cache_read: u64,
        output: u64,
        model: Option<String>,
        session: Option<String>,
    },
    Compact {
        auto: bool,
        before: u64,
    },
}

/// Parse one transcript line. Sidechain (subagent) lines are skipped.
#[must_use]
pub fn parse_line(line: &str) -> Option<Event> {
    if !line.contains("\"usage\"") && !line.contains("compact_boundary") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if v.get("subtype").and_then(Value::as_str) == Some("compact_boundary") {
        let md = v.get("compactMetadata")?;
        return Some(Event::Compact {
            auto: md.get("trigger").and_then(Value::as_str) == Some("auto"),
            before: md.get("preTokens").and_then(Value::as_u64)?,
        });
    }
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let msg = v.get("message")?;
    let u = msg.get("usage")?;
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let context = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
    if context == 0 {
        return None;
    }
    Some(Event::Call {
        id: msg.get("id").and_then(Value::as_str)?.to_string(),
        context,
        cache_read: n("cache_read_input_tokens"),
        output: n("output_tokens"),
        model: msg.get("model").and_then(Value::as_str).filter(|m| !m.starts_with('<')).map(str::to_string),
        session: v.get("sessionId").and_then(Value::as_str).map(str::to_string),
    })
}

/// The context left after resetting at `budget` every time it is reached.
#[must_use]
pub const fn folded(context: u64, budget: u64) -> u64 {
    if budget <= RESUME_TOKENS || context <= budget {
        context
    } else {
        RESUME_TOKENS + (context - RESUME_TOKENS) % (budget - RESUME_TOKENS)
    }
}

fn median(v: &mut [u64]) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

fn label(i: usize) -> String {
    let k = |n: u64| format!("{}k", n / 1000);
    match i {
        0 => format!("<{}", k(EDGES[0])),
        i if i == EDGES.len() - 1 => format!("{}+", k(EDGES[i - 1])),
        i => format!("{}-{}", k(EDGES[i - 1]), k(EDGES[i])),
    }
}

/// Accumulates events from every transcript into a [`Report`].
pub struct Builder {
    report: Report,
    budgets: Budgets,
    seen: HashSet<String>,
    sessions: HashSet<String>,
    auto: (Vec<u64>, Vec<u64>),
    manual: (Vec<u64>, Vec<u64>),
}

impl Builder {
    #[must_use]
    pub fn new(days: u32, budgets: Budgets) -> Self {
        let report = Report {
            days,
            budget: budgets.global,
            suggest: budgets.suggest,
            buckets: (0..EDGES.len())
                .map(|i| Bucket {
                    label: label(i),
                    ..Bucket::default()
                })
                .collect(),
            ..Report::default()
        };
        Self {
            report,
            budgets,
            seen: HashSet::new(),
            sessions: HashSet::new(),
            auto: (Vec::new(), Vec::new()),
            manual: (Vec::new(), Vec::new()),
        }
    }

    /// Feed one transcript's events, in file order.
    pub fn file(&mut self, events: impl IntoIterator<Item = Event>, file_key: &str) {
        let mut pending: Option<(bool, u64)> = None;
        for e in events {
            match e {
                Event::Compact { auto, before } => pending = Some((auto, before)),
                Event::Call {
                    id,
                    context,
                    cache_read,
                    output,
                    model,
                    session,
                } => {
                    // Streamed turns repeat one message id; resumed sessions copy history.
                    if !self.seen.insert(id) {
                        continue;
                    }
                    if let Some((auto, before)) = pending.take() {
                        let side = if auto { &mut self.auto } else { &mut self.manual };
                        side.0.push(before);
                        side.1.push(context);
                    }
                    let r = &mut self.report;
                    self.sessions.insert(session.unwrap_or_else(|| file_key.to_string()));
                    r.calls += 1;
                    r.input_tokens += context;
                    r.cache_read_tokens += cache_read;
                    r.output_tokens += output;
                    let b = EDGES.iter().position(|&edge| context < edge).unwrap_or(EDGES.len() - 1);
                    r.buckets[b].calls += 1;
                    r.buckets[b].input_tokens += context;
                    let (budget, _) = self.budgets.for_session("claude-code", model.as_deref());
                    if budget > 0 && context > budget {
                        r.input_over_budget += context;
                    }
                    r.input_at_budget += if budget > 0 { folded(context, budget) } else { context };
                    let suggest = self.budgets.suggest_for("claude-code", model.as_deref());
                    r.input_at_suggest += if suggest > 0 { folded(context, suggest) } else { context };
                }
            }
        }
    }

    #[must_use]
    pub fn finish(mut self) -> Report {
        let stats = |(mut before, mut after): (Vec<u64>, Vec<u64>)| Compactions {
            count: before.len() as u64,
            before_median: median(&mut before),
            after_median: median(&mut after),
        };
        self.report.auto_compactions = stats(std::mem::take(&mut self.auto));
        self.report.manual_compactions = stats(std::mem::take(&mut self.manual));
        self.report.sessions = self.sessions.len() as u64;
        self.report
    }
}

/// `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`.
fn projects_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d).join("projects"));
    }
    Ok(dirs_next::home_dir().context("no home dir")?.join(".claude").join("projects"))
}

/// Transcripts under `dir` (one level of project dirs) modified since `since`.
fn transcripts(dir: &Path, since: SystemTime) -> Vec<PathBuf> {
    let Ok(projects) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = projects
        .filter_map(Result::ok)
        .filter_map(|p| fs::read_dir(p.path()).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter(|p| fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|m| m >= since))
        .collect();
    out.sort();
    out
}

/// Build the report over `dir`.
#[must_use]
pub fn build(dir: &Path, days: u32, budgets: Budgets) -> Report {
    let since = SystemTime::now() - Duration::from_secs(u64::from(days) * 86_400);
    let mut b = Builder::new(days, budgets);
    for path in transcripts(dir, since) {
        let Ok(f) = fs::File::open(&path) else { continue };
        let events = BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| parse_line(&l));
        b.file(events, &path.to_string_lossy());
    }
    b.finish()
}

fn human(n: u64) -> String {
    #[allow(clippy::cast_precision_loss, reason = "display only")]
    let f = n as f64;
    if n >= 1_000_000_000 {
        format!("{:.2}B", f / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", f / 1e6)
    } else {
        format!("{}k", n / 1000)
    }
}

fn pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0%".into();
    }
    #[allow(clippy::cast_precision_loss, reason = "display only")]
    let p = part as f64 * 100.0 / whole as f64;
    format!("{p:.0}%")
}

/// The human-readable report.
#[must_use]
pub fn render(r: &Report) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Claude Code main-thread usage, last {} days: {} sessions, {} model calls",
        r.days, r.sessions, r.calls
    );
    if r.calls == 0 {
        out.push_str("No transcripts with usage in that window.\n");
        return out;
    }
    let _ = writeln!(
        out,
        "input {} tokens ({} of it cache reads) · output {}\n",
        human(r.input_tokens),
        pct(r.cache_read_tokens, r.input_tokens),
        human(r.output_tokens)
    );
    let _ = writeln!(out, "{:<16}{:>10}{:>8}{:>10}{:>8}", "context at call", "calls", "share", "input", "share");
    for b in r.buckets.iter().filter(|b| b.calls > 0) {
        let _ = writeln!(
            out,
            "{:<16}{:>10}{:>8}{:>10}{:>8}",
            b.label,
            b.calls,
            pct(b.calls, r.calls),
            human(b.input_tokens),
            pct(b.input_tokens, r.input_tokens)
        );
    }
    out.push('\n');
    for (name, c) in [("auto", &r.auto_compactions), ("manual", &r.manual_compactions)] {
        if c.count > 0 {
            let _ = writeln!(
                out,
                "{name} compactions: {} (median {} before → {} after)",
                c.count,
                human(c.before_median),
                human(c.after_median)
            );
        }
    }
    if r.budget > 0 {
        let _ = writeln!(
            out,
            "\nbudget {} (harness.context_budget, per-model overrides applied): {} of input came from calls already over it",
            human(r.budget),
            pct(r.input_over_budget, r.input_tokens)
        );
        let _ = writeln!(
            out,
            "resetting there (to ~{}) would have used about {} fewer input tokens — an upper bound; it ignores re-reading after each reset",
            human(RESUME_TOKENS),
            pct(r.input_tokens.saturating_sub(r.input_at_budget), r.input_tokens)
        );
    } else {
        out.push_str("\nbudget off (harness.context_budget = 0)\n");
    }
    if r.suggest > 0 {
        let _ = writeln!(
            out,
            "taking the /th-clear suggestion at {} (harness.context_suggest) every time: about {} fewer — the same upper bound",
            human(r.suggest),
            pct(r.input_tokens.saturating_sub(r.input_at_suggest), r.input_tokens)
        );
    }
    out
}

/// `th harness budget --report`.
///
/// # Errors
/// No home directory, or JSON serialisation failure.
pub fn run(days: u32, json: bool) -> Result<()> {
    let report = build(&projects_dir()?, days, Budgets::from_settings());
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use crate::harness_compact::{Override, Reset};
    use tempfile::TempDir;

    fn call(id: &str, ctx: u64, model: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"s1","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":{},"cache_read_input_tokens":{},"output_tokens":7}}}}}}"#,
            ctx / 10,
            ctx - ctx / 10
        )
    }

    fn compact(trigger: &str, before: u64) -> String {
        format!(r#"{{"type":"system","subtype":"compact_boundary","compactMetadata":{{"trigger":"{trigger}","preTokens":{before}}}}}"#)
    }

    fn budgets(global: u64, overrides: &[&str]) -> Budgets {
        Budgets {
            global,
            warn: 0,
            overrides: overrides.iter().map(|o| Override::parse(o).unwrap()).collect(),
            reset: Reset::Compact,
            suggest: 0,
            suggest_overrides: Vec::new(),
        }
    }

    #[test]
    fn parse_line_reads_calls_and_boundaries_and_skips_the_rest() {
        let Some(Event::Call {
            id,
            context,
            cache_read,
            output,
            model,
            session,
        }) = parse_line(&call("m1", 100_000, "claude-opus-5-5"))
        else {
            panic!("call");
        };
        assert_eq!((id.as_str(), context, cache_read, output), ("m1", 100_000, 90_000, 7));
        assert_eq!(model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(session.as_deref(), Some("s1"));
        assert_eq!(parse_line(&compact("auto", 967_000)), Some(Event::Compact { auto: true, before: 967_000 }));
        assert_eq!(parse_line(&compact("manual", 1)), Some(Event::Compact { auto: false, before: 1 }));
        let side = call("m2", 5, "x").replace(r#""type":"assistant""#, r#""type":"assistant","isSidechain":true"#);
        assert_eq!(parse_line(&side), None);
        assert_eq!(parse_line(r#"{"type":"user","message":{"content":"hi"}}"#), None);
        assert_eq!(parse_line("garbage \"usage\""), None);
        assert_eq!(parse_line(r#"{"type":"assistant","message":{"id":"z","usage":{}}}"#), None, "zero context");
    }

    #[test]
    fn folded_resets_at_the_budget() {
        assert_eq!(folded(100_000, 250_000), 100_000);
        assert_eq!(folded(250_000, 250_000), 250_000);
        assert_eq!(folded(251_000, 250_000), 31_000);
        assert_eq!(folded(470_000, 250_000), 30_000);
        assert_eq!(folded(500_000, 20_000), 500_000, "a budget under the resume point can't fold");
    }

    #[test]
    fn builder_buckets_dedupes_and_pairs_compactions() {
        let mut b = Builder::new(30, budgets(250_000, &[]));
        let lines = [
            call("a", 50_000, "m"),
            call("a", 50_000, "m"), // streamed duplicate
            call("b", 600_000, "m"),
            compact("auto", 960_000),
            call("c", 90_000, "m"),
            compact("manual", 400_000),
            call("d", 100_000, "m"),
        ];
        b.file(lines.iter().filter_map(|l| parse_line(l)), "f1");
        let r = b.finish();
        assert_eq!(r.calls, 4);
        assert_eq!(r.sessions, 1);
        assert_eq!(r.input_tokens, 840_000);
        assert_eq!(r.output_tokens, 28);
        assert_eq!(r.buckets[0].calls, 2, "<100k: a, c");
        assert_eq!(r.buckets[1].calls, 1, "100-200k: d");
        assert_eq!(r.buckets[4].calls, 1, "500-750k: b");
        assert_eq!(
            r.auto_compactions,
            Compactions {
                count: 1,
                before_median: 960_000,
                after_median: 90_000
            }
        );
        assert_eq!(r.manual_compactions.count, 1);
        assert_eq!(r.manual_compactions.after_median, 100_000);
        assert_eq!(r.input_over_budget, 600_000);
        assert_eq!(r.input_at_budget, 50_000 + folded(600_000, 250_000) + 90_000 + 100_000);
    }

    #[test]
    fn builder_applies_per_model_budgets() {
        let mut b = Builder::new(1, budgets(250_000, &["*/claude-opus-*=500000"]));
        let lines = [call("a", 400_000, "claude-opus-5-5"), call("b", 400_000, "claude-sonnet-5-5")];
        b.file(lines.iter().filter_map(|l| parse_line(l)), "f");
        let r = b.finish();
        assert_eq!(r.input_over_budget, 400_000, "only the sonnet call is over its budget");
    }

    #[test]
    fn the_suggest_line_estimates_its_own_saving() {
        let mut with = budgets(300_000, &[]);
        with.suggest = 150_000;
        let mut b = Builder::new(1, with);
        b.file(parse_line(&call("a", 400_000, "m")), "f");
        let r = b.finish();
        assert_eq!(r.input_at_suggest, folded(400_000, 150_000));
        assert!(render(&r).contains("/th-clear suggestion at 150k"), "{}", render(&r));
        let mut b = Builder::new(1, budgets(300_000, &[]));
        b.file(parse_line(&call("a", 400_000, "m")), "f");
        assert!(!render(&b.finish()).contains("suggestion"), "suggest off → no line");
    }

    #[test]
    fn a_disabled_budget_estimates_no_savings() {
        let mut b = Builder::new(1, budgets(0, &[]));
        b.file(parse_line(&call("a", 900_000, "m")), "f");
        let r = b.finish();
        assert_eq!((r.input_over_budget, r.input_at_budget), (0, 900_000));
        assert!(render(&r).contains("budget off"));
    }

    #[test]
    fn build_reads_project_dirs_and_render_summarises() {
        let dir = TempDir::new().unwrap();
        let proj = dir.path().join("-Users-x-repo");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("s1.jsonl"),
            [call("a", 300_000, "m"), compact("auto", 980_000), call("b", 80_000, "m")].join("\n"),
        )
        .unwrap();
        fs::write(proj.join("notes.txt"), call("z", 1, "m")).unwrap();
        let r = build(dir.path(), 30, budgets(250_000, &[]));
        assert_eq!(r.calls, 2);
        let text = render(&r);
        assert!(text.contains("2 model calls"), "{text}");
        assert!(text.contains("auto compactions: 1"), "{text}");
        assert!(text.contains("fewer input tokens"), "{text}");
        let json: Value = serde_json::to_value(&r).unwrap();
        assert_eq!(json["auto_compactions"]["before_median"], 980_000);
    }

    #[test]
    fn an_empty_machine_renders_without_dividing_by_zero() {
        let dir = TempDir::new().unwrap();
        let r = build(&dir.path().join("missing"), 30, budgets(250_000, &[]));
        assert_eq!(r.calls, 0);
        assert!(render(&r).contains("No transcripts"));
        assert_eq!(pct(1, 0), "0%");
        assert_eq!(median(&mut []), 0);
    }

    #[test]
    fn labels_cover_every_bucket() {
        let labels: Vec<String> = (0..EDGES.len()).map(label).collect();
        assert_eq!(labels, ["<100k", "100k-200k", "200k-300k", "300k-500k", "500k-750k", "750k+"]);
    }
}
