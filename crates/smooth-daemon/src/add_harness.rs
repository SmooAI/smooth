//! `add_harness` — Big Smooth adds a coding-agent CLI to SmoothFlow by itself
//! (pearl th-473294, phase 2 of epic th-faa590).
//!
//! The tool is the agentic loop; the pieces it composes are deterministic and
//! live in `smooth-flow`:
//!
//! 1. **probe** — resolve the binary, run its `--help` (`-h` / `help`
//!    fallbacks), fetch the docs URL through `th crawl scrape` when given;
//! 2. **facts** — `harness_draft::parse_help` → ranked argv candidates + a
//!    manifest skeleton (`prompt_as`, session id, resume shape);
//! 3. **draft** — an LLM ([`Drafter`]) turns the brief into a manifest TOML,
//!    with the schema, the built-in `claude.toml` as the reference, and the
//!    previous attempt's verdict + pane tail as feedback;
//! 4. **validate** — `harness_validate::validate` on a PRIVATE engine
//!    ([`Validator`]): launch → working → idle, steer, kill+resume — proofs,
//!    not assumptions;
//! 5. **iterate** up to `max_iterations`, keep the best attempt, **install**
//!    a usable one to `~/.smooth/harnesses/<name>.toml`, and report the
//!    manifest plus exactly what could not be proven.
//!
//! The draft is the only LLM-driven part; everything else is unit-tested
//! with a scripted drafter + validator. No provider ⇒ a structured
//! `needs_provider` answer (`llm_provider::status`), never a mid-run failure.

// The brief and the report are built by appending formatted lines; `write!`
// into a String buys nothing here but noise.
#![allow(clippy::format_push_string, reason = "message builders")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use smooth_flow::harness::{Manifest, StateSource, BUILTIN, CMUX_SHIM_DIR};
use smooth_flow::harness_draft::{self, Candidate, HelpFacts, Skeleton};
use smooth_flow::harness_validate::{self, Budget, EngineDriver, Verdict};
use smooth_operator::{LlmClient, LlmConfig, Message, Tool, ToolSchema};

/// Help output kept for the brief (chars).
const HELP_CAP: usize = 30_000;
/// Docs kept for the brief (chars).
const DOCS_CAP: usize = 24_000;
/// `--help` must answer within this.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// The drafter must answer within this.
const DRAFT_TIMEOUT: Duration = Duration::from_secs(180);
const DEFAULT_ITERATIONS: u8 = 3;
const MAX_ITERATIONS: u8 = 6;

/// The tool's arguments, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub name: String,
    pub binary_hint: Option<String>,
    pub docs_url: Option<String>,
    pub max_iterations: u8,
    pub force: bool,
    pub install_unverified: bool,
    pub model: Option<String>,
}

/// Parse + validate the tool arguments.
///
/// # Errors
/// A missing/ill-shaped `name`, a non-http docs URL, or a bad type.
pub fn parse_request(args: &Value) -> Result<Request> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required string parameter `name`"))?;
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') || name.starts_with('-') {
        anyhow::bail!("`name` must be lowercase letters, digits and dashes (it becomes the session kind and the file name): got `{name}`");
    }
    let opt = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let docs_url = opt("docs_url");
    if let Some(u) = &docs_url {
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            anyhow::bail!("`docs_url` must be an http(s) URL, got: {u}");
        }
    }
    let max_iterations = args
        .get("max_iterations")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_ITERATIONS, |n| u8::try_from(n).unwrap_or(MAX_ITERATIONS))
        .clamp(1, MAX_ITERATIONS);
    Ok(Request {
        name,
        binary_hint: opt("binary_hint"),
        docs_url,
        max_iterations,
        force: args.get("force").and_then(Value::as_bool).unwrap_or(false),
        install_unverified: args.get("install_unverified").and_then(Value::as_bool).unwrap_or(false),
        model: opt("model"),
    })
}

// ── probing ──────────────────────────────────────────────────────────────────

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.is_file() && std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// The dirs the real installs live in, tried before `PATH` (`$HOME`-relative).
const PREFER_DIRS: &[&str] = &[".local/bin", ".cargo/bin", ".npm-global/bin", ".opencode/bin", "bin"];

/// Resolve the harness binary: an explicit path wins; else `names` under the
/// preferred dirs, then on `path` (skipping cmux shim dirs).
#[must_use]
pub fn which_bin(names: &[String], home: &Path, path: &std::ffi::OsStr) -> Option<PathBuf> {
    for n in names {
        if n.contains('/') {
            let p = PathBuf::from(n);
            if is_executable(&p) {
                return Some(p);
            }
        }
    }
    for n in names.iter().filter(|n| !n.contains('/')) {
        if let Some(p) = PREFER_DIRS.iter().map(|d| home.join(d).join(n)).find(|p| is_executable(p)) {
            return Some(p);
        }
    }
    let skip = |dir: &Path| dir.components().any(|c| c.as_os_str() == CMUX_SHIM_DIR);
    for n in names.iter().filter(|n| !n.contains('/')) {
        if let Some(p) = std::env::split_paths(path).filter(|d| !skip(d)).map(|d| d.join(n)).find(|p| is_executable(p)) {
            return Some(p);
        }
    }
    None
}

/// `~/…` spelling of a path under `home`, for `binary.prefer_paths`.
fn home_relative(p: &Path, home: &Path) -> Option<String> {
    p.strip_prefix(home).ok().map(|r| r.to_string_lossy().into_owned())
}

/// What probing the binary learned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeResult {
    /// The help text (stdout + stderr of the invocation that answered).
    pub help: String,
    /// `--help` | `-h` | `help` — which one answered.
    pub invocation: String,
    /// `--version` output, if any.
    pub version: Option<String>,
}

fn cap(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let kept: String = s.chars().take(n).collect();
    format!("{kept}\n… (truncated, {} chars total)", s.chars().count())
}

async fn run_capture(bin: &Path, args: &[&str], cwd: &Path) -> Result<(bool, String)> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("CI", "1")
        .env_remove("FORCE_COLOR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(PROBE_TIMEOUT, cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("`{} {}` did not answer within {}s", bin.display(), args.join(" "), PROBE_TIMEOUT.as_secs()))??;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        text.push('\n');
        text.push_str(&err);
    }
    Ok((out.status.success(), text))
}

/// Run the binary's help (`--help`, then `-h`, then `help`) and `--version`.
///
/// # Errors
/// When no invocation produced a help-looking answer.
pub async fn probe_help(bin: &Path, cwd: &Path) -> Result<ProbeResult> {
    let mut tried = Vec::new();
    let mut best: Option<(String, String)> = None;
    for inv in [&["--help"][..], &["-h"], &["help"]] {
        match run_capture(bin, inv, cwd).await {
            Ok((ok, text)) => {
                let looks_like_help = text.to_ascii_lowercase().contains("usage")
                    || text.to_ascii_lowercase().contains("options")
                    || text.lines().any(|l| l.trim_start().starts_with("--"));
                if looks_like_help && (ok || text.len() > 200) {
                    best = Some((inv.join(" "), text));
                    break;
                }
                if best.is_none() && !text.trim().is_empty() {
                    best = Some((inv.join(" "), text));
                }
                tried.push(format!("{} (exit {})", inv.join(" "), if ok { "0" } else { "non-zero" }));
            }
            Err(e) => tried.push(format!("{}: {e}", inv.join(" "))),
        }
    }
    let (invocation, help) = best.ok_or_else(|| anyhow::anyhow!("`{}` answered none of --help / -h / help: {}", bin.display(), tried.join("; ")))?;
    let version = run_capture(bin, &["--version"], cwd).await.ok().and_then(|(ok, t)| {
        let t = t.lines().next().unwrap_or("").trim().to_string();
        (ok && !t.is_empty() && t.len() < 120).then_some(t)
    });
    Ok(ProbeResult {
        help: cap(&help, HELP_CAP),
        invocation,
        version,
    })
}

/// The stdout section of a `run_th` frame (`$ th … / exit code / --- stdout ---
/// / --- stderr ---`), or the whole thing when it isn't framed.
#[must_use]
pub fn stdout_section(frame: &str) -> String {
    let Some(start) = frame.find("--- stdout ---") else {
        return frame.to_string();
    };
    let body = &frame[start + "--- stdout ---".len()..];
    let end = body.find("--- stderr ---").unwrap_or(body.len());
    body[..end].trim().to_string()
}

/// Fetch a docs page as markdown through `th crawl scrape` (egress rules
/// apply as for the `crawl` tool). Best-effort: `None` on any failure.
pub async fn fetch_docs(url: &str, cwd: &Path) -> Option<String> {
    let frame = smooth_tools::th::run_th(&["crawl".to_string(), "scrape".to_string(), url.to_string()], cwd)
        .await
        .ok()?;
    if !frame.contains("exit code: 0") {
        return None;
    }
    let text = stdout_section(&frame);
    (!text.trim().is_empty()).then(|| cap(&text, DOCS_CAP))
}

// ── drafting ─────────────────────────────────────────────────────────────────

/// Everything the drafter sees.
#[derive(Debug, Clone, Serialize)]
pub struct Brief {
    pub name: String,
    pub binary_path: String,
    pub binary_names: Vec<String>,
    pub prefer_paths: Vec<String>,
    pub version: Option<String>,
    pub help_invocation: String,
    pub help: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    pub facts: HelpFacts,
    pub candidates: Vec<Candidate>,
    /// The deterministic skeleton, rendered — the starting point.
    pub skeleton_toml: String,
}

/// One round of the loop, for feedback and the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub iteration: u8,
    pub toml: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

impl Attempt {
    fn proven_count(&self) -> usize {
        self.verdict.as_ref().map_or(0, |v| v.proven().len())
    }
    fn usable(&self) -> bool {
        self.verdict.as_ref().is_some_and(|v| v.usable)
    }
    fn complete(&self) -> bool {
        self.verdict.as_ref().is_some_and(|v| v.complete)
    }
}

/// Produces a manifest TOML from a brief (+ what went wrong before).
#[async_trait]
pub trait Drafter: Send + Sync {
    /// # Errors
    /// When the model is unreachable or answers nothing.
    async fn draft(&self, brief: &Brief, attempts: &[Attempt]) -> Result<String>;
}

/// Runs a manifest and reports proofs.
pub trait Validator: Send + Sync {
    /// # Errors
    /// When the private engine cannot be set up at all (tmux missing, …) —
    /// a run that launched and failed is an `Ok` verdict with failures.
    fn validate(&self, name: &str, manifest_toml: &str, model: Option<&str>) -> Result<Verdict>;
}

/// The manifest schema, as the drafter is told it.
pub const SCHEMA_DOC: &str = r#"A harness manifest is one TOML file. Fields (unknown fields are errors):

name = "<lowercase-dashes>"          # REQUIRED, must equal the requested name
display_name = "Human Name"

[binary]
names = ["bin"]                      # REQUIRED — PATH candidates; names[0] is the bare fallback
prefer_paths = [".local/bin/bin"]    # $HOME-relative, tried BEFORE PATH
skip_path_patterns = ["cmux-cli-shims"]

[launch]
argv = ["--flag", "{model}", "{prompt}"]   # AFTER the binary. Placeholders: {prompt} {session_id} {cwd} {model} {daemon_url}
                                     # an element whose placeholder is empty is DROPPED together with a bare -flag right before it
prompt_as = "argv"                   # "argv" (needs {prompt} in argv) | "paste" (no {prompt}; pasted into the composer ~4s after launch)
session_id = "learned"               # "preassigned" (engine mints a uuid → {session_id}) | "learned" (from the first hook)
[launch.env]                         # optional; values may use placeholders
SOME_VAR = "{session_id}"

[resume]
argv = ["--resume", "{session_id}"]  # needs {session_id} when mode = "resume_session"
mode = "resume_session"              # "resume_session" | "relaunch_command" (relaunch the original argv)

[state]
source = "scrape"                    # "hooks" (the CLI posts to /api/flow/hooks — ONLY if it documents hooks and you state the install recipe) | "scrape" (pane regexes) | "native" (ours only)
[state.hooks]
install = "how the hooks get wired"
[state.hooks.event_map]              # harness event → "working" | "idle" | "needs_you" | "ended" | "ignore"
[state.scrape]                       # case-insensitive regexes over the visible pane (source = "scrape" needs working and/or idle)
working = ["esc to interrupt"]       # matched in the LAST 12 non-blank lines
idle = ["> "]                        # matched in the last 12 lines; the prompt marker / footer hint
needs_you = ["\\(y/n\\)"]            # approval / trust / auth prompts, anywhere
usage_limit = ["quota .* resets at (?P<reset>[^\n]+)"]
error = ["api error"]

[steer]
method = "bracketed_paste"           # | "stdin"
submit_key = "Enter"

[kill]
signal = "TERM"
grace_ms = 3000

[install]                            # optional: where th pkg renders skills/rules/MCP for this harness
skills_dir = "~/.tool/skills"
mcp_config = "~/.tool/mcp.json"
"#;

/// The system prompt for the drafting model.
pub const DRAFT_SYSTEM_PROMPT: &str = "You write SmoothFlow harness manifests: a TOML file that tells the engine how to launch, resume, observe, steer and kill a coding-agent CLI as a supervised interactive session (a TUI in a tmux pane).\n\
Rules:\n\
- Answer with ONE ```toml fenced block containing the whole manifest and nothing else outside it (a one-line note before the block is fine).\n\
- `name` must equal the requested name; `binary.names` must be the given binary name(s).\n\
- The session must be INTERACTIVE and stay alive after the first prompt. Never use print/headless/non-interactive/one-shot flags (`-p`, `--print`, `--message`, `--output-format`) as the prompt slot. If there is no interactive prompt flag or positional, use prompt_as = \"paste\".\n\
- Do NOT add auto-approve / yolo / skip-permission / trust flags unless the captured pane proves the session cannot start without one, and say so in a comment.\n\
- Prefer the highest-confidence candidate unless the help text contradicts it. Keep `{model}` only if the CLI has a model flag.\n\
- state.source: use \"scrape\" unless the CLI documents hooks AND you can state how to wire them to POST /api/flow/hooks; then \"hooks\" with an `install` note, keeping scrape patterns as the fallback.\n\
- Scrape patterns are regexes over the LAST 12 visible lines for working/idle. When a captured pane is given, derive `idle` from its prompt marker / footer and `working` from what changes while it thinks. Escape regex metacharacters. Keep patterns short and specific.\n\
- Resume: `resume_session` only when the CLI takes a session id; otherwise `relaunch_command`.\n\
- When a previous attempt failed, change ONLY what the verdict points at.";

/// Render the drafter's user message.
#[must_use]
pub fn draft_user_message(brief: &Brief, attempts: &[Attempt]) -> String {
    let mut m = String::new();
    m.push_str(&format!("Write the manifest for harness `{}` (binary: {}", brief.name, brief.binary_path));
    if let Some(v) = &brief.version {
        m.push_str(&format!(", version: {v}"));
    }
    m.push_str(").\n\n## Schema\n");
    m.push_str(SCHEMA_DOC);
    m.push_str("\n## Reference: the built-in Claude Code manifest\n```toml\n");
    m.push_str(BUILTIN[0].1);
    m.push_str("```\n\n## What `");
    m.push_str(&brief.help_invocation);
    m.push_str("` printed\n```\n");
    m.push_str(&brief.help);
    m.push_str("\n```\n");
    if let Some(d) = &brief.docs {
        m.push_str("\n## Docs page (markdown)\n");
        m.push_str(d);
        m.push('\n');
    }
    m.push_str("\n## Deterministic analysis\nHooks mentioned in the help: ");
    m.push_str(if brief.facts.mentions_hooks { "yes" } else { "no" });
    m.push_str("\nRanked launch candidates (confidence, argv, prompt_as, session_id, resume, why):\n");
    for c in &brief.candidates {
        m.push_str(&format!(
            "- {}%: argv={:?} prompt_as={:?} session_id={:?} resume={:?} ({:?})\n  {}\n",
            c.confidence,
            c.launch_argv,
            c.prompt_as,
            c.session_id,
            c.resume_argv,
            c.resume_mode,
            c.rationale.join("; ")
        ));
    }
    m.push_str("\n## Skeleton (start from this)\n```toml\n");
    m.push_str(&brief.skeleton_toml);
    m.push_str("```\n");
    for a in attempts {
        m.push_str(&format!("\n## Attempt {} — what happened\n```toml\n{}```\n", a.iteration, a.toml));
        if let Some(e) = &a.parse_error {
            m.push_str(&format!("The manifest did not validate: {e}\nFix exactly that.\n"));
        }
        if let Some(v) = &a.verdict {
            m.push_str("Validation run:\n");
            for l in v.summary_lines() {
                m.push_str("  ");
                m.push_str(&l);
                m.push('\n');
            }
            if !v.pane_tail.trim().is_empty() {
                m.push_str("Last visible pane (last 12 lines):\n```\n");
                m.push_str(&v.pane_tail);
                m.push_str("\n```\n");
                let derived = harness_draft::scrape_from_panes(&v.pane_tail, None);
                m.push_str(&format!("Idle pattern derived from that pane: {:?}\n", derived.idle));
            }
            m.push_str(&format!("state_source the engine reported: {}\n", v.state_source));
        }
    }
    m.push_str("\nNow answer with the manifest.");
    m
}

/// The real drafter: one chat call to the daemon's model.
pub struct LlmDrafter {
    client: LlmClient,
}

impl LlmDrafter {
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        Self {
            client: LlmClient::new(config),
        }
    }
}

#[async_trait]
impl Drafter for LlmDrafter {
    async fn draft(&self, brief: &Brief, attempts: &[Attempt]) -> Result<String> {
        let sys = Message::system(DRAFT_SYSTEM_PROMPT);
        let user = Message::user(draft_user_message(brief, attempts));
        let resp = tokio::time::timeout(DRAFT_TIMEOUT, self.client.chat(&[&sys, &user], &[]))
            .await
            .map_err(|_| anyhow::anyhow!("the drafting model did not answer within {}s", DRAFT_TIMEOUT.as_secs()))??;
        if resp.content.trim().is_empty() {
            anyhow::bail!("the drafting model returned an empty reply");
        }
        Ok(resp.content)
    }
}

/// The real validator: a private engine per run (blocking; call it from
/// `spawn_blocking`).
pub struct EngineValidator {
    pub budget: Budget,
}

impl Validator for EngineValidator {
    fn validate(&self, name: &str, manifest_toml: &str, model: Option<&str>) -> Result<Verdict> {
        let mut driver = EngineDriver::private(name, manifest_toml, model.map(str::to_string))?;
        Ok(harness_validate::validate(&mut driver, name, &self.budget))
    }
}

// ── the loop ─────────────────────────────────────────────────────────────────

/// How the run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The daemon has no LLM credentials — nothing was attempted.
    NeedsProvider,
    /// Installed, every step proven.
    Installed,
    /// Installed (launch + idle proven), some steps unproven.
    InstalledPartial,
    /// Not installed: never usable (or exists without `force`); the best draft is in the report.
    Drafted,
    /// The binary could not be found / probed.
    Failed,
}

/// The tool's answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub status: Status,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manifest: String,
    pub iterations: u8,
    #[serde(default)]
    pub proven: Vec<String>,
    /// `(step, why)`.
    #[serde(default)]
    pub unproven: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pane_tail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<crate::llm_provider::ProviderStatus>,
}

impl Report {
    fn failed(name: &str, error: String) -> Self {
        Self {
            status: Status::Failed,
            name: name.to_string(),
            path: None,
            binary: None,
            manifest: String::new(),
            iterations: 0,
            proven: Vec::new(),
            unproven: Vec::new(),
            pane_tail: String::new(),
            error: Some(error),
            next_steps: Vec::new(),
            provider: None,
        }
    }

    /// The structured `needs_provider` answer.
    #[must_use]
    pub fn needs_provider(name: &str, provider: crate::llm_provider::ProviderStatus) -> Self {
        let next_steps = provider.options.iter().map(|o| format!("{}: {}", o.title, o.command)).collect();
        Self {
            status: Status::NeedsProvider,
            name: name.to_string(),
            path: None,
            binary: None,
            manifest: String::new(),
            iterations: 0,
            proven: Vec::new(),
            unproven: Vec::new(),
            pane_tail: String::new(),
            error: Some("Big Smooth has no LLM provider configured — drafting a manifest needs a model".into()),
            next_steps,
            provider: Some(provider),
        }
    }

    /// Human text first (what the agent relays), then the JSON.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let head = match self.status {
            Status::NeedsProvider => "needs a provider",
            Status::Installed => "installed — every step proven",
            Status::InstalledPartial => "installed — some steps unproven",
            Status::Drafted => "drafted, NOT installed",
            Status::Failed => "failed",
        };
        out.push_str(&format!("add_harness `{}`: {head}\n", self.name));
        if let Some(p) = &self.path {
            out.push_str(&format!("manifest: {p}\n"));
        }
        if let Some(b) = &self.binary {
            out.push_str(&format!("binary: {b}\n"));
        }
        if let Some(e) = &self.error {
            out.push_str(&format!("error: {e}\n"));
        }
        if self.iterations > 0 {
            out.push_str(&format!("iterations: {}\n", self.iterations));
        }
        for p in &self.proven {
            out.push_str(&format!("● {p}\n"));
        }
        for (s, why) in &self.unproven {
            out.push_str(&format!("○ {s} — {}\n", why.lines().next().unwrap_or("")));
        }
        if !self.next_steps.is_empty() {
            out.push_str("next:\n");
            for n in &self.next_steps {
                out.push_str(&format!("  {n}\n"));
            }
        }
        if !self.manifest.is_empty() {
            out.push_str("\n```toml\n");
            out.push_str(&self.manifest);
            if !self.manifest.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
        }
        out.push_str("\n```json\n");
        out.push_str(&serde_json::to_string_pretty(self).unwrap_or_default());
        out.push_str("\n```");
        out
    }
}

/// Where a manifest is installed.
#[must_use]
pub fn install_path(home: &Path, name: &str) -> PathBuf {
    home.join(".smooth").join("harnesses").join(format!("{name}.toml"))
}

/// The loop, with every side effect behind a parameter: `home` (install dir
/// + prefer-path spelling), `path` (binary search), `cwd` (probe/docs cwd).
///
/// # Errors
/// Never for a probe/draft/validate problem — those are reported in the
/// [`Report`]; only when the install write itself fails.
#[allow(clippy::too_many_lines, reason = "the loop is one linear pipeline: probe → draft → validate → install")]
pub async fn run(req: &Request, home: &Path, path: &std::ffi::OsStr, cwd: &Path, drafter: &dyn Drafter, validator: &dyn Validator) -> Result<Report> {
    // 1. Binary.
    let mut names: Vec<String> = Vec::new();
    if let Some(h) = &req.binary_hint {
        names.push(h.clone());
    }
    if !names.contains(&req.name) {
        names.push(req.name.clone());
    }
    let Some(bin) = which_bin(&names, home, path) else {
        return Ok(Report::failed(
            &req.name,
            format!(
                "`{}` is not installed (looked in ~/{{{}}} and PATH, skipping cmux shims). Install it first, or pass binary_hint with the executable's name or full path.",
                names.join("`/`"),
                PREFER_DIRS.join(",")
            ),
        ));
    };
    let bin_name = bin.file_name().map_or_else(|| req.name.clone(), |f| f.to_string_lossy().into_owned());
    let binary_names: Vec<String> = {
        let mut v = vec![bin_name.clone()];
        for n in &names {
            if !n.contains('/') && !v.contains(n) {
                v.push(n.clone());
            }
        }
        v
    };
    let prefer_paths: Vec<String> = home_relative(&bin, home).into_iter().collect();

    // 2. Probe.
    let probe = match probe_help(&bin, cwd).await {
        Ok(p) => p,
        Err(e) => return Ok(Report::failed(&req.name, format!("{e:#}"))),
    };
    let docs = match &req.docs_url {
        Some(u) => fetch_docs(u, cwd).await,
        None => None,
    };

    // 3. Facts + skeleton.
    let facts = harness_draft::parse_help(&probe.help);
    let candidates = harness_draft::argv_candidates(&facts);
    let skeleton = Skeleton {
        name: req.name.clone(),
        display_name: display_name(&req.name),
        binary_names: binary_names.clone(),
        prefer_paths: prefer_paths.clone(),
        candidate: candidates.first().cloned(),
        scrape: harness_draft::scrape_from_panes("> ", None),
        state_source: StateSource::Scrape,
        hooks_install: if facts.mentions_hooks {
            "the CLI documents hooks — wire them to POST /api/flow/hooks {harness, event, session_id, cwd, payload}; until then state is scraped".into()
        } else {
            String::new()
        },
        launch_env: std::collections::BTreeMap::new(),
    };
    let skeleton_toml = harness_draft::render_toml(&harness_draft::draft_manifest(&skeleton), "").unwrap_or_default();
    let brief = Brief {
        name: req.name.clone(),
        binary_path: bin.to_string_lossy().into_owned(),
        binary_names,
        prefer_paths,
        version: probe.version.clone(),
        help_invocation: probe.invocation.clone(),
        help: probe.help.clone(),
        docs,
        facts,
        candidates,
        skeleton_toml: skeleton_toml.clone(),
    };

    // 4. Draft → validate → iterate.
    let header = format!(
        "{} — drafted by Big Smooth's add_harness (th-473294) from `{} {}`{}\nedit freely; `th harness show {}` prints it, `th flow new --kind {}` runs it",
        req.name,
        bin_name,
        probe.invocation,
        probe.version.as_deref().map(|v| format!(" ({v})")).unwrap_or_default(),
        req.name,
        req.name
    );
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut iterations = 0u8;
    while iterations < req.max_iterations {
        iterations += 1;
        let reply = match drafter.draft(&brief, &attempts).await {
            Ok(r) => r,
            Err(e) => {
                // A drafter that cannot answer at all: fall back to the skeleton
                // once, so a dead model still yields a validated draft.
                if attempts.iter().any(|a| a.parse_error.as_deref() == Some("drafter unavailable")) || skeleton_toml.is_empty() {
                    attempts.push(Attempt {
                        iteration: iterations,
                        toml: String::new(),
                        parse_error: Some(format!("drafter error: {e:#}")),
                        verdict: None,
                    });
                    break;
                }
                tracing::warn!(error = %e, "add_harness: drafter failed — validating the deterministic skeleton instead");
                attempts.push(Attempt {
                    iteration: iterations,
                    toml: String::new(),
                    parse_error: Some("drafter unavailable".into()),
                    verdict: None,
                });
                format!("```toml\n{skeleton_toml}```")
            }
        };
        let toml = harness_draft::extract_toml(&reply);
        let toml = match Manifest::parse(&toml) {
            Ok(mut m) if m.name == req.name => {
                // Keep provenance + the name pinned; render canonical TOML. The
                // resolved install path is deterministic knowledge the drafter
                // has no reason to drop: it is what beats a PATH shim.
                m.display_name = if m.display_name.trim().is_empty() {
                    display_name(&req.name)
                } else {
                    m.display_name
                };
                if m.binary.prefer_paths.is_empty() {
                    m.binary.prefer_paths.clone_from(&brief.prefer_paths);
                }
                harness_draft::render_toml(&m, &header).unwrap_or(toml)
            }
            Ok(m) => {
                attempts.push(Attempt {
                    iteration: iterations,
                    toml,
                    parse_error: Some(format!("`name` must be \"{}\", got \"{}\"", req.name, m.name)),
                    verdict: None,
                });
                continue;
            }
            Err(e) => {
                attempts.push(Attempt {
                    iteration: iterations,
                    toml,
                    parse_error: Some(format!("{e:#}")),
                    verdict: None,
                });
                continue;
            }
        };
        let (name, toml_c, model) = (req.name.clone(), toml.clone(), req.model.clone());
        let verdict = match validator.validate(&name, &toml_c, model.as_deref()) {
            Ok(v) => v,
            Err(e) => {
                attempts.push(Attempt {
                    iteration: iterations,
                    toml,
                    parse_error: Some(format!("validation could not run: {e:#}")),
                    verdict: None,
                });
                break;
            }
        };
        let complete = verdict.complete;
        attempts.push(Attempt {
            iteration: iterations,
            toml,
            parse_error: None,
            verdict: Some(verdict),
        });
        if complete {
            break;
        }
    }

    // 5. Best attempt → install → report.
    let best = attempts
        .iter()
        .filter(|a| a.verdict.is_some())
        .max_by_key(|a| (a.complete(), a.usable(), a.proven_count(), a.iteration))
        .or_else(|| attempts.iter().rev().find(|a| !a.toml.is_empty()));
    let Some(best) = best else {
        let last_err = attempts
            .last()
            .and_then(|a| a.parse_error.clone())
            .unwrap_or_else(|| "no draft was produced".into());
        let mut r = Report::failed(&req.name, last_err);
        r.binary = Some(bin.to_string_lossy().into_owned());
        r.iterations = iterations;
        return Ok(r);
    };
    let verdict = best.verdict.clone().unwrap_or_default();
    let installable = best.usable() || (req.install_unverified && best.verdict.is_some());
    let dest = install_path(home, &req.name);
    let mut next_steps = Vec::new();
    let (status, path_written, error) = if installable {
        if dest.exists() && !req.force {
            (
                Status::Drafted,
                None,
                Some(format!("{} exists — pass force=true to replace it", dest.display())),
            )
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
            }
            std::fs::write(&dest, &best.toml).with_context(|| format!("write {}", dest.display()))?;
            (
                if best.complete() { Status::Installed } else { Status::InstalledPartial },
                Some(dest.to_string_lossy().into_owned()),
                None,
            )
        }
    } else {
        (
            Status::Drafted,
            None,
            Some(match best.parse_error.as_deref() {
                Some(e) => format!("the last draft did not validate: {e}"),
                None => {
                    "no attempt reached launch + idle; the best draft is below — fix it by hand (`th harness add <file>`) or rerun with install_unverified=true"
                        .to_string()
                }
            }),
        )
    };
    if path_written.is_some() {
        next_steps.push(format!("th harness show {}   # the manifest", req.name));
        next_steps.push(format!(
            "th flow new --kind {} --prompt \"say hi\"   # a real session; th flow snapshot <id> shows the pane",
            req.name
        ));
        if verdict.state_source == "inferred" {
            next_steps.push("state is scraped (inferred); wire hooks to POST /api/flow/hooks for exact working/idle and a learned session id".into());
        }
    } else {
        next_steps.push(format!(
            "save the TOML below and run: th harness add <file>   # after editing; or th harness add --agentic {} --force",
            req.name
        ));
    }
    Ok(Report {
        status,
        name: req.name.clone(),
        path: path_written,
        binary: Some(bin.to_string_lossy().into_owned()),
        manifest: best.toml.clone(),
        iterations,
        proven: verdict.proven().iter().map(|s| harness_validate::step_label(*s).to_string()).collect(),
        unproven: verdict
            .not_proven()
            .into_iter()
            .map(|(s, why)| (harness_validate::step_label(s).to_string(), why))
            .collect(),
        pane_tail: verdict.pane_tail.clone(),
        error,
        next_steps,
        provider: None,
    })
}

fn display_name(name: &str) -> String {
    name.split('-')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ── the tool ─────────────────────────────────────────────────────────────────

/// Builds the drafting model's config at call time (`None` ⇒ no provider).
pub type LlmConfigSource = std::sync::Arc<dyn Fn() -> Option<LlmConfig> + Send + Sync>;

/// `add_harness` — the daemon tool.
pub struct AddHarnessTool {
    /// Where probes run (the session cwd).
    pub workspace: PathBuf,
    /// The drafting model, resolved per call so a provider added while the
    /// daemon runs is picked up by the tool (the turn itself still needs a
    /// restart — see `llm_provider`).
    pub llm: LlmConfigSource,
}

#[async_trait]
impl Tool for AddHarnessTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "add_harness".into(),
            description: "Add a coding-agent CLI (gemini, aider, cursor-agent, amp, pi, …) to SmoothFlow as a harness, agentically: probe its --help (and a docs page if given), draft a harness manifest, VALIDATE it by launching a real session on a private engine (launch → working → idle, steer, kill+resume), iterate on failures, install it to ~/.smooth/harnesses/<name>.toml and report what was proven and what was not. Takes minutes (each validation launches the CLI). Use when the user asks to add/support/onboard a new coding agent CLI in SmoothFlow. Needs an LLM provider on this daemon; without one it returns a structured needs_provider answer — relay its options verbatim."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Harness name: lowercase letters, digits, dashes — becomes the session kind and the file name, e.g. \"gemini\", \"aider\", \"cursor-agent\"." },
                    "binary_hint": { "type": "string", "description": "The executable's name or full path when it differs from the name (e.g. name \"gemini-cli\" → binary \"gemini\")." },
                    "docs_url": { "type": "string", "description": "Optional http(s) URL of the CLI's docs page (CLI reference / hooks page); fetched as markdown and given to the drafter." },
                    "max_iterations": { "type": "integer", "minimum": 1, "maximum": 6, "description": "Draft → validate rounds before giving up (default 3)." },
                    "force": { "type": "boolean", "description": "Replace an existing ~/.smooth/harnesses/<name>.toml (default false)." },
                    "install_unverified": { "type": "boolean", "description": "Install the best draft even when no run reached idle (default false)." },
                    "model": { "type": "string", "description": "A model name to pass through the manifest's {model} placeholder during validation (optional)." }
                },
                "required": ["name"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        // Launches processes, writes ~/.smooth/harnesses — one at a time.
        false
    }

    async fn execute(&self, arguments: Value) -> Result<String> {
        let req = parse_request(&arguments)?;
        let Some(cfg) = (self.llm)() else {
            let status = crate::llm_provider::status();
            return Ok(Report::needs_provider(&req.name, status).render());
        };
        let home = dirs_next::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve the home directory"))?;
        let path = std::env::var_os("PATH").unwrap_or_default();
        let drafter = LlmDrafter::new(cfg);
        let validator = BlockingValidator {
            inner: EngineValidator { budget: Budget::default() },
        };
        let report = run(&req, &home, &path, &self.workspace, &drafter, &validator).await?;
        Ok(report.render())
    }
}

/// Runs the (blocking, sleeping) engine validator on the blocking pool.
/// `Validator` is sync by design (the fake in tests is a plain script); this
/// adapter keeps the tokio runtime free while a real run sleeps.
struct BlockingValidator {
    inner: EngineValidator,
}

impl Validator for BlockingValidator {
    fn validate(&self, name: &str, manifest_toml: &str, model: Option<&str>) -> Result<Verdict> {
        let (name, toml, model, budget) = (name.to_string(), manifest_toml.to_string(), model.map(str::to_string), self.inner.budget);
        tokio::task::block_in_place(|| EngineValidator { budget }.validate(&name, &toml, model.as_deref()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use smooth_flow::harness_validate::{Outcome, Proof, Step};
    use std::sync::Mutex;

    fn ok_verdict(complete: bool) -> Verdict {
        let mut v = Verdict {
            usable: true,
            complete,
            state_source: "inferred".into(),
            pane_tail: "> ".into(),
            ..Verdict::default()
        };
        for s in Step::ALL {
            let outcome = if complete || matches!(s, Step::Launch | Step::Idle | Step::Working) {
                Outcome::Proven
            } else {
                Outcome::Unproven("not shown".into())
            };
            v.proofs.push(Proof { step: *s, outcome });
        }
        v
    }

    fn bad_verdict() -> Verdict {
        let mut v = Verdict {
            pane_tail: "Trust this folder? (y/n)".into(),
            state_source: "inferred".into(),
            ..Verdict::default()
        };
        v.proofs.push(Proof {
            step: Step::Launch,
            outcome: Outcome::Proven,
        });
        v.proofs.push(Proof {
            step: Step::Idle,
            outcome: Outcome::Failed("approval prompt".into()),
        });
        v
    }

    struct ScriptDrafter {
        replies: Mutex<Vec<String>>,
        seen: Mutex<Vec<String>>,
    }

    impl ScriptDrafter {
        fn new(replies: &[&str]) -> Self {
            Self {
                replies: Mutex::new(replies.iter().rev().map(ToString::to_string).collect()),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Drafter for ScriptDrafter {
        async fn draft(&self, brief: &Brief, attempts: &[Attempt]) -> Result<String> {
            self.seen.lock().unwrap().push(draft_user_message(brief, attempts));
            self.replies.lock().unwrap().pop().ok_or_else(|| anyhow::anyhow!("drafter exhausted"))
        }
    }

    struct ScriptValidator {
        verdicts: Mutex<Vec<Verdict>>,
        seen: Mutex<Vec<String>>,
        error: Option<String>,
    }

    impl ScriptValidator {
        fn new(verdicts: Vec<Verdict>) -> Self {
            Self {
                verdicts: Mutex::new(verdicts.into_iter().rev().collect()),
                seen: Mutex::new(Vec::new()),
                error: None,
            }
        }
    }

    impl Validator for ScriptValidator {
        fn validate(&self, _name: &str, toml: &str, _model: Option<&str>) -> Result<Verdict> {
            if let Some(e) = &self.error {
                anyhow::bail!("{e}");
            }
            self.seen.lock().unwrap().push(toml.to_string());
            Ok(self.verdicts.lock().unwrap().pop().unwrap_or_else(bad_verdict))
        }
    }

    /// A fake CLI on a private PATH: prints a commander-style help.
    fn fake_cli(dir: &Path, name: &str) -> PathBuf {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let p = bin.join(name);
        std::fs::write(
            &p,
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo \"faketool 1.2.3\";;\n  --help|-h) cat <<'EOF'\nUsage: faketool [options] [prompt...]\n\nArguments:\n  prompt   Initial prompt for the agent\n\nOptions:\n  --model <model>     Model to use\n  --resume [chatId]   Resume a chat session\n  -p, --print         Print responses and exit (non-interactive)\n  -h, --help          Display help\nEOF\n;;\n  *) echo \"faketool: unknown\" >&2; exit 2;;\nesac\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn manifest_toml(name: &str) -> String {
        format!(
            "name = \"{name}\"\n[binary]\nnames = [\"faketool\"]\n[launch]\nargv = [\"--model\", \"{{model}}\", \"{{prompt}}\"]\n[resume]\nargv = [\"--resume\", \"{{session_id}}\"]\nmode = \"resume_session\"\n[state]\nsource = \"scrape\"\n[state.scrape]\nidle = [\"> \"]\n"
        )
    }

    fn req(name: &str) -> Request {
        Request {
            name: name.into(),
            binary_hint: Some("faketool".into()),
            docs_url: None,
            max_iterations: 3,
            force: false,
            install_unverified: false,
            model: None,
        }
    }

    #[test]
    fn parse_request_validates_name_url_and_clamps_iterations() {
        let r = parse_request(&json!({"name": " Gemini ", "max_iterations": 99, "docs_url": "https://x/y", "force": true})).unwrap();
        assert_eq!(r.name, "gemini");
        assert_eq!(r.max_iterations, MAX_ITERATIONS);
        assert!(r.force);
        assert_eq!(parse_request(&json!({"name": "x"})).unwrap().max_iterations, DEFAULT_ITERATIONS);
        assert_eq!(parse_request(&json!({"name": "x", "max_iterations": 0})).unwrap().max_iterations, 1);
        assert!(parse_request(&json!({})).is_err());
        assert!(parse_request(&json!({"name": "Bad Name"})).is_err());
        assert!(parse_request(&json!({"name": "-x"})).is_err());
        assert!(parse_request(&json!({"name": "x", "docs_url": "ftp://nope"})).is_err());
        assert_eq!(parse_request(&json!({"name": "x", "binary_hint": "  "})).unwrap().binary_hint, None);
    }

    #[test]
    fn which_bin_prefers_home_dirs_then_path_and_skips_cmux_shims() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let real = fake_cli(&home.join(".local"), "tool"); // ~/.local/bin/tool
        let shim_dir = tmp.path().join("cmux-cli-shims");
        fake_cli(&shim_dir, "other"); // <tmp>/cmux-cli-shims/bin/other
        let elsewhere = fake_cli(&tmp.path().join("elsewhere"), "other");
        let path = std::env::join_paths([shim_dir.join("bin"), tmp.path().join("elsewhere").join("bin")]).unwrap();
        assert_eq!(which_bin(&["tool".into()], &home, &path), Some(real.clone()));
        assert_eq!(which_bin(&["other".into()], &home, &path), Some(elsewhere), "the shim dir is skipped");
        assert_eq!(which_bin(&["missing".into()], &home, &path), None);
        // An explicit path wins outright.
        assert_eq!(which_bin(&[real.to_string_lossy().into_owned()], &home, &path), Some(real.clone()));
        assert_eq!(home_relative(&real, &home).as_deref(), Some(".local/bin/tool"));
    }

    #[tokio::test]
    async fn probe_help_reads_the_fake_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_cli(tmp.path(), "faketool");
        let p = probe_help(&bin, tmp.path()).await.unwrap();
        assert_eq!(p.invocation, "--help");
        assert!(p.help.contains("Usage: faketool"));
        assert_eq!(p.version.as_deref(), Some("faketool 1.2.3"));
        let facts = harness_draft::parse_help(&p.help);
        assert_eq!(facts.positionals[0].name, "prompt");
    }

    #[tokio::test]
    async fn probe_help_fails_loudly_for_a_silent_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("mute");
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let err = probe_help(&p, tmp.path()).await.unwrap_err().to_string();
        assert!(err.contains("answered none of"), "{err}");
    }

    #[test]
    fn stdout_section_unframes_run_th_output() {
        assert_eq!(
            stdout_section("$ th crawl\nexit code: 0\n--- stdout ---\n# Title\n\nbody\n--- stderr ---\n"),
            "# Title\n\nbody"
        );
        assert_eq!(stdout_section("plain"), "plain");
    }

    #[tokio::test]
    async fn loop_retries_a_bad_draft_then_installs_a_complete_one() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fake_cli(&home.join(".local"), "faketool");
        let drafter = ScriptDrafter::new(&[
            "```toml\nname = \"tool\"\n[binary]\nnames = [\"faketool\"]\n[launch]\nargv = []\n```", // invalid: argv needs {prompt}
            &format!("```toml\n{}```", manifest_toml("tool")),
        ]);
        let validator = ScriptValidator::new(vec![ok_verdict(true)]);
        let r = run(&req("tool"), &home, std::ffi::OsStr::new(""), tmp.path(), &drafter, &validator)
            .await
            .unwrap();
        assert_eq!(r.status, Status::Installed, "{r:#?}");
        assert_eq!(r.iterations, 2);
        let path = install_path(&home, "tool");
        assert_eq!(r.path.as_deref(), Some(path.to_string_lossy().as_ref()));
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.starts_with("# tool — drafted by Big Smooth"), "{on_disk}");
        let m = Manifest::parse(&on_disk).unwrap();
        assert_eq!(
            m.binary.prefer_paths,
            vec![".local/bin/faketool"],
            "the resolved install path is filled in when the draft leaves it empty: {on_disk}"
        );
        assert_eq!(r.proven.len(), Step::ALL.len());
        assert!(r.unproven.is_empty());
        // The second draft saw the first attempt's parse error.
        let seen = drafter.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].contains("did not validate"), "{}", seen[1]);
        assert!(seen[0].contains("Ranked launch candidates"));
        assert!(seen[0].contains("faketool 1.2.3"));
        assert_eq!(validator.seen.lock().unwrap().len(), 1, "an unparseable draft is never run");
        let text = r.render();
        assert!(text.contains("installed — every step proven"));
        assert!(text.contains("```json"));
    }

    #[tokio::test]
    async fn loop_feeds_the_verdict_back_and_installs_partial_when_usable() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fake_cli(&home.join(".local"), "faketool");
        let m = format!("```toml\n{}```", manifest_toml("tool"));
        let drafter = ScriptDrafter::new(&[&m, &m, &m]);
        let validator = ScriptValidator::new(vec![bad_verdict(), ok_verdict(false), ok_verdict(false)]);
        let r = run(&req("tool"), &home, std::ffi::OsStr::new(""), tmp.path(), &drafter, &validator)
            .await
            .unwrap();
        assert_eq!(r.status, Status::InstalledPartial, "{r:#?}");
        assert_eq!(r.iterations, 3, "never complete ⇒ all rounds used");
        assert!(!r.unproven.is_empty());
        assert!(r.next_steps.iter().any(|n| n.contains("th flow new --kind tool")));
        assert!(r.next_steps.iter().any(|n| n.contains("scraped")), "{:?}", r.next_steps);
        let seen = drafter.seen.lock().unwrap();
        assert!(seen[1].contains("Trust this folder? (y/n)"), "the pane tail is fed back: {}", seen[1]);
        assert!(seen[1].contains("Idle pattern derived from that pane"));
        assert!(seen[1].contains("FAILED: approval prompt"));
    }

    #[tokio::test]
    async fn never_usable_is_drafted_not_installed_unless_asked() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fake_cli(&home.join(".local"), "faketool");
        let m = format!("```toml\n{}```", manifest_toml("tool"));
        let drafter = ScriptDrafter::new(&[&m, &m]);
        let validator = ScriptValidator::new(vec![bad_verdict(), bad_verdict()]);
        let mut rq = req("tool");
        rq.max_iterations = 2;
        let r = run(&rq, &home, std::ffi::OsStr::new(""), tmp.path(), &drafter, &validator).await.unwrap();
        assert_eq!(r.status, Status::Drafted);
        assert!(!install_path(&home, "tool").exists());
        assert!(r.manifest.contains("name = \"tool\""));
        assert!(r.error.as_deref().unwrap().contains("install_unverified"));
        assert!(r.render().contains("drafted, NOT installed"));
        // install_unverified ⇒ written anyway, still partial.
        let drafter = ScriptDrafter::new(&[&m]);
        let validator = ScriptValidator::new(vec![bad_verdict()]);
        rq.install_unverified = true;
        rq.max_iterations = 1;
        let r = run(&rq, &home, std::ffi::OsStr::new(""), tmp.path(), &drafter, &validator).await.unwrap();
        assert_eq!(r.status, Status::InstalledPartial);
        assert!(install_path(&home, "tool").exists());
    }

    #[tokio::test]
    async fn existing_manifest_needs_force() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fake_cli(&home.join(".local"), "faketool");
        let dest = install_path(&home, "tool");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, "old").unwrap();
        let m = format!("```toml\n{}```", manifest_toml("tool"));
        let drafter = ScriptDrafter::new(&[&m]);
        let validator = ScriptValidator::new(vec![ok_verdict(true)]);
        let r = run(&req("tool"), &home, std::ffi::OsStr::new(""), tmp.path(), &drafter, &validator)
            .await
            .unwrap();
        assert_eq!(r.status, Status::Drafted);
        assert!(r.error.as_deref().unwrap().contains("force"));
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old", "never clobbered");
        let drafter = ScriptDrafter::new(&[&m]);
        let validator = ScriptValidator::new(vec![ok_verdict(true)]);
        let mut rq = req("tool");
        rq.force = true;
        let r = run(&rq, &home, std::ffi::OsStr::new(""), tmp.path(), &drafter, &validator).await.unwrap();
        assert_eq!(r.status, Status::Installed);
        assert_ne!(std::fs::read_to_string(&dest).unwrap(), "old");
    }

    #[tokio::test]
    async fn wrong_name_in_draft_is_a_parse_error_not_a_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fake_cli(&home.join(".local"), "faketool");
        let drafter = ScriptDrafter::new(&[
            &format!("```toml\n{}```", manifest_toml("other")),
            &format!("```toml\n{}```", manifest_toml("tool")),
        ]);
        let validator = ScriptValidator::new(vec![ok_verdict(true)]);
        let r = run(&req("tool"), &home, std::ffi::OsStr::new(""), tmp.path(), &drafter, &validator)
            .await
            .unwrap();
        assert_eq!(r.status, Status::Installed);
        let seen = drafter.seen.lock().unwrap();
        assert!(seen[1].contains("`name` must be \"tool\""), "{}", seen[1]);
    }

    #[tokio::test]
    async fn missing_binary_fails_before_any_drafting() {
        let tmp = tempfile::tempdir().unwrap();
        let drafter = ScriptDrafter::new(&[]);
        let validator = ScriptValidator::new(vec![]);
        let r = run(&req("nothere"), tmp.path(), std::ffi::OsStr::new(""), tmp.path(), &drafter, &validator)
            .await
            .unwrap();
        assert_eq!(r.status, Status::Failed);
        assert!(r.error.as_deref().unwrap().contains("not installed"));
        assert!(drafter.seen.lock().unwrap().is_empty());
        assert!(r.render().contains("failed"));
    }

    #[tokio::test]
    async fn drafter_outage_falls_back_to_the_skeleton_once() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fake_cli(&home.join(".local"), "faketool");
        let drafter = ScriptDrafter::new(&[]); // always errors
        let validator = ScriptValidator::new(vec![ok_verdict(false)]);
        let r = run(&req("tool"), &home, std::ffi::OsStr::new(""), tmp.path(), &drafter, &validator)
            .await
            .unwrap();
        assert_eq!(r.status, Status::InstalledPartial, "{r:#?}");
        let seen = validator.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let m = Manifest::parse(&seen[0]).unwrap();
        assert_eq!(m.launch.argv, ["--model", "{model}", "{prompt}"], "the skeleton came from the fake CLI's help");
        assert_eq!(m.binary.prefer_paths, vec![".local/bin/faketool"]);
    }

    #[tokio::test]
    async fn validator_setup_error_ends_the_loop_with_a_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fake_cli(&home.join(".local"), "faketool");
        let drafter = ScriptDrafter::new(&[&format!("```toml\n{}```", manifest_toml("tool"))]);
        let mut validator = ScriptValidator::new(vec![]);
        validator.error = Some("tmux is not installed".into());
        let r = run(&req("tool"), &home, std::ffi::OsStr::new(""), tmp.path(), &drafter, &validator)
            .await
            .unwrap();
        assert_eq!(r.status, Status::Drafted);
        assert!(r.error.as_deref().unwrap().contains("tmux is not installed"), "{r:#?}");
        assert!(r.manifest.contains("name = \"tool\""));
    }

    #[test]
    fn needs_provider_report_carries_the_options() {
        let status = crate::llm_provider::status_from(None, None, None, Some(Path::new("/nonexistent")), None);
        let r = Report::needs_provider("gemini", status);
        assert_eq!(r.status, Status::NeedsProvider);
        assert_eq!(r.next_steps.len(), 2);
        assert!(r.next_steps[0].starts_with("Smoo AI Gateway"));
        let text = r.render();
        assert!(text.contains("needs a provider"));
        assert!(text.contains("\"status\": \"needs_provider\""));
    }

    #[tokio::test]
    async fn tool_gates_on_the_provider_before_touching_anything() {
        let tool = AddHarnessTool {
            workspace: std::env::temp_dir(),
            llm: std::sync::Arc::new(|| None),
        };
        let out = tool.execute(json!({"name": "gemini"})).await.unwrap();
        assert!(out.contains("needs_provider"), "{out}");
        assert!(tool.execute(json!({})).await.is_err(), "bad args are an error, not a report");
        let s = tool.schema();
        assert_eq!(s.name, "add_harness");
        assert!(s.description.contains("needs_provider"));
        assert!(!tool.is_concurrent_safe());
    }

    #[test]
    fn display_name_title_cases_dashes() {
        assert_eq!(display_name("cursor-agent"), "Cursor Agent");
        assert_eq!(display_name("aider"), "Aider");
    }

    #[test]
    fn draft_message_carries_schema_reference_and_docs() {
        let brief = Brief {
            name: "x".into(),
            binary_path: "/b/x".into(),
            binary_names: vec!["x".into()],
            prefer_paths: vec![],
            version: None,
            help_invocation: "--help".into(),
            help: "Usage: x".into(),
            docs: Some("# docs".into()),
            facts: HelpFacts::default(),
            candidates: harness_draft::argv_candidates(&HelpFacts::default()),
            skeleton_toml: "name = \"x\"\n".into(),
        };
        let m = draft_user_message(&brief, &[]);
        for needle in [
            "## Schema",
            "prompt_as",
            "name = \"claude\"",
            "## Docs page",
            "# docs",
            "## Skeleton",
            "Ranked launch candidates",
            "40%",
        ] {
            assert!(m.contains(needle), "missing {needle}");
        }
    }
}
