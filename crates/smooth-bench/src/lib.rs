//! Smooth benchmark harness.
//!
//! Internal tool — not part of the user-facing `th` binary. Run via
//! `cargo run -p smooai-smooth-bench --` or the top-level
//! `scripts/bench.sh` wrapper.
//!
//! Phase-1 MVP: **Aider Polyglot** single-task runs.
//!
//! Flow:
//!   1. Ensure the upstream dataset is cached at `~/.smooth/bench-cache/polyglot-benchmark/`.
//!      (Cloned on first use; reused after — the benchmark repo is static.)
//!   2. Copy the task's source + test + instruction files into a fresh
//!      scratch run dir at `~/.smooth/bench-runs/<run-id>/work/`.
//!   3. Invoke `th code --headless` against Big Smooth over WebSocket,
//!      capturing tool calls + cost via [`smooth_code::headless::run_headless_capture`].
//!   4. Run the language's test command in the scratch dir, count
//!      pass/fail.
//!   5. Write `result.json` and print a one-line summary.
//!
//! Not yet wired: batch mode (`--all`), parallelism, the web scoreboard,
//! SWE-bench, Terminal-Bench. Those are separate pearls.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

/// Where we cache the cloned polyglot-benchmark repo.
pub fn cache_root() -> anyhow::Result<PathBuf> {
    let home = dirs_next::home_dir().ok_or_else(|| anyhow!("no home dir"))?;
    Ok(home.join(".smooth").join("bench-cache"))
}

/// Where we put per-run scratch dirs + result.json.
pub fn runs_root() -> anyhow::Result<PathBuf> {
    let home = dirs_next::home_dir().ok_or_else(|| anyhow!("no home dir"))?;
    Ok(home.join(".smooth").join("bench-runs"))
}

const POLYGLOT_REPO: &str = "https://github.com/Aider-AI/polyglot-benchmark.git";
const POLYGLOT_DIR: &str = "polyglot-benchmark";

/// Supported Aider Polyglot languages. MVP handles Python + Rust;
/// the rest have known test-command shapes but aren't exercised yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolyglotLang {
    Python,
    Rust,
    Go,
    Javascript,
    Java,
    Cpp,
}

impl PolyglotLang {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "python" | "py" => Some(Self::Python),
            "rust" | "rs" => Some(Self::Rust),
            "go" => Some(Self::Go),
            "javascript" | "js" | "node" => Some(Self::Javascript),
            "java" => Some(Self::Java),
            "cpp" | "c++" | "cplusplus" => Some(Self::Cpp),
            _ => None,
        }
    }

    pub fn dataset_dir(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Javascript => "javascript",
            Self::Java => "java",
            Self::Cpp => "cpp",
        }
    }

    /// Shell command used to run the task's tests inside the
    /// scratch work dir. Split on whitespace; first element is the
    /// program.
    pub fn test_command(self) -> &'static [&'static str] {
        // Prefer per-test-case output over terse summaries — the
        // judge can count `PASS/FAIL` lines, but gets fooled by a
        // single `ok <package>` summary (undercounts a 31-case
        // suite as 1). Keep the commands verbose enough that the
        // summary names individual cases.
        match self {
            Self::Python => &["python3", "-m", "pytest", "-q"],
            // Default cargo test (not `--quiet`) emits per-test `test … ok`
            // lines plus the `test result: ok. N passed; N failed; …` summary.
            Self::Rust => &["cargo", "test"],
            // `-v` gives `--- PASS: TestName` / `--- FAIL:` per subtest,
            // instead of only the terminal `ok <package> <duration>` line.
            Self::Go => &["go", "test", "-v", "./..."],
            Self::Javascript => &["npm", "test"],
            Self::Java => &["gradle", "test"],
            Self::Cpp => &["sh", "-c", "mkdir -p build && cd build && cmake .. && make && ctest --output-on-failure"],
        }
    }
}

/// Counts parsed out of a test runner's output.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestCounts {
    pub passed: u32,
    pub failed: u32,
    pub total: u32,
}

impl TestCounts {
    pub fn solved(&self) -> bool {
        self.total > 0 && self.failed == 0 && self.passed == self.total
    }
}

/// Options for running a single Aider Polyglot task.
#[derive(Debug, Clone)]
pub struct BenchOpts {
    pub big_smooth_url: String,
    pub budget_usd: Option<f64>,
    pub model: Option<String>,
}

impl Default for BenchOpts {
    fn default() -> Self {
        Self {
            big_smooth_url: "http://localhost:4400".into(),
            budget_usd: Some(0.50),
            model: None,
        }
    }
}

/// Final result written to `<run-dir>/result.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchResult {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub benchmark: String,
    pub task: String,
    pub lang: String,
    pub timestamp: String,
    pub model: Option<String>,
    pub budget_usd: Option<f64>,
    pub counts: TestCounts,
    pub solved: bool,
    pub duration_s: f64,
    pub cost_usd: f64,
    pub tool_calls: Vec<ToolCallRecord>,
    pub llm_error: Option<String>,
    pub test_stdout: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub name: String,
    pub success: bool,
}

/// Run one Aider Polyglot task end-to-end.
///
/// # Errors
/// Returns an error for setup failures (dataset clone, scratch dir
/// creation, task not found). LLM-side errors are captured in the
/// `llm_error` field of the result rather than propagated, so a
/// 504-interrupted run still produces a scored result.
pub async fn run_aider_polyglot(lang: PolyglotLang, task: &str, opts: &BenchOpts) -> anyhow::Result<BenchResult> {
    ensure_dataset()?;
    let task_src = locate_task(lang, task)?;
    let run = new_run_dir()?;
    let work_dir = run.join("work");
    std::fs::create_dir_all(&work_dir).with_context(|| format!("mkdir {}", work_dir.display()))?;

    copy_task_files(&task_src, &work_dir)?;
    let prompt = build_prompt(task, lang, &work_dir)?;
    std::fs::write(run.join("PROMPT.txt"), &prompt)?;

    let t0 = Instant::now();

    let (cost_usd, tool_calls, llm_error) =
        match smooth_code::headless::run_headless_capture(&opts.big_smooth_url, work_dir.clone(), prompt.clone(), opts.model.clone(), opts.budget_usd).await {
            Ok(out) => (
                out.cost,
                out.tool_calls
                    .into_iter()
                    .map(|t| ToolCallRecord {
                        name: t.name,
                        success: t.success,
                    })
                    .collect(),
                None,
            ),
            Err(e) => (0.0, Vec::new(), Some(e.to_string())),
        };

    let duration_s = t0.elapsed().as_secs_f64();

    let (test_stdout, counts) = score_work_dir(lang, &work_dir).await?;

    let result = BenchResult {
        run_id: run.file_name().and_then(|s| s.to_str()).unwrap_or("unknown").to_string(),
        run_dir: run.clone(),
        benchmark: "aider-polyglot".into(),
        task: task.into(),
        lang: lang.dataset_dir().into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        model: opts.model.clone(),
        budget_usd: opts.budget_usd,
        counts,
        solved: counts.solved(),
        duration_s,
        cost_usd,
        tool_calls,
        llm_error,
        test_stdout,
    };

    let json = serde_json::to_string_pretty(&result)?;
    std::fs::write(run.join("result.json"), json)?;

    Ok(result)
}

fn ensure_dataset() -> anyhow::Result<()> {
    let root = cache_root()?;
    let repo = root.join(POLYGLOT_DIR);
    if repo.join(".git").is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(&root)?;
    let status = Command::new("git")
        .arg("clone")
        .arg("--depth=1")
        .arg(POLYGLOT_REPO)
        .arg(&repo)
        .status()
        .with_context(|| "spawning git clone")?;
    if !status.success() {
        return Err(anyhow!("git clone {POLYGLOT_REPO} failed"));
    }
    Ok(())
}

fn locate_task(lang: PolyglotLang, task: &str) -> anyhow::Result<PathBuf> {
    let repo = cache_root()?.join(POLYGLOT_DIR);
    let candidate = repo.join(lang.dataset_dir()).join("exercises").join("practice").join(task);
    if !candidate.is_dir() {
        return Err(anyhow!(
            "task '{task}' not found for language '{}' at {}",
            lang.dataset_dir(),
            candidate.display()
        ));
    }
    Ok(candidate)
}

fn new_run_dir() -> anyhow::Result<PathBuf> {
    let runs = runs_root()?;
    std::fs::create_dir_all(&runs)?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let short: String = id.chars().take(8).collect();
    let dir = runs.join(short);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Copy the task's source files into `dst`. Skips `.meta/` (which
/// contains the reference solution) so the agent never sees it.
/// Copies `.docs/instructions.md` (+ append) to `INSTRUCTIONS.md`
/// at the work dir root for easy discovery.
fn copy_task_files(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let mut any_source = false;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str == ".meta" {
            continue;
        }
        if name_str == ".docs" {
            continue; // handled below
        }
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dst.join(&*name_str))?;
        } else {
            std::fs::copy(entry.path(), dst.join(&*name_str))?;
            any_source = true;
        }
    }
    if !any_source {
        return Err(anyhow!("no source files found in {}", src.display()));
    }

    // Concatenate .docs/instructions.md and .docs/instructions.append.md
    let docs = src.join(".docs");
    let mut instructions = String::new();
    for doc in ["introduction.md", "instructions.md", "instructions.append.md"] {
        let p = docs.join(doc);
        if let Ok(body) = std::fs::read_to_string(&p) {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(&body);
        }
    }
    if !instructions.is_empty() {
        std::fs::write(dst.join("INSTRUCTIONS.md"), instructions)?;
    }

    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dst.join(&name))?;
        } else {
            std::fs::copy(entry.path(), dst.join(&name))?;
        }
    }
    Ok(())
}

fn build_prompt(task: &str, lang: PolyglotLang, work_dir: &Path) -> anyhow::Result<String> {
    let files = list_non_hidden_files(work_dir)?;
    let files_joined = files.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n");
    let cmd = lang.test_command().join(" ");

    Ok(format!(
        "You are solving an Aider Polyglot coding benchmark task: `{task}` ({lang}).\n\
\n\
Working directory: the current directory.\n\
Files present:\n\
{files_joined}\n\
\n\
Your job:\n\
1. Read INSTRUCTIONS.md and the test file to understand the requirements.\n\
2. Edit the source file(s) so `{cmd}` passes every test.\n\
3. Do not modify test files.\n\
4. Stop once the tests pass — do not keep iterating.\n\
\n\
Constraints:\n\
- Use only the standard library for the language.\n\
- Keep the implementation idiomatic and concise.\n",
        lang = lang.dataset_dir(),
    ))
}

fn list_non_hidden_files(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        out.push(name);
    }
    out.sort();
    Ok(out)
}

/// Run the language's test command, then ask `smooth-judge` to
/// extract counts from its stdout. Agentic scoring — no per-language
/// regex parsers, so new languages and test runner format drift
/// don't require harness changes.
async fn score_work_dir(lang: PolyglotLang, work_dir: &Path) -> anyhow::Result<(String, TestCounts)> {
    let argv = lang.test_command();
    let program = argv[0];
    let args = &argv[1..];
    let output = Command::new(program)
        .args(args)
        .current_dir(work_dir)
        .output()
        .with_context(|| format!("spawning `{}`", argv.join(" ")))?;
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let counts = judge_test_output(&combined).await.unwrap_or_default();
    Ok((combined, counts))
}

/// Ask the `smooth-judge` slot to extract pass/fail/total counts
/// from a test runner's combined stdout+stderr. Returns the default
/// `TestCounts` on any failure — we'd rather under-report (marks the
/// run as unsolved) than fabricate success.
///
/// # Errors
/// Returns an error only when the registry can't be loaded at all.
/// LLM failures are converted into zero counts, not propagated.
pub async fn judge_test_output(combined_stdout: &str) -> anyhow::Result<TestCounts> {
    use smooth_operator::conversation::Message;
    use smooth_operator::llm::LlmClient;
    use smooth_operator::providers::{Activity, ProviderRegistry};

    let providers_path = dirs_next::home_dir()
        .map(|h| h.join(".smooth/providers.json"))
        .ok_or_else(|| anyhow!("no home dir"))?;
    let registry = ProviderRegistry::load_from_file(&providers_path).context("loading providers.json")?;
    let config = registry.llm_config_for(Activity::Judge).context("no `judge` routing slot configured")?;
    let llm = LlmClient::new(config);

    // Keep the input modest — judge doesn't need 2MB of verbose
    // cargo test output, just enough to see the summary lines. Keep
    // both ends since some runners print the tally at the top
    // (e.g. `go test -v`) and some at the bottom (pytest, cargo).
    let trimmed = trim_for_judge(combined_stdout, 4000);

    let system = Message::system(
        "You extract test-result counts from the output of a test \
         runner (pytest, cargo test, go test, jest, etc.). Respond \
         with a SINGLE line of JSON only: \
         {\"passed\": N, \"failed\": N, \"total\": N}. \
         No code fences, no prose, no preamble.\n\n\
         Scoring rules:\n\
         - Prefer per-case counts when the runner prints them \
         (pytest's `N passed, N failed`, cargo's `test result: ok. \
         N passed; N failed`, go's `--- PASS:` / `--- FAIL:` lines, \
         jest's `Tests: N passed, N failed, N total`).\n\
         - When a suite-level runner only prints `ok <package>` or \
         `FAIL <package>` with no per-case breakdown (e.g. `go test \
         ./...` in non-verbose mode, or `cargo test --quiet`), treat \
         that as a single test: `ok` ⇒ passed=1 failed=0 total=1, \
         `FAIL` ⇒ passed=0 failed=1 total=1. DO NOT return all \
         zeros — the suite is definitive, the counts just aren't.\n\
         - Build/compile errors that prevent the tests from running \
         count as failed=1 total=1.\n\
         - Only return all zeros when the output is truly empty or \
         gives no signal about whether tests ran.",
    );
    let user = Message::user(&format!("Test runner output:\n\n{trimmed}"));
    let response = llm.chat(&[&system, &user], &[]).await.context("smooth-judge call failed")?;

    Ok(parse_judge_response(&response.content))
}

/// Parse the judge's JSON response into `TestCounts`. Lenient:
/// strips code fences, finds the first `{...}` block, accepts
/// partial totals (infers total when the model only gives passed +
/// failed). Unit-tested without a live LLM.
pub fn parse_judge_response(raw: &str) -> TestCounts {
    let body = strip_code_fence(raw.trim());
    let Some(json_slice) = extract_first_object(body) else {
        return TestCounts::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_slice) else {
        return TestCounts::default();
    };

    let passed = value.get("passed").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
    let failed = value.get("failed").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
    let total = value
        .get("total")
        .and_then(serde_json::Value::as_u64)
        .map_or(passed.saturating_add(failed), |n| n as u32);

    TestCounts { passed, failed, total }
}

fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        rest.trim_end_matches("```").trim()
    } else if let Some(rest) = s.strip_prefix("```") {
        rest.trim_end_matches("```").trim()
    } else {
        s
    }
}

fn extract_first_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&s[start..=end])
}

fn trim_for_judge(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Keep the head (setup errors) and the tail (summary). Those are
    // the two spots test runners print their counts.
    let head_bytes = max_bytes / 3;
    let tail_bytes = max_bytes - head_bytes - 64;

    // Careful with UTF-8 — step back to a char boundary.
    let head_end = head_bytes.min(s.len());
    let head_end = (0..=head_end).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);

    let tail_start_raw = s.len().saturating_sub(tail_bytes);
    let tail_start = (tail_start_raw..s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(tail_start_raw);

    format!(
        "{}\n\n[... {} bytes elided ...]\n\n{}",
        &s[..head_end],
        s.len() - head_end - (s.len() - tail_start),
        &s[tail_start..]
    )
}

// ---------------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------------

/// Pretty-print a run summary to stdout.
pub fn print_summary(r: &BenchResult) {
    let status = if r.solved { "SOLVED" } else { "UNSOLVED" };
    println!();
    println!("Benchmark: aider-polyglot/{}/{}", r.lang, r.task);
    println!(
        "Result:    {}  ({}/{} passed, {} failed)",
        status, r.counts.passed, r.counts.total, r.counts.failed
    );
    println!("Duration:  {:.1}s", r.duration_s);
    println!("Cost:      ${:.4}", r.cost_usd);
    if let Some(m) = &r.model {
        println!("Model:     {m}");
    }
    if let Some(err) = &r.llm_error {
        println!("LLM note:  {err}");
    }
    println!("Results:   {}", r.run_dir.join("result.json").display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polyglot_lang_parses_common_names() {
        assert_eq!(PolyglotLang::from_name("python"), Some(PolyglotLang::Python));
        assert_eq!(PolyglotLang::from_name("py"), Some(PolyglotLang::Python));
        assert_eq!(PolyglotLang::from_name("Rust"), Some(PolyglotLang::Rust));
        assert_eq!(PolyglotLang::from_name("rs"), Some(PolyglotLang::Rust));
        assert_eq!(PolyglotLang::from_name("JS"), Some(PolyglotLang::Javascript));
        assert_eq!(PolyglotLang::from_name("fortran"), None);
    }

    #[test]
    fn test_counts_solved_semantics() {
        assert!(TestCounts {
            passed: 20,
            failed: 0,
            total: 20
        }
        .solved());
        assert!(!TestCounts {
            passed: 0,
            failed: 0,
            total: 0
        }
        .solved());
        assert!(!TestCounts {
            passed: 19,
            failed: 1,
            total: 20
        }
        .solved());
        assert!(!TestCounts {
            passed: 20,
            failed: 0,
            total: 21
        }
        .solved());
    }

    #[test]
    fn judge_response_plain_json() {
        let c = parse_judge_response(r#"{"passed": 20, "failed": 0, "total": 20}"#);
        assert_eq!(
            c,
            TestCounts {
                passed: 20,
                failed: 0,
                total: 20
            }
        );
    }

    #[test]
    fn judge_response_strips_json_code_fence() {
        let c = parse_judge_response("```json\n{\"passed\": 5, \"failed\": 2, \"total\": 7}\n```");
        assert_eq!(
            c,
            TestCounts {
                passed: 5,
                failed: 2,
                total: 7
            }
        );
    }

    #[test]
    fn judge_response_strips_bare_code_fence() {
        let c = parse_judge_response("```\n{\"passed\": 1, \"failed\": 0, \"total\": 1}\n```");
        assert_eq!(
            c,
            TestCounts {
                passed: 1,
                failed: 0,
                total: 1
            }
        );
    }

    #[test]
    fn judge_response_infers_total_from_passed_plus_failed() {
        let c = parse_judge_response(r#"{"passed": 3, "failed": 2}"#);
        assert_eq!(
            c,
            TestCounts {
                passed: 3,
                failed: 2,
                total: 5
            }
        );
    }

    #[test]
    fn judge_response_tolerates_prose_around_object() {
        let c = parse_judge_response("Sure! Here you go: {\"passed\": 10, \"failed\": 0, \"total\": 10} — hope that helps.");
        assert_eq!(
            c,
            TestCounts {
                passed: 10,
                failed: 0,
                total: 10
            }
        );
    }

    #[test]
    fn judge_response_malformed_returns_zero() {
        assert_eq!(parse_judge_response("I don't know."), TestCounts::default());
        assert_eq!(parse_judge_response("{not json}"), TestCounts::default());
        assert_eq!(parse_judge_response(""), TestCounts::default());
    }

    #[test]
    fn trim_for_judge_keeps_head_and_tail_under_budget() {
        let big = "head-line\n".repeat(500) + &"tail-line\n".repeat(500);
        let out = trim_for_judge(&big, 500);
        assert!(out.len() <= 800, "trimmed output should be ≲ budget + elision note: got {}", out.len());
        assert!(out.starts_with("head-line"));
        assert!(out.contains("[... "));
        assert!(out.trim_end().ends_with("tail-line"));
    }

    #[test]
    fn trim_for_judge_below_budget_is_unchanged() {
        let s = "short output";
        assert_eq!(trim_for_judge(s, 1000), s);
    }

    #[test]
    fn copy_task_files_excludes_meta() {
        let src = tempfile::tempdir().expect("src");
        let dst = tempfile::tempdir().expect("dst");

        std::fs::write(src.path().join("main.py"), b"pass\n").unwrap();
        std::fs::write(src.path().join("main_test.py"), b"def test_x(): pass\n").unwrap();
        std::fs::create_dir(src.path().join(".meta")).unwrap();
        std::fs::write(src.path().join(".meta/example.py"), b"class A: pass\n").unwrap();
        std::fs::create_dir(src.path().join(".docs")).unwrap();
        std::fs::write(src.path().join(".docs/instructions.md"), b"do the thing").unwrap();

        copy_task_files(src.path(), dst.path()).expect("copy ok");

        assert!(dst.path().join("main.py").exists());
        assert!(dst.path().join("main_test.py").exists());
        assert!(dst.path().join("INSTRUCTIONS.md").exists());
        assert!(!dst.path().join(".meta").exists(), ".meta must not leak to the agent");
        assert!(!dst.path().join("example.py").exists());
    }

    #[test]
    fn build_prompt_lists_files_and_test_command() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        std::fs::write(tmp.path().join("grade_school.py"), b"").unwrap();
        std::fs::write(tmp.path().join("grade_school_test.py"), b"").unwrap();
        std::fs::write(tmp.path().join("INSTRUCTIONS.md"), b"stuff").unwrap();

        let prompt = build_prompt("grade-school", PolyglotLang::Python, tmp.path()).expect("prompt");
        assert!(prompt.contains("grade-school"));
        assert!(prompt.contains("grade_school.py"));
        assert!(prompt.contains("grade_school_test.py"));
        assert!(prompt.contains("INSTRUCTIONS.md"));
        assert!(prompt.contains("python3 -m pytest"));
    }
}
