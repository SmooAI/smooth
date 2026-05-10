//! Render bench `result.json` records into self-contained `eval.html`
//! files for human review. Pearl th-2be27b.
//!
//! The reports are produced offline from existing `result.json` —
//! no network, no LLM, no live agent. Output is a single HTML file
//! per run with embedded CSS, plus an index page when the input
//! contains more than one run.
//!
//! Usage from the binary:
//!
//! ```text
//! smooth-bench eval-report --run-dir ~/.smooth/bench-runs
//! smooth-bench eval-report --run-dir ~/.smooth/bench-runs/<id>
//! ```
//!
//! Either form works — single-run dirs render `eval.html` next to
//! their `result.json`; sweep dirs (multiple subdirs) render one
//! per child plus a sibling `eval-report-<date>.html` index.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::BenchResult;

/// Produced by [`render_dir`]. Tells the caller which files were
/// written so the CLI can echo paths.
#[derive(Debug, Clone, Default)]
pub struct RenderOutcome {
    /// One per run-dir found.
    pub eval_paths: Vec<PathBuf>,
    /// `Some(path)` when an index roll-up was emitted (>= 2 runs).
    pub index_path: Option<PathBuf>,
}

/// Walk `root` for `result.json` files and emit `eval.html` next to
/// each. When more than one result is found, also write a sibling
/// `eval-report-<date>.html` summarizing the sweep.
///
/// `root` may be either a single run dir (containing `result.json`)
/// or a sweep dir (containing one subdirectory per run). The function
/// auto-detects.
pub fn render_dir(root: &Path) -> Result<RenderOutcome> {
    let results = discover_results(root)?;
    if results.is_empty() {
        anyhow::bail!("no result.json found under {}", root.display());
    }

    let mut outcome = RenderOutcome::default();
    let mut summaries: Vec<(BenchResult, PathBuf)> = Vec::with_capacity(results.len());

    for result_path in &results {
        let result = read_result(result_path)?;
        let html = render_eval_html(&result);
        let eval_path = result_path
            .parent()
            .map(|p| p.join("eval.html"))
            .ok_or_else(|| anyhow::anyhow!("result.json has no parent: {}", result_path.display()))?;
        std::fs::write(&eval_path, html).with_context(|| format!("writing {}", eval_path.display()))?;
        outcome.eval_paths.push(eval_path.clone());
        summaries.push((result, eval_path));
    }

    if summaries.len() >= 2 {
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let index_html = render_index_html(&summaries, &date);
        let index_path = root.join(format!("eval-report-{date}.html"));
        std::fs::write(&index_path, index_html).with_context(|| format!("writing {}", index_path.display()))?;
        outcome.index_path = Some(index_path);
    }

    Ok(outcome)
}

/// Discover `result.json` files under `root`. Looks at `root` itself
/// first (single-run case), then one level deeper (sweep case).
/// Limited to two levels to avoid pathological recursive walks if
/// the user points us at the wrong directory.
fn discover_results(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        anyhow::bail!("not a directory: {}", root.display());
    }
    let direct = root.join("result.json");
    if direct.is_file() {
        out.push(direct);
        return Ok(out);
    }
    for entry in std::fs::read_dir(root).with_context(|| format!("read_dir {}", root.display()))? {
        let entry = entry?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let candidate = entry.path().join("result.json");
        if candidate.is_file() {
            out.push(candidate);
        }
    }
    out.sort();
    Ok(out)
}

fn read_result(path: &Path) -> Result<BenchResult> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

/// HTML-escape `<`, `>`, `&`, `"`, `'`. Just enough for safe
/// embedding of bench-output strings (file paths, stdout snippets).
/// Not a full sanitizer — the input is bench data, not user-supplied.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render one run to HTML. Inline CSS so the file is self-contained
/// and viewable from a file:// URL.
pub(crate) fn render_eval_html(r: &BenchResult) -> String {
    let verdict = if r.solved { "PASS" } else { "FAIL" };
    let verdict_class = if r.solved { "pass" } else { "fail" };
    let llm_error_block = if let Some(err) = &r.llm_error {
        format!("<section class=\"err\"><h2>LLM Error</h2><pre>{}</pre></section>", escape(err))
    } else {
        String::new()
    };

    let tools_rows = if r.tool_calls.is_empty() {
        "<tr><td colspan=\"2\" class=\"empty\">no tool calls captured</td></tr>".to_string()
    } else {
        r.tool_calls
            .iter()
            .map(|t| {
                let cls = if t.success { "ok" } else { "ko" };
                format!(
                    "<tr><td class=\"name\">{}</td><td class=\"{cls}\">{}</td></tr>",
                    escape(&t.name),
                    if t.success { "ok" } else { "FAILED" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let model_row = r.model.as_deref().map(|m| format!("<dt>Model</dt><dd>{}</dd>", escape(m))).unwrap_or_default();
    let budget_row = r.budget_usd.map(|b| format!("<dt>Budget</dt><dd>${b:.2}</dd>")).unwrap_or_default();

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{task} — {verdict}</title>
<style>
:root {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace; line-height: 1.45; }}
body {{ margin: 2rem auto; max-width: 56rem; padding: 0 1rem; color: #111; background: #fafafa; }}
h1 {{ margin: 0 0 0.25rem; font-size: 1.5rem; }}
.banner {{ display: inline-block; padding: 0.25rem 0.75rem; border-radius: 0.25rem; font-weight: 700; margin-left: 0.75rem; vertical-align: middle; }}
.banner.pass {{ background: #d1fae5; color: #065f46; }}
.banner.fail {{ background: #fee2e2; color: #991b1b; }}
.lang {{ color: #555; font-size: 0.9rem; }}
section {{ margin-top: 1.75rem; }}
section h2 {{ font-size: 1.05rem; margin: 0 0 0.5rem; border-bottom: 1px solid #ddd; padding-bottom: 0.25rem; }}
dl {{ display: grid; grid-template-columns: 9rem 1fr; gap: 0.25rem 1rem; margin: 0; }}
dt {{ color: #555; }}
dd {{ margin: 0; }}
table {{ width: 100%; border-collapse: collapse; font-size: 0.9rem; }}
table td {{ padding: 0.25rem 0.5rem; border-bottom: 1px solid #eee; }}
.name {{ color: #1d4ed8; }}
.ok {{ color: #065f46; }}
.ko {{ color: #991b1b; }}
.empty {{ color: #888; font-style: italic; text-align: center; }}
pre {{ background: #111; color: #eee; padding: 0.75rem; border-radius: 0.25rem; overflow: auto; max-height: 24rem; font-size: 0.85rem; }}
.err pre {{ background: #7f1d1d; color: #fee2e2; }}
.meta {{ color: #555; font-size: 0.85rem; }}
</style>
</head>
<body>
<h1>{task}<span class="banner {verdict_class}">{verdict}</span></h1>
<div class="lang">{benchmark} · {lang} · run {run_id}</div>

<section>
  <h2>Metrics</h2>
  <dl>
    <dt>Result</dt><dd>{passed}/{total} tests passed</dd>
    <dt>Wall-clock</dt><dd>{duration_s:.1}s</dd>
    <dt>Cost</dt><dd>${cost_usd:.4}</dd>
    <dt>Tool calls</dt><dd>{tool_count}</dd>
    {model_row}
    {budget_row}
    <dt>Timestamp</dt><dd>{timestamp}</dd>
  </dl>
</section>

{llm_error_block}

<section>
  <h2>Tool calls</h2>
  <table>{tools_rows}</table>
</section>

<section>
  <h2>Test stdout</h2>
  <pre>{test_stdout}</pre>
</section>

<footer class="meta">Generated by <code>smooth-bench eval-report</code> · pearl th-2be27b</footer>
</body>
</html>
"#,
        task = escape(&r.task),
        verdict = verdict,
        verdict_class = verdict_class,
        benchmark = escape(&r.benchmark),
        lang = escape(&r.lang),
        run_id = escape(&r.run_id),
        passed = r.counts.passed,
        total = r.counts.total,
        duration_s = r.duration_s,
        cost_usd = r.cost_usd,
        tool_count = r.tool_calls.len(),
        model_row = model_row,
        budget_row = budget_row,
        timestamp = escape(&r.timestamp),
        llm_error_block = llm_error_block,
        tools_rows = tools_rows,
        test_stdout = escape(&r.test_stdout),
    )
}

/// Render a sweep-level index. Each row links to a sibling `eval.html`.
fn render_index_html(items: &[(BenchResult, PathBuf)], date: &str) -> String {
    let total = items.len();
    let passed = items.iter().filter(|(r, _)| r.solved).count();
    let total_cost: f64 = items.iter().map(|(r, _)| r.cost_usd).sum();
    let total_dur: f64 = items.iter().map(|(r, _)| r.duration_s).sum();

    let rows = items
        .iter()
        .map(|(r, eval_path)| {
            let cls = if r.solved { "pass" } else { "fail" };
            let verdict = if r.solved { "PASS" } else { "FAIL" };
            let rel = eval_path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| {
                    eval_path
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|s| s.to_str())
                        .map(|d| format!("{d}/{n}"))
                        .unwrap_or_else(|| n.to_string())
                })
                .unwrap_or_else(|| eval_path.display().to_string());
            format!(
                "<tr class=\"{cls}\"><td>{lang}</td><td><a href=\"{rel}\">{task}</a></td><td>{verdict}</td><td>{passed}/{total}</td><td>{dur:.1}s</td><td>${cost:.4}</td></tr>",
                cls = cls,
                rel = escape(&rel),
                lang = escape(&r.lang),
                task = escape(&r.task),
                verdict = verdict,
                passed = r.counts.passed,
                total = r.counts.total,
                dur = r.duration_s,
                cost = r.cost_usd,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Sweep eval report — {date}</title>
<style>
:root {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }}
body {{ margin: 2rem auto; max-width: 64rem; padding: 0 1rem; color: #111; background: #fafafa; }}
h1 {{ margin: 0 0 0.25rem; }}
.summary {{ background: #f3f4f6; padding: 0.75rem 1rem; border-radius: 0.25rem; margin: 1rem 0; }}
table {{ width: 100%; border-collapse: collapse; font-size: 0.9rem; }}
th, td {{ padding: 0.5rem; text-align: left; border-bottom: 1px solid #e5e7eb; }}
tr.pass td:nth-child(3) {{ color: #065f46; font-weight: 700; }}
tr.fail td:nth-child(3) {{ color: #991b1b; font-weight: 700; }}
a {{ color: #1d4ed8; text-decoration: none; }}
a:hover {{ text-decoration: underline; }}
</style>
</head>
<body>
<h1>Sweep eval report — {date}</h1>
<div class="summary">
  {passed}/{total} passed · ${cost:.4} total cost · {dur:.1}s wall-clock
</div>
<table>
  <thead><tr><th>Lang</th><th>Task</th><th>Verdict</th><th>Tests</th><th>Time</th><th>Cost</th></tr></thead>
  <tbody>
{rows}
  </tbody>
</table>
<footer style="margin-top: 1.5rem; color: #555; font-size: 0.85rem;">Generated by <code>smooth-bench eval-report</code> · pearl th-2be27b</footer>
</body>
</html>
"#,
        date = escape(date),
        passed = passed,
        total = total,
        cost = total_cost,
        dur = total_dur,
        rows = rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TestCounts, ToolCallRecord};

    fn fixture(name: &str, solved: bool) -> BenchResult {
        BenchResult {
            run_id: "0123abcd".to_string(),
            run_dir: PathBuf::from("/tmp/bench/0123abcd"),
            benchmark: "aider-polyglot".to_string(),
            task: name.to_string(),
            lang: "python".to_string(),
            timestamp: "2026-05-10T12:00:00Z".to_string(),
            model: Some("smooth-coding".to_string()),
            budget_usd: Some(5.0),
            counts: TestCounts {
                passed: if solved { 12 } else { 8 },
                failed: if solved { 0 } else { 4 },
                total: 12,
            },
            solved,
            duration_s: 42.0,
            cost_usd: 0.25,
            tool_calls: vec![
                ToolCallRecord {
                    name: "bash".to_string(),
                    success: true,
                },
                ToolCallRecord {
                    name: "edit_file".to_string(),
                    success: true,
                },
            ],
            llm_error: None,
            test_stdout: "============================= test session starts =============================\n12 passed in 0.05s".to_string(),
        }
    }

    #[test]
    fn escape_handles_html_metacharacters() {
        assert_eq!(escape("<script>"), "&lt;script&gt;");
        assert_eq!(escape("a & b"), "a &amp; b");
        assert_eq!(escape("\"x\""), "&quot;x&quot;");
        assert_eq!(escape("'y'"), "&#39;y&#39;");
        assert_eq!(escape("plain"), "plain");
    }

    #[test]
    fn render_eval_html_includes_pass_verdict_and_metrics() {
        let r = fixture("grade-school", true);
        let html = render_eval_html(&r);
        assert!(html.contains("PASS"), "PASS banner missing");
        assert!(html.contains("grade-school"), "task name missing");
        assert!(html.contains("12/12"), "test counts missing");
        assert!(html.contains("$0.2500"), "cost missing");
        assert!(html.contains("bash"), "tool call missing");
    }

    #[test]
    fn render_eval_html_marks_failure() {
        let mut r = fixture("forth", false);
        r.llm_error = Some("budget hit".to_string());
        let html = render_eval_html(&r);
        assert!(html.contains("FAIL"), "FAIL banner missing");
        assert!(html.contains("budget hit"), "llm_error missing");
        assert!(html.contains("8/12"), "partial counts missing");
    }

    #[test]
    fn render_eval_html_escapes_stdout() {
        // Test stdout with HTML-like content shouldn't break the render.
        let mut r = fixture("xml-doc", true);
        r.test_stdout = "expected <doc> got <body>".to_string();
        let html = render_eval_html(&r);
        assert!(html.contains("&lt;doc&gt;"), "stdout not escaped");
        assert!(!html.contains("<doc>"), "raw <doc> leaked");
    }

    #[test]
    fn render_dir_writes_eval_next_to_result_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = fixture("leap", true);
        std::fs::write(dir.path().join("result.json"), serde_json::to_string(&r).unwrap()).unwrap();

        let outcome = render_dir(dir.path()).expect("render");
        assert_eq!(outcome.eval_paths.len(), 1);
        let eval_html = std::fs::read_to_string(&outcome.eval_paths[0]).unwrap();
        assert!(eval_html.contains("PASS"));
        assert!(outcome.index_path.is_none(), "single run shouldn't get an index");
    }

    #[test]
    fn render_dir_emits_index_for_sweep() {
        let root = tempfile::tempdir().expect("tempdir");
        for (name, solved) in [("alpha", true), ("beta", false), ("gamma", true)] {
            let sub = root.path().join(name);
            std::fs::create_dir_all(&sub).unwrap();
            let r = fixture(name, solved);
            std::fs::write(sub.join("result.json"), serde_json::to_string(&r).unwrap()).unwrap();
        }
        let outcome = render_dir(root.path()).expect("render");
        assert_eq!(outcome.eval_paths.len(), 3);
        let index = outcome.index_path.expect("index");
        let body = std::fs::read_to_string(&index).unwrap();
        assert!(body.contains("alpha"));
        assert!(body.contains("beta"));
        assert!(body.contains("gamma"));
        assert!(body.contains("2/3 passed"), "summary count missing");
    }

    #[test]
    fn render_dir_errors_when_no_result_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = render_dir(dir.path()).unwrap_err();
        assert!(err.to_string().contains("no result.json"));
    }
}
