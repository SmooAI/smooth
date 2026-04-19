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
        match self {
            Self::Python => &["python3", "-m", "pytest", "-q"],
            Self::Rust => &["cargo", "test", "--quiet"],
            Self::Go => &["go", "test", "./..."],
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

    let (test_stdout, counts) = score_work_dir(lang, &work_dir)?;

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

/// Run the language's test command and parse counts out of stdout.
fn score_work_dir(lang: PolyglotLang, work_dir: &Path) -> anyhow::Result<(String, TestCounts)> {
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
    let counts = parse_test_output(lang, &combined);
    Ok((combined, counts))
}

/// Parse passed/failed/total out of a test runner's combined
/// stdout+stderr. Language-specific because each runner has its
/// own summary line.
pub fn parse_test_output(lang: PolyglotLang, s: &str) -> TestCounts {
    match lang {
        PolyglotLang::Python => parse_pytest(s),
        PolyglotLang::Rust => parse_cargo_test(s),
        _ => TestCounts::default(), // not yet wired for MVP
    }
}

/// Parse pytest summary. Handles:
///   "20 passed in 0.01s"
///   "5 failed, 15 passed in 0.05s"
///   "18 passed, 2 skipped in 0.02s"
fn parse_pytest(s: &str) -> TestCounts {
    let mut counts = TestCounts::default();
    for line in s.lines().rev() {
        let line_lc = line.to_lowercase();
        if !(line_lc.contains("passed") || line_lc.contains("failed") || line_lc.contains("error")) {
            continue;
        }
        // Scan for "N passed" / "N failed" patterns. Ignore skipped / xfailed.
        let mut tokens = line.split_whitespace().peekable();
        while let Some(tok) = tokens.next() {
            let Ok(n) = tok.parse::<u32>() else { continue };
            match tokens.peek().copied() {
                Some("passed") | Some("passed,") => counts.passed = counts.passed.saturating_add(n),
                Some("failed") | Some("failed,") | Some("errors") | Some("error") => {
                    counts.failed = counts.failed.saturating_add(n);
                }
                _ => {}
            }
        }
        if counts.passed + counts.failed > 0 {
            break;
        }
    }
    counts.total = counts.passed + counts.failed;
    counts
}

/// Parse `cargo test` summary. Each `test result:` block has the form
///   "test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
/// A workspace can have multiple blocks — sum them.
fn parse_cargo_test(s: &str) -> TestCounts {
    let mut counts = TestCounts::default();
    for line in s.lines() {
        let l = line.trim();
        if !l.starts_with("test result:") {
            continue;
        }
        let mut tokens = l.split_whitespace().peekable();
        while let Some(tok) = tokens.next() {
            let clean = tok.trim_end_matches([';', ',']);
            let Ok(n) = clean.parse::<u32>() else { continue };
            match tokens.peek().copied() {
                Some("passed") | Some("passed;") | Some("passed,") => counts.passed += n,
                Some("failed") | Some("failed;") | Some("failed,") => counts.failed += n,
                _ => {}
            }
        }
    }
    counts.total = counts.passed + counts.failed;
    counts
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
    fn pytest_parse_all_passed() {
        let s = "....................                                                     [100%]\n20 passed in 0.01s\n";
        let c = parse_pytest(s);
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
    fn pytest_parse_mixed_fail_and_pass() {
        let s = "F.F..F\n3 failed, 3 passed in 0.05s\n";
        let c = parse_pytest(s);
        assert_eq!(
            c,
            TestCounts {
                passed: 3,
                failed: 3,
                total: 6
            }
        );
    }

    #[test]
    fn pytest_parse_ignores_skipped_count() {
        let s = "18 passed, 2 skipped in 0.02s";
        let c = parse_pytest(s);
        assert_eq!(
            c,
            TestCounts {
                passed: 18,
                failed: 0,
                total: 18
            }
        );
    }

    #[test]
    fn pytest_parse_with_errors() {
        let s = "1 failed, 2 passed, 1 error in 0.10s";
        let c = parse_pytest(s);
        assert_eq!(
            c,
            TestCounts {
                passed: 2,
                failed: 2,
                total: 4
            }
        );
    }

    #[test]
    fn cargo_test_parse_single_block() {
        let s = "\ntest foo ... ok\ntest bar ... ok\n\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let c = parse_cargo_test(s);
        assert_eq!(
            c,
            TestCounts {
                passed: 2,
                failed: 0,
                total: 2
            }
        );
    }

    #[test]
    fn cargo_test_parse_multiple_blocks_sum() {
        let s = "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\
                 test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let c = parse_cargo_test(s);
        assert_eq!(
            c,
            TestCounts {
                passed: 4,
                failed: 2,
                total: 6
            }
        );
    }

    #[test]
    fn cargo_test_parse_empty_returns_zero() {
        let c = parse_cargo_test("no matches here");
        assert_eq!(c, TestCounts::default());
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
