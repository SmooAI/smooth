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
            // `--include-ignored` runs tests marked `#[ignore]` — the
            // polyglot Rust tasks ship with most tests `#[ignore]`d
            // (bowling: 30 of 31). Without this, we'd score "solved"
            // off a single trivial case and miss the real evaluation.
            // `--` separates cargo flags from test-runner flags.
            Self::Rust => &["cargo", "test", "--", "--include-ignored"],
            // `-v` gives `--- PASS: TestName` / `--- FAIL:` per subtest,
            // instead of only the terminal `ok <package> <duration>` line.
            Self::Go => &["go", "test", "-v", "./..."],
            // `npm install` first — devDependencies (jest, babel, etc.) aren't
            // in the task's scratch dir by default; only the package.json is.
            // `--silent --no-audit --no-fund` keep install output terse so the
            // judge still has a short tail with jest's actual summary.
            Self::Javascript => &["sh", "-c", "npm install --silent --no-audit --no-fund && npm test"],
            // Use the task's bundled Gradle wrapper (`gradlew`) so we don't
            // depend on a system-wide gradle of a specific version.
            // `--no-daemon` avoids leaking a background daemon per task.
            Self::Java => &["sh", "-c", "./gradlew test --no-daemon"],
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
    enable_skipped_tests(lang, &work_dir)?;
    // Snapshot the original file set BEFORE the agent touches
    // anything. Used after the agent finishes to delete any
    // test-file-pattern files it added — the polyglot scorer runs
    // the test command over the whole work dir, so agent-written
    // `test_*.py` / `*_test.go` / `*.spec.ts` / `*Test.java` / etc.
    // would get counted and tilt the score. Benchmark invariant:
    // only the provided tests count.
    let original_files = snapshot_files(&work_dir)?;
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

    // Delete any test-file-pattern files the agent added that
    // weren't in the original task. Leaves production-code files
    // alone regardless of name, and leaves original test files
    // intact — only new files matching per-language test patterns
    // get cleaned. See `strip_agent_added_tests` for the patterns.
    let stripped_files = strip_agent_added_tests(lang, &work_dir, &original_files)?;
    if !stripped_files.is_empty() {
        eprintln!(
            "bench: stripped {} agent-added test file(s) before scoring: {:?}",
            stripped_files.len(),
            stripped_files
        );
    }

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

/// Strip per-language "skip this test" markers so every case in the
/// upstream dataset actually runs. Aider Polyglot intentionally ships
/// most of its tests disabled (so the stub compiles); unless we flip
/// them on the bench scores only the one trivial remaining case.
///
/// Handled here rather than as a blanket command-line flag because
/// some languages (JS) don't have a "run-skipped" flag — the markers
/// are literal in the test file and have to be rewritten.
///
/// Safe to call on any lang: it's a no-op when the language has no
/// skip markers or the target files aren't present.
fn enable_skipped_tests(lang: PolyglotLang, work_dir: &Path) -> anyhow::Result<()> {
    match lang {
        PolyglotLang::Rust => {
            // Nothing to do on disk — Rust runs all tests via the
            // `cargo test -- --include-ignored` command-line flag.
            Ok(())
        }
        PolyglotLang::Javascript => {
            // jest skip shapes:
            //   xtest( / xit(        — the whole case is skipped
            //   test.skip( / it.skip( — same thing, alternate syntax
            // Rewrite all four variants in every *.spec.js (or similar)
            // file under the work dir.
            rewrite_jest_skips(work_dir)
        }
        PolyglotLang::Java => {
            // JUnit 5 / JUnit 4 skip shapes:
            //   @Disabled           — JUnit 5
            //   @Disabled("reason") — JUnit 5 with reason
            //   @Ignore             — JUnit 4
            //   @Ignore("reason")   — JUnit 4 with reason
            // Strip the annotations from every *.java under the test
            // tree so the underlying `@Test` runs.
            rewrite_junit_skips(work_dir)
        }
        PolyglotLang::Python | PolyglotLang::Go | PolyglotLang::Cpp => {
            // No-op: polyglot Python/Go tasks don't ship skipped
            // tests; C++ is future work once it's exercised.
            Ok(())
        }
    }
}

/// Walk the work dir and remove every JUnit `@Disabled`/`@Ignore`
/// annotation — whether bare (`@Disabled`) or with a reason string
/// (`@Disabled("not yet implemented")`). Leaves the underlying
/// `@Test` intact so the case actually runs.
///
/// Only touches `.java` files under `src/test/…`; leaves production
/// code alone even if it happens to include a doc-comment mentioning
/// the annotation.
fn rewrite_junit_skips(dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() && !name.starts_with('.') && name != "build" && name != ".gradle" {
            rewrite_junit_skips(&path)?;
            continue;
        }
        if !name.ends_with(".java") {
            continue;
        }
        // Only touch test files — avoid stripping a @Disabled the
        // project uses legitimately in prod code.
        let s = path.to_string_lossy();
        if !s.contains("/test/") && !s.ends_with("Test.java") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else { continue };
        let rewritten = strip_junit_skip_annotations(&body);
        if rewritten != body {
            std::fs::write(&path, rewritten)?;
        }
    }
    Ok(())
}

/// Remove JUnit skip annotation lines from a Java source. A line is
/// dropped when its only non-whitespace content is `@Disabled` or
/// `@Ignore`, optionally followed by a parenthesized argument. Keeps
/// surrounding whitespace layout tidy.
fn strip_junit_skip_annotations(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let is_skip = trimmed.starts_with("@Disabled") || trimmed.starts_with("@Ignore");
        if is_skip {
            // Verify the NEXT non-blank thing is the annotation (not
            // a word like `@DisabledByDefault`) — require it to end
            // the identifier cleanly before `(` / whitespace / EOL.
            let rest = &trimmed[1..]; // past the @
            let (name, _) = rest.split_once(|c: char| !c.is_ascii_alphanumeric()).unwrap_or((rest, ""));
            if name == "Disabled" || name == "Ignore" {
                continue; // skip the whole line
            }
        }
        out.push_str(line);
    }
    out
}

fn rewrite_jest_skips(dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() && !name.starts_with('.') && name != "node_modules" {
            rewrite_jest_skips(&path)?;
            continue;
        }
        if !(name.ends_with(".spec.js") || name.ends_with(".test.js") || name.ends_with(".spec.ts") || name.ends_with(".test.ts")) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else { continue };
        let rewritten = body
            .replace("xtest(", "test(")
            .replace("xit(", "it(")
            .replace("test.skip(", "test(")
            .replace("it.skip(", "it(")
            .replace("describe.skip(", "describe(")
            .replace("xdescribe(", "describe(");
        if rewritten != body {
            std::fs::write(&path, rewritten)?;
        }
    }
    Ok(())
}

/// Recursively collect the set of relative paths under `root`.
/// Directories aren't included — just files — because the strip
/// step only cares about file-level additions. Skips `.git` +
/// `.smooth` (transient/metadata dirs that never appear in the
/// dataset).
fn snapshot_files(root: &Path) -> anyhow::Result<std::collections::HashSet<PathBuf>> {
    let mut out = std::collections::HashSet::new();
    walk(root, root, &mut out)?;
    return Ok(out);

    fn walk(base: &Path, dir: &Path, out: &mut std::collections::HashSet<PathBuf>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str == ".git" || name_str == ".smooth" || name_str == "node_modules" || name_str == "target" || name_str == "build" || name_str == ".gradle"
            {
                continue;
            }
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(base, &path, out)?;
            } else if let Ok(rel) = path.strip_prefix(base) {
                out.insert(rel.to_path_buf());
            }
        }
        Ok(())
    }
}

/// Delete any test-file-pattern files the agent added that weren't
/// in the original task snapshot. Non-test files the agent created
/// (helpers, new modules) are left alone — the polyglot scorer
/// only looks at test output, so a new module only matters if a
/// test imports it, and imports from the agent's own code are
/// fine.
///
/// Returns the list of relative paths that got deleted so callers
/// can log them.
fn strip_agent_added_tests(lang: PolyglotLang, work_dir: &Path, original: &std::collections::HashSet<PathBuf>) -> anyhow::Result<Vec<PathBuf>> {
    let current = snapshot_files(work_dir)?;
    let mut stripped = Vec::new();
    for rel in current.difference(original) {
        if is_test_file(lang, rel) {
            let full = work_dir.join(rel);
            if std::fs::remove_file(&full).is_ok() {
                stripped.push(rel.clone());
            }
        }
    }
    Ok(stripped)
}

/// Per-language test-file naming conventions. Only matches files
/// the agent ADDED; originals stay in place (they're excluded at a
/// higher level via the snapshot diff).
fn is_test_file(lang: PolyglotLang, rel: &Path) -> bool {
    let name = match rel.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    let rel_str = rel.to_string_lossy();
    match lang {
        PolyglotLang::Python => {
            // pytest discovery patterns: `test_*.py`, `*_test.py`, and
            // anything under a top-level `tests/` dir.
            name.starts_with("test_") && name.ends_with(".py") || name.ends_with("_test.py") || rel_str.starts_with("tests/") && name.ends_with(".py")
        }
        PolyglotLang::Rust => {
            // Rust integration tests live under `tests/`. Unit tests
            // are inline `#[cfg(test)]` blocks — we can't strip those
            // without parsing, so leave them. They're inside source
            // files anyway; agent additions to lib.rs don't count as
            // new files.
            rel_str.starts_with("tests/") && name.ends_with(".rs")
        }
        PolyglotLang::Go => name.ends_with("_test.go"),
        PolyglotLang::Javascript => name.ends_with(".spec.js") || name.ends_with(".test.js") || name.ends_with(".spec.ts") || name.ends_with(".test.ts"),
        PolyglotLang::Java => {
            // JUnit discovery: classes whose name ends in Test /
            // Tests, or files under `src/test/…`.
            rel_str.contains("/test/") && name.ends_with(".java") || name.ends_with("Test.java") || name.ends_with("Tests.java")
        }
        PolyglotLang::Cpp => {
            name.ends_with("_test.cpp") || name.starts_with("test_") && name.ends_with(".cpp") || rel_str.contains("/tests/") && name.ends_with(".cpp")
        }
    }
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
    let user = Message::user(format!("Test runner output:\n\n{trimmed}"));
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

    #[test]
    fn rewrite_jest_skips_flips_every_variant() {
        let tmp = tempfile::tempdir().expect("tmp");
        let spec = tmp.path().join("foo.spec.js");
        std::fs::write(
            &spec,
            r#"
describe("bowling", () => {
  test("one", () => { expect(1).toBe(1); });
  xtest("two", () => {});
  test.skip("three", () => {});
  it.skip("four", () => {});
  xit("five", () => {});
  xdescribe("nested", () => {
    test("six", () => {});
  });
  describe.skip("also nested", () => {
    test("seven", () => {});
  });
});
"#,
        )
        .unwrap();

        rewrite_jest_skips(tmp.path()).expect("rewrite");

        let body = std::fs::read_to_string(&spec).unwrap();
        assert!(!body.contains("xtest("), "xtest not rewritten: {body}");
        assert!(!body.contains("xit("), "xit not rewritten: {body}");
        assert!(!body.contains("test.skip("), "test.skip not rewritten: {body}");
        assert!(!body.contains("it.skip("), "it.skip not rewritten: {body}");
        assert!(!body.contains("xdescribe("), "xdescribe not rewritten: {body}");
        assert!(!body.contains("describe.skip("), "describe.skip not rewritten: {body}");
        // Every case becomes an active test/it/describe call.
        assert_eq!(body.matches("test(").count(), 5);
    }

    #[test]
    fn rewrite_jest_skips_recurses_into_subdirs() {
        let tmp = tempfile::tempdir().expect("tmp");
        let nested = tmp.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("deep.spec.js"), "xtest(\"x\", () => {});").unwrap();

        rewrite_jest_skips(tmp.path()).expect("rewrite");

        let body = std::fs::read_to_string(nested.join("deep.spec.js")).unwrap();
        assert_eq!(body, "test(\"x\", () => {});");
    }

    #[test]
    fn rewrite_jest_skips_skips_node_modules() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
        std::fs::write(tmp.path().join("node_modules/foo.spec.js"), "xtest(\"x\", () => {});").unwrap();

        rewrite_jest_skips(tmp.path()).expect("rewrite");

        // node_modules content untouched — we don't rewrite third-party code.
        let body = std::fs::read_to_string(tmp.path().join("node_modules/foo.spec.js")).unwrap();
        assert!(body.contains("xtest("));
    }

    #[test]
    fn enable_skipped_tests_is_noop_for_python_rust_go() {
        let tmp = tempfile::tempdir().expect("tmp");
        for lang in [PolyglotLang::Python, PolyglotLang::Rust, PolyglotLang::Go] {
            enable_skipped_tests(lang, tmp.path()).expect("no-op");
        }
    }

    #[test]
    fn strip_junit_skip_annotations_removes_bare_disabled() {
        let src = r#"import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.Disabled;

class BowlingTest {
    @Test
    void passing() {}

    @Disabled
    @Test
    void skipped() {}

    @Disabled("not yet implemented")
    @Test
    void skippedWithReason() {}
}
"#;
        let out = strip_junit_skip_annotations(src);
        assert!(!out.contains("@Disabled"), "all @Disabled lines should be gone: {out}");
        // @Test for each method must remain (3 of them).
        assert_eq!(out.matches("@Test").count(), 3);
        // The method bodies remain.
        assert!(out.contains("void skipped()"));
        assert!(out.contains("void skippedWithReason()"));
    }

    #[test]
    fn strip_junit_skip_annotations_handles_junit4_ignore() {
        let src = "@Ignore\n@Test public void x() {}\n@Ignore(\"meh\")\n@Test public void y() {}\n";
        let out = strip_junit_skip_annotations(src);
        assert!(!out.contains("@Ignore"));
        assert!(out.contains("@Test public void x()"));
        assert!(out.contains("@Test public void y()"));
    }

    #[test]
    fn strip_junit_skip_annotations_preserves_unrelated_annotations() {
        let src = "@DisabledInNativeImage\n@Test void z() {}\n";
        let out = strip_junit_skip_annotations(src);
        // `@DisabledInNativeImage` is NOT `@Disabled` — must survive.
        assert!(out.contains("@DisabledInNativeImage"));
    }

    #[test]
    fn rewrite_junit_skips_recurses_into_test_dir() {
        let tmp = tempfile::tempdir().expect("tmp");
        let test_dir = tmp.path().join("src/test/java");
        std::fs::create_dir_all(&test_dir).unwrap();
        let file = test_dir.join("BowlingTest.java");
        std::fs::write(&file, "@Disabled\n@Test void a() {}\n").unwrap();

        rewrite_junit_skips(tmp.path()).expect("rewrite");

        let body = std::fs::read_to_string(&file).unwrap();
        assert!(!body.contains("@Disabled"));
    }

    #[test]
    fn is_test_file_matches_per_language_conventions() {
        // Python — pytest discovery
        assert!(is_test_file(PolyglotLang::Python, Path::new("test_bowling.py")));
        assert!(is_test_file(PolyglotLang::Python, Path::new("bowling_test.py")));
        assert!(is_test_file(PolyglotLang::Python, Path::new("tests/extra.py")));
        assert!(!is_test_file(PolyglotLang::Python, Path::new("bowling.py")));
        assert!(!is_test_file(PolyglotLang::Python, Path::new("helper.py")));

        // Rust — integration-test files under tests/ only
        assert!(is_test_file(PolyglotLang::Rust, Path::new("tests/edge_cases.rs")));
        assert!(!is_test_file(PolyglotLang::Rust, Path::new("src/lib.rs")));
        assert!(!is_test_file(PolyglotLang::Rust, Path::new("src/tests.rs")));

        // Go
        assert!(is_test_file(PolyglotLang::Go, Path::new("bowling_test.go")));
        assert!(is_test_file(PolyglotLang::Go, Path::new("extra_test.go")));
        assert!(!is_test_file(PolyglotLang::Go, Path::new("bowling.go")));

        // JavaScript
        assert!(is_test_file(PolyglotLang::Javascript, Path::new("bowling.spec.js")));
        assert!(is_test_file(PolyglotLang::Javascript, Path::new("extra.test.ts")));
        assert!(!is_test_file(PolyglotLang::Javascript, Path::new("bowling.js")));

        // Java
        assert!(is_test_file(PolyglotLang::Java, Path::new("src/test/java/BowlingTest.java")));
        assert!(is_test_file(PolyglotLang::Java, Path::new("ExtraTests.java")));
        assert!(!is_test_file(PolyglotLang::Java, Path::new("src/main/java/BowlingGame.java")));
    }

    #[test]
    fn strip_agent_added_tests_removes_only_new_test_files() {
        use std::fs;
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();

        // Original task files
        fs::write(root.join("bowling.py"), "class BowlingGame: pass").unwrap();
        fs::write(root.join("bowling_test.py"), "def test_x(): pass").unwrap();

        let original = snapshot_files(root).unwrap();
        assert_eq!(original.len(), 2);

        // Agent adds: one new test file (should be stripped), one
        // new helper module (should be kept).
        fs::write(root.join("test_extra.py"), "def test_edge(): pass").unwrap();
        fs::write(root.join("helper.py"), "def helper(): pass").unwrap();
        // Agent modifies an existing file — should stay put.
        fs::write(root.join("bowling.py"), "class BowlingGame:\n    def score(self): return 0").unwrap();

        let stripped = strip_agent_added_tests(PolyglotLang::Python, root, &original).unwrap();

        // test_extra.py is gone
        assert_eq!(stripped.len(), 1);
        assert_eq!(stripped[0], Path::new("test_extra.py"));
        assert!(!root.join("test_extra.py").exists());
        // helper.py and bowling.py (modified) survive
        assert!(root.join("helper.py").exists());
        assert!(root.join("bowling.py").exists());
        // Original bowling_test.py untouched
        assert!(root.join("bowling_test.py").exists());
    }

    #[test]
    fn strip_agent_added_tests_ignores_untouched_originals() {
        use std::fs;
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        fs::write(root.join("bowling_test.py"), "def test_x(): pass").unwrap();
        let original = snapshot_files(root).unwrap();
        // Agent didn't add any new test files.
        let stripped = strip_agent_added_tests(PolyglotLang::Python, root, &original).unwrap();
        assert!(stripped.is_empty());
        assert!(root.join("bowling_test.py").exists());
    }

    #[test]
    fn rewrite_junit_skips_leaves_production_code_alone() {
        let tmp = tempfile::tempdir().expect("tmp");
        let main_dir = tmp.path().join("src/main/java");
        std::fs::create_dir_all(&main_dir).unwrap();
        // A production-code file that happens to reference
        // `@Disabled` via a doc comment or something — don't
        // rewrite it just because the annotation name appears.
        let prod = main_dir.join("BowlingGame.java");
        std::fs::write(&prod, "class BowlingGame {\n    // See @Disabled tests in BowlingTest\n}\n").unwrap();

        rewrite_junit_skips(tmp.path()).expect("rewrite");

        let body = std::fs::read_to_string(&prod).unwrap();
        assert!(body.contains("@Disabled"));
    }
}
