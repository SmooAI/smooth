//! Per-task eval HTML renderer.
//!
//! Reads a bench run's artifacts and produces a single self-contained HTML
//! file showing what the operator + supervisor actually did:
//!
//! - Task brief (PROMPT.txt)
//! - Verdict (PASS / FAIL / INCONCLUSIVE — see `classify` below) with
//!   wall time, cost, supervisor steer count
//! - Pearl comment timeline: heartbeats, [STEERING:GUIDANCE] (highlighted),
//!   [METRICS], [IDLE], plain operator chat
//! - Tool-call summary count
//! - Test output (truncated to 4 KB if huge)
//!
//! Inputs:
//! - `<run_dir>/result.json` — `BenchResult`. Must exist.
//! - `<run_dir>/PROMPT.txt` — task brief. Optional.
//! - Pearl comments from `~/.smooth/dolt/` keyed on `result.pearl_id`.
//!   Best-effort; missing pearl just renders without the timeline.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use smooth_pearls::PearlComment;

use crate::BenchResult;

/// Verdict shown in the eval HTML. Distinguishes a real PASS / FAIL from
/// an HTTP-timeout starter-pass artifact (the same INCONCLUSIVE rule the
/// score sweep uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    /// Wall time hit `SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S * 1000` ms exactly
    /// AND the workspace was unmodified — polyglot starter passed/failed
    /// without the operator getting a chance to actually solve.
    Inconclusive,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Inconclusive => "INCONCLUSIVE",
        }
    }

    fn css_class(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Inconclusive => "note",
        }
    }
}

/// Apply the same INCONCLUSIVE heuristic the sweep uses: 300003 ms wall
/// time AND no supervisor steering (a steered run is by definition not a
/// starter-pass coincidence). Used for the per-task HTML's verdict pill.
pub fn classify(result: &BenchResult) -> Verdict {
    let timeout_ms = std::env::var("SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(300);
    let wall_ms = (result.duration_s * 1000.0) as u64;
    // 2 ms wiggle room on either side of the exact harness timeout.
    let near_timeout = wall_ms.abs_diff(timeout_ms * 1000) <= 5;
    let no_supervisor_activity = result.supervisor_steer_count.unwrap_or(0) == 0;
    if near_timeout && no_supervisor_activity {
        return Verdict::Inconclusive;
    }
    if result.solved {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// Categorize a pearl comment by its bracketed prefix so the HTML can
/// style heartbeats / steering / metrics differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    Progress,
    Steering,
    Metrics,
    Idle,
    Chat,
    Plain,
}

pub fn classify_comment(content: &str) -> CommentKind {
    let t = content.trim_start();
    if t.starts_with("[PROGRESS]") {
        CommentKind::Progress
    } else if t.starts_with("[STEERING:") {
        CommentKind::Steering
    } else if t.starts_with("[METRICS]") {
        CommentKind::Metrics
    } else if t.starts_with("[IDLE]") {
        CommentKind::Idle
    } else if t.starts_with("[CHAT") || t.starts_with("[QUESTION") || t.starts_with("[ANSWER") {
        CommentKind::Chat
    } else {
        CommentKind::Plain
    }
}

fn kind_class(k: CommentKind) -> &'static str {
    match k {
        CommentKind::Progress => "progress",
        CommentKind::Steering => "steering",
        CommentKind::Metrics => "metrics",
        CommentKind::Idle => "idle",
        CommentKind::Chat => "chat",
        CommentKind::Plain => "plain",
    }
}

/// Locate the global Dolt pearl store, falling back to env override.
/// Mirrors the bench's own `chat_driver::locate_pearl_store_dir`.
pub fn locate_pearl_store_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("SMOOTH_BENCH_PEARL_STORE") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
    }
    let global = dirs_next::home_dir().context("$HOME unset")?.join(".smooth").join("dolt");
    if global.exists() {
        return Ok(global);
    }
    anyhow::bail!("no .smooth/dolt found at SMOOTH_BENCH_PEARL_STORE or ~/.smooth/dolt")
}

/// Read the pearl's comments from the local Dolt store. Best-effort —
/// returns an empty Vec on any error so the renderer can still produce
/// HTML for runs where the pearl was deleted or the store moved.
fn read_pearl_comments(pearl_id: &str) -> Vec<PearlComment> {
    let dir = match locate_pearl_store_dir() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let store = match smooth_pearls::PearlStore::open(&dir) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    store.get_comments(pearl_id).unwrap_or_default()
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Truncate a string to roughly `max` bytes (char-boundary aware), adding
/// a "…(N more chars truncated)" suffix when it doesn't fit. Used to keep
/// long test stdouts and tool-result blobs from inflating the HTML.
fn truncate_for_html(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…({} more chars truncated)", &s[..end], s.len() - end)
}

/// Read result.json, PROMPT.txt, and pearl comments from a run dir,
/// then render the per-task HTML. Returns the HTML string; caller writes
/// it wherever (default: `<run_dir>/eval.html`).
pub fn render_run_html(run_dir: &Path) -> Result<String> {
    let result_path = run_dir.join("result.json");
    let result_bytes = std::fs::read(&result_path).with_context(|| format!("read {}", result_path.display()))?;
    let result: BenchResult = serde_json::from_slice(&result_bytes).with_context(|| format!("parse {}", result_path.display()))?;

    let prompt = std::fs::read_to_string(run_dir.join("PROMPT.txt")).unwrap_or_default();
    let comments = result.pearl_id.as_deref().map(read_pearl_comments).unwrap_or_default();
    let verdict = classify(&result);
    Ok(render_html(&result, &prompt, &comments, verdict))
}

/// Pure rendering function — separated from I/O so unit tests can verify
/// the output structure without touching the filesystem or the pearl store.
pub fn render_html(result: &BenchResult, prompt: &str, comments: &[PearlComment], verdict: Verdict) -> String {
    let title = format!("{}/{} — {} ({})", result.lang, result.task, verdict.label(), result.run_id);
    let pearl_id_str = result.pearl_id.as_deref().unwrap_or("—");
    let steer_count = result.supervisor_steer_count.unwrap_or(0);
    let cost = result.cost_usd;
    let wall_min = result.duration_s / 60.0;
    let n_comments = comments.len();
    let test_stdout = truncate_for_html(&result.test_stdout, 4096);

    let progress_count = comments.iter().filter(|c| classify_comment(&c.content) == CommentKind::Progress).count();
    let steering_in_pearl = comments.iter().filter(|c| classify_comment(&c.content) == CommentKind::Steering).count();

    let mut html = String::new();
    html.push_str(&format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title_esc}</title><style>{css}</style></head><body><div class=\"container\">",
        title_esc = html_escape(&title),
        css = STYLE
    ));

    // Header
    html.push_str(&format!(
        "<header><h1>{title}</h1><p class=\"meta\">Run <code>{run_id}</code> &middot; Pearl <code>{pearl_id}</code> &middot; Generated <code>{ts}</code></p></header>",
        title = html_escape(&format!("{} / {}", result.lang, result.task)),
        run_id = html_escape(&result.run_id),
        pearl_id = html_escape(pearl_id_str),
        ts = html_escape(&result.timestamp),
    ));

    // Stats row
    html.push_str("<div class=\"grid\">");
    html.push_str(&format!(
        "<div class=\"card stat {cls}\"><div class=\"label\">Verdict</div><div class=\"value\">{label}</div><div class=\"delta\">{passed}/{total} tests passed</div></div>",
        cls = verdict.css_class(),
        label = verdict.label(),
        passed = result.counts.passed,
        total = result.counts.total,
    ));
    html.push_str(&format!(
        "<div class=\"card stat\"><div class=\"label\">Wall time</div><div class=\"value\">{:.1} min</div><div class=\"delta\">{:.0} s</div></div>",
        wall_min, result.duration_s
    ));
    html.push_str(&format!(
        "<div class=\"card stat\"><div class=\"label\">Cost</div><div class=\"value\">${:.4}</div><div class=\"delta\">supervisor steers: {steer_count}</div></div>",
        cost
    ));
    html.push_str(&format!(
        "<div class=\"card stat\"><div class=\"label\">Pearl activity</div><div class=\"value\">{n_comments}</div><div class=\"delta\">{progress_count} PROGRESS &middot; {steering_in_pearl} STEERING</div></div>",
    ));
    html.push_str("</div>");

    // Task brief
    if !prompt.is_empty() {
        html.push_str("<h2>Task brief</h2>");
        html.push_str(&format!("<pre class=\"brief\">{}</pre>", html_escape(&truncate_for_html(prompt, 8192))));
    }

    // LLM error (if any)
    if let Some(err) = &result.llm_error {
        html.push_str("<h2>LLM error</h2>");
        html.push_str(&format!("<pre class=\"error\">{}</pre>", html_escape(err)));
    }

    // Pearl timeline
    html.push_str("<h2>Pearl timeline</h2>");
    if comments.is_empty() {
        html.push_str("<p class=\"meta\">No comments — the pearl is missing from the local store, or the run pre-dates pearl_id capture.</p>");
    } else {
        html.push_str("<table class=\"timeline\"><thead><tr><th>Time</th><th>Kind</th><th>Content</th></tr></thead><tbody>");
        for c in comments {
            let kind = classify_comment(&c.content);
            // Trim very long comments (tool-result blobs) to 1.5 KB to
            // keep the HTML manageable; full content is in the pearl.
            let content = truncate_for_html(c.content.trim_end(), 1500);
            html.push_str(&format!(
                "<tr class=\"{cls}\"><td class=\"ts\">{ts}</td><td class=\"kind\">{kind_label}</td><td class=\"content\"><pre>{content}</pre></td></tr>",
                cls = kind_class(kind),
                ts = c.created_at.format("%H:%M:%S"),
                kind_label = format!("{:?}", kind),
                content = html_escape(&content),
            ));
        }
        html.push_str("</tbody></table>");
    }

    // Tool calls summary (best-effort — chat_driver collects stub
    // PROGRESS calls; richer events.jsonl is a future workstream)
    html.push_str(&format!(
        "<h2>Tool-call summary</h2><p class=\"meta\">{} tool call(s) recorded by the bench-side observer. Per-call detail comes from <code>events.jsonl</code> (future workstream).</p>",
        result.tool_calls.len()
    ));

    // Test output
    html.push_str("<h2>Test output</h2>");
    html.push_str(&format!("<pre class=\"test-stdout\">{}</pre>", html_escape(&test_stdout)));

    html.push_str("</div></body></html>");
    html
}

const STYLE: &str = r#"
:root { --bg:#0c0d10; --panel:#16181d; --panel-2:#1c1f26; --line:#2a2e38;
    --text:#e6e8ec; --text-dim:#9ba3af; --pass:#4ade80; --fail:#f87171;
    --warn:#fbbf24; --accent:#60a5fa; --steer:#a78bfa; }
* { box-sizing: border-box; }
html, body { margin:0; padding:0; background:var(--bg); color:var(--text);
    font:14px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; }
.container { max-width: 1200px; margin: 0 auto; padding: 32px 24px 80px; }
header { border-bottom: 1px solid var(--line); padding-bottom: 24px; margin-bottom: 32px; }
h1 { font-size: 22px; margin: 0 0 8px; font-weight: 600; }
h2 { font-size: 16px; margin: 32px 0 12px; font-weight: 600; }
p { margin: 0 0 12px; }
.meta { color: var(--text-dim); font-size: 13px; }
.meta code { background: var(--panel); padding:1px 6px; border-radius:3px; color:var(--accent); }
header code { background: var(--panel); padding:1px 6px; border-radius:3px; color:var(--accent); }
.grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px; margin: 16px 0; }
.card { background: var(--panel); border: 1px solid var(--line); border-radius: 6px; padding: 16px; }
.stat .label { font-size: 12px; color: var(--text-dim); text-transform: uppercase; letter-spacing: 0.04em; margin-bottom: 4px; }
.stat .value { font-size: 24px; font-weight: 600; }
.stat .delta { font-size: 12px; color: var(--text-dim); margin-top: 4px; }
.stat.pass .value { color: var(--pass); }
.stat.fail .value { color: var(--fail); }
.stat.note .value { color: var(--warn); }
pre { background: var(--panel); border: 1px solid var(--line); border-radius: 6px;
    padding: 12px; overflow-x: auto; font: 12.5px/1.5 ui-monospace, "SF Mono", Menlo, monospace;
    white-space: pre-wrap; word-break: break-word; margin: 0; }
pre.brief { color: var(--text-dim); }
pre.error { color: var(--fail); border-color: var(--fail); }
pre.test-stdout { max-height: 600px; overflow-y: auto; }
table.timeline { width: 100%; border-collapse: collapse; font-size: 12.5px; margin: 12px 0; }
table.timeline th { text-align:left; padding:8px 10px; border-bottom:1px solid var(--line);
    color:var(--text-dim); font-size:11px; text-transform:uppercase; letter-spacing:0.04em; }
table.timeline td { padding:8px 10px; border-bottom:1px solid var(--line); vertical-align: top; }
table.timeline td.ts { color: var(--text-dim); white-space: nowrap; font-family: ui-monospace, "SF Mono", Menlo, monospace; }
table.timeline td.kind { white-space: nowrap; font-size: 11px; text-transform: uppercase; letter-spacing: 0.04em; }
table.timeline tr.steering td.kind { color: var(--steer); font-weight: 600; }
table.timeline tr.steering { background: rgba(167, 139, 250, 0.08); }
table.timeline tr.metrics td.kind { color: var(--accent); }
table.timeline tr.idle td.kind { color: var(--warn); }
table.timeline tr.progress td.kind { color: var(--text-dim); }
table.timeline pre { background: transparent; border: none; padding: 0; max-height: 300px; overflow-y: auto; }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestCounts;
    use chrono::Utc;
    use std::path::PathBuf;

    fn make_result(solved: bool, duration_s: f64, steer_count: Option<u32>) -> BenchResult {
        BenchResult {
            run_id: "test123".into(),
            run_dir: PathBuf::from("/tmp/test"),
            benchmark: "aider-polyglot".into(),
            task: "leap".into(),
            lang: "python".into(),
            timestamp: "2026-05-03T12:00:00Z".into(),
            model: None,
            budget_usd: Some(5.0),
            counts: TestCounts {
                passed: if solved { 16 } else { 0 },
                failed: if solved { 0 } else { 16 },
                total: 16,
            },
            solved,
            duration_s,
            cost_usd: 0.123,
            tool_calls: vec![],
            llm_error: None,
            test_stdout: "PASSED".into(),
            pearl_id: Some("th-aaaaaa".into()),
            supervisor_steer_count: steer_count,
        }
    }

    fn make_comment(prefix: &str, body: &str) -> PearlComment {
        PearlComment {
            id: "c1".into(),
            pearl_id: "th-aaaaaa".into(),
            content: format!("{prefix} {body}"),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn classify_comment_recognises_brackets() {
        assert_eq!(classify_comment("[PROGRESS] heartbeat #1"), CommentKind::Progress);
        assert_eq!(classify_comment("  [STEERING:GUIDANCE] try X"), CommentKind::Steering);
        assert_eq!(classify_comment("[METRICS] cost_usd=0.0"), CommentKind::Metrics);
        assert_eq!(classify_comment("[IDLE]"), CommentKind::Idle);
        assert_eq!(classify_comment("[CHAT:HELLO]"), CommentKind::Chat);
        assert_eq!(classify_comment("hello world"), CommentKind::Plain);
    }

    #[test]
    fn classify_pass_when_solved_and_real_run() {
        let r = make_result(true, 600.0, Some(2));
        assert_eq!(classify(&r), Verdict::Pass);
    }

    #[test]
    fn classify_fail_when_unsolved_and_real_run() {
        let r = make_result(false, 600.0, Some(0));
        assert_eq!(classify(&r), Verdict::Fail);
    }

    #[test]
    fn classify_inconclusive_at_http_timeout_with_no_steers() {
        // 300003 ms wall AND no supervisor activity → INCONCLUSIVE
        let r = make_result(true, 300.003, Some(0));
        assert_eq!(classify(&r), Verdict::Inconclusive);
    }

    #[test]
    fn classify_pass_at_timeout_when_supervisor_steered() {
        // Supervisor activity proves a real run, so a coincidental
        // 300003 ms wall is no longer INCONCLUSIVE.
        let r = make_result(true, 300.003, Some(1));
        assert_eq!(classify(&r), Verdict::Pass);
    }

    #[test]
    fn classify_uses_env_override() {
        std::env::set_var("SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S", "120");
        let r = make_result(true, 120.0, Some(0));
        assert_eq!(classify(&r), Verdict::Inconclusive);
        std::env::remove_var("SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S");
    }

    #[test]
    fn truncate_for_html_keeps_short_strings() {
        assert_eq!(truncate_for_html("hello", 100), "hello");
    }

    #[test]
    fn truncate_for_html_clips_long_strings_at_char_boundary() {
        let s = "a".repeat(200);
        let out = truncate_for_html(&s, 50);
        assert!(out.contains("…(150 more chars truncated)"));
        assert!(out.starts_with(&"a".repeat(50)));
    }

    #[test]
    fn html_escape_handles_special_chars() {
        assert_eq!(html_escape("<a href=\"x\">&'</a>"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;");
    }

    #[test]
    fn render_html_contains_key_sections() {
        let r = make_result(true, 600.0, Some(3));
        let comments = vec![make_comment("[PROGRESS]", "tick"), make_comment("[STEERING:GUIDANCE]", "try X")];
        let html = render_html(&r, "Solve leap years.", &comments, Verdict::Pass);

        assert!(html.contains("<title>"));
        assert!(html.contains("python / leap"));
        assert!(html.contains("PASS"));
        assert!(html.contains("Solve leap years."));
        assert!(html.contains("[PROGRESS]"));
        assert!(html.contains("[STEERING:GUIDANCE]"));
        assert!(html.contains("Tool-call summary"));
        assert!(html.contains("Test output"));
        assert!(html.contains("th-aaaaaa"));
        // Steer count surfaced in the cost card.
        assert!(html.contains("supervisor steers: 3"));
    }

    #[test]
    fn render_html_handles_missing_pearl_comments() {
        let r = make_result(false, 600.0, Some(0));
        let html = render_html(&r, "", &[], Verdict::Fail);
        assert!(html.contains("No comments"));
        assert!(html.contains("FAIL"));
    }

    #[test]
    fn render_html_shows_inconclusive_with_warn_class() {
        let r = make_result(true, 300.003, Some(0));
        let html = render_html(&r, "", &[], Verdict::Inconclusive);
        assert!(html.contains("INCONCLUSIVE"));
        assert!(html.contains("class=\"card stat note\""));
    }
}
