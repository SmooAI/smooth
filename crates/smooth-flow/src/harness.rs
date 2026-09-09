//! Harness manifests (th-0f6126): one TOML per coding agent CLI.
//!
//! A manifest says how to find the binary, launch it, resume it, learn its
//! state, steer it and kill it. The engine's launch table, binary resolver and
//! pane scraper all read these; `claude`, `opencode`, `codex` and `th-code`
//! are the built-ins (`crates/smooth-flow/harnesses/*.toml`, embedded).
//!
//! Precedence, lowest to highest: built-in < `~/.smooth/harnesses/<name>.toml`
//! < `<project>/.smooth/harnesses/<name>.toml` < `th pkg` packages'
//! `harness/<name>/harness.toml`. A later manifest with the same `name`
//! replaces the earlier one wholesale (no field merging — a manifest is small
//! enough to copy). Schema + how to add one by hand:
//! `docs/Engineering/Harness-Manifests.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use smooth_tmux::detect::PaneState;

/// The embedded built-ins, in the order they are listed by default.
pub const BUILTIN: &[(&str, &str)] = &[
    ("claude", include_str!("../harnesses/claude.toml")),
    ("opencode", include_str!("../harnesses/opencode.toml")),
    ("codex", include_str!("../harnesses/codex.toml")),
    ("th-code", include_str!("../harnesses/th-code.toml")),
];

/// Any `PATH` entry under a directory with this name is a cmux CLI shim —
/// the default `binary.skip_path_patterns`.
pub const CMUX_SHIM_DIR: &str = "cmux-cli-shims";

/// Placeholders a launch/resume argv or env value may use.
pub const PLACEHOLDERS: &[&str] = &["{prompt}", "{session_id}", "{cwd}", "{model}", "{daemon_url}"];

/// Live signals (working / idle) render at the BOTTOM of the pane; only this
/// many trailing lines are consulted for them (same rule as
/// `smooth_tmux::detect`).
const LIVE_TAIL_LINES: usize = 12;

/// Where a manifest was loaded from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[default]
    Builtin,
    User(PathBuf),
    Project(PathBuf),
    Package(PathBuf),
}

impl Origin {
    /// The wire spelling: `builtin` | `user` | `project` | `package`.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User(_) => "user",
            Self::Project(_) => "project",
            Self::Package(_) => "package",
        }
    }

    /// The file it came from (`None` for a built-in).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Builtin => None,
            Self::User(p) | Self::Project(p) | Self::Package(p) => Some(p),
        }
    }
}

/// `[binary]` — how to find the executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binary {
    /// Candidate names on `PATH`, first match wins; `names[0]` is also the
    /// bare fallback when nothing resolves (so a missing CLI fails loudly as
    /// exit 127 in the pane).
    pub names: Vec<String>,
    /// `$HOME`-relative paths tried BEFORE `PATH` (the real installs).
    #[serde(default)]
    pub prefer_paths: Vec<String>,
    /// A `PATH` entry containing any of these path components is skipped.
    #[serde(default = "default_skip_patterns")]
    pub skip_path_patterns: Vec<String>,
}

fn default_skip_patterns() -> Vec<String> {
    vec![CMUX_SHIM_DIR.to_string()]
}

impl Default for Binary {
    fn default() -> Self {
        Self {
            names: Vec::new(),
            prefer_paths: Vec::new(),
            skip_path_patterns: default_skip_patterns(),
        }
    }
}

/// How the initial prompt reaches the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptAs {
    /// Through the `{prompt}` placeholder in `launch.argv`.
    #[default]
    Argv,
    /// Bracketed-paste + Enter into the composer once the TUI is up
    /// (harnesses with no prompt flag in interactive mode, e.g. `th code`).
    Paste,
}

/// Who mints the harness session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionIdMode {
    /// The engine mints a uuid and hands it over via `{session_id}`.
    Preassigned,
    /// The harness mints its own; the engine learns it from the first hook
    /// out of the session's worktree.
    #[default]
    Learned,
}

/// `[launch]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    /// argv AFTER the resolved binary. An element whose placeholder is empty
    /// is dropped, and so is a bare `-flag` literal right before it.
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub prompt_as: PromptAs,
    #[serde(default)]
    pub session_id: SessionIdMode,
    /// Extra environment for the pane; values take placeholders, an entry
    /// whose placeholder is empty is dropped.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// What `flow.kill {resume:true}` / a crash relaunch does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeMode {
    /// `binary + resume.argv` once the harness session id is known (else
    /// relaunch).
    ResumeSession,
    /// Always relaunch the original argv (a fresh session, not a continuation).
    #[default]
    RelaunchCommand,
}

/// `[resume]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resume {
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub mode: ResumeMode,
}

/// Where a session's state comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateSource {
    /// The harness posts hook events to `/api/flow/hooks` (an install step
    /// wires them); scraping covers limits + approvals hooks missed.
    #[default]
    Hooks,
    /// Pane scraping only.
    Scrape,
    /// The harness is ours and reports its own turns to the engine — no
    /// install step, no working/idle scraping.
    Native,
}

impl StateSource {
    /// The `Session.state_source` label once this source has spoken.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hooks => "hooks",
            Self::Scrape => "inferred",
            Self::Native => "native",
        }
    }
}

/// What a harness hook event means for the session (`state.hooks.event_map`
/// values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowEventName {
    Working,
    Idle,
    NeedsYou,
    Ended,
    Ignore,
}

/// `[state.hooks]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HooksSpec {
    /// Free text: how the hooks get wired on this machine.
    #[serde(default)]
    pub install: String,
    /// Harness event name → flow event. Empty ⇒ the Claude Code table
    /// (`protocol::map_hook_event`).
    #[serde(default)]
    pub event_map: BTreeMap<String, FlowEventName>,
}

/// `[state.scrape]` — case-insensitive regexes over the visible pane.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScrapeSpec {
    #[serde(default)]
    pub working: Vec<String>,
    #[serde(default)]
    pub idle: Vec<String>,
    #[serde(default)]
    pub needs_you: Vec<String>,
    /// May carry a `(?P<reset>…)` capture naming the reset-time text; without
    /// one the whole pane is parsed for it.
    #[serde(default)]
    pub usage_limit: Vec<String>,
    #[serde(default)]
    pub error: Vec<String>,
}

/// `[state]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSpec {
    #[serde(default)]
    pub source: StateSource,
    #[serde(default)]
    pub hooks: HooksSpec,
    #[serde(default)]
    pub scrape: ScrapeSpec,
}

/// How `flow.send` steers the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteerMethod {
    #[default]
    BracketedPaste,
    Stdin,
}

/// `[steer]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Steer {
    #[serde(default)]
    pub method: SteerMethod,
    #[serde(default = "default_submit_key")]
    pub submit_key: String,
}

fn default_submit_key() -> String {
    "Enter".to_string()
}

impl Default for Steer {
    fn default() -> Self {
        Self {
            method: SteerMethod::default(),
            submit_key: default_submit_key(),
        }
    }
}

/// `[kill]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kill {
    #[serde(default = "default_signal")]
    pub signal: String,
    #[serde(default = "default_grace_ms")]
    pub grace_ms: u64,
}

fn default_signal() -> String {
    "TERM".to_string()
}

const fn default_grace_ms() -> u64 {
    3000
}

impl Default for Kill {
    fn default() -> Self {
        Self {
            signal: default_signal(),
            grace_ms: default_grace_ms(),
        }
    }
}

/// `[install]` — where `th pkg` renders skills / rules / MCP for this harness.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Install {
    #[serde(default)]
    pub skills_dir: Option<String>,
    #[serde(default)]
    pub rules_dir: Option<String>,
    #[serde(default)]
    pub mcp_config: Option<String>,
}

/// One harness manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Lowercase `[a-z0-9-]+`; the session `kind` and the file/CLI name.
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub binary: Binary,
    #[serde(default)]
    pub launch: Launch,
    #[serde(default)]
    pub resume: Resume,
    #[serde(default)]
    pub state: StateSpec,
    #[serde(default)]
    pub steer: Steer,
    #[serde(default)]
    pub kill: Kill,
    #[serde(default)]
    pub install: Install,
    #[serde(skip)]
    pub origin: Origin,
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !s.starts_with('-')
}

fn check_placeholders(field: &str, values: impl Iterator<Item = String>) -> Result<()> {
    for v in values {
        let mut rest = v.as_str();
        while let Some(start) = rest.find('{') {
            let Some(len) = rest[start..].find('}') else {
                bail!("{field}: unclosed placeholder in `{v}`");
            };
            let ph = &rest[start..=start + len];
            if !PLACEHOLDERS.contains(&ph) {
                bail!("{field}: unknown placeholder `{ph}` in `{v}` (known: {})", PLACEHOLDERS.join(" "));
            }
            rest = &rest[start + len + 1..];
        }
    }
    Ok(())
}

impl Manifest {
    /// Parse + validate one manifest.
    ///
    /// # Errors
    /// A TOML error, an unknown field, or any semantic problem — each with the
    /// field named.
    pub fn parse(text: &str) -> Result<Self> {
        let mut m: Self = toml::from_str(text).map_err(|e| anyhow::anyhow!("{}", e.message()))?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&mut self) -> Result<()> {
        if !valid_name(&self.name) {
            bail!("name: `{}` must be lowercase letters, digits and dashes", self.name);
        }
        if self.display_name.trim().is_empty() {
            self.display_name.clone_from(&self.name);
        }
        if self.binary.names.is_empty() || self.binary.names.iter().any(|n| n.trim().is_empty()) {
            bail!("binary.names: at least one non-empty binary name is required");
        }
        check_placeholders("launch.argv", self.launch.argv.iter().cloned())?;
        check_placeholders("launch.env", self.launch.env.values().cloned())?;
        check_placeholders("resume.argv", self.resume.argv.iter().cloned())?;
        if self.launch.prompt_as == PromptAs::Argv && !self.launch.argv.iter().any(|a| a.contains("{prompt}")) {
            bail!("launch.argv: prompt_as = \"argv\" needs a `{{prompt}}` placeholder");
        }
        if self.launch.prompt_as == PromptAs::Paste && self.launch.argv.iter().any(|a| a.contains("{prompt}")) {
            bail!("launch.argv: prompt_as = \"paste\" must not also take `{{prompt}}`");
        }
        if self.resume.mode == ResumeMode::ResumeSession {
            if self.resume.argv.is_empty() {
                bail!("resume.argv: mode = \"resume_session\" needs an argv");
            }
            if !self.resume.argv.iter().any(|a| a.contains("{session_id}")) {
                bail!("resume.argv: mode = \"resume_session\" needs a `{{session_id}}` placeholder");
            }
        }
        if self.state.source == StateSource::Scrape && self.state.scrape.working.is_empty() && self.state.scrape.idle.is_empty() {
            bail!("state.scrape: source = \"scrape\" needs working and/or idle patterns");
        }
        ScrapeRules::compile(&self.state.scrape)?;
        if self.steer.submit_key.trim().is_empty() {
            bail!("steer.submit_key: must not be empty");
        }
        if self.kill.signal.trim().is_empty() {
            bail!("kill.signal: must not be empty");
        }
        Ok(())
    }

    /// The resolved executable, pure: `binary.prefer_paths` under `home`,
    /// then the first executable `binary.names` hit on `path` not inside a
    /// `skip_path_patterns` directory. `None` when nothing is installed.
    #[must_use]
    pub fn resolve_binary_in(&self, home: &Path, path: &std::ffi::OsStr) -> Option<PathBuf> {
        if let Some(p) = self.binary.prefer_paths.iter().map(|rel| home.join(rel)).find(|p| is_executable(p)) {
            return Some(p);
        }
        let skip = |dir: &Path| {
            dir.components()
                .any(|c| self.binary.skip_path_patterns.iter().any(|s| c.as_os_str() == s.as_str()))
        };
        for name in &self.binary.names {
            if let Some(p) = std::env::split_paths(path)
                .filter(|d| !skip(d))
                .map(|d| d.join(name))
                .find(|p| is_executable(p))
            {
                return Some(p);
            }
        }
        None
    }

    /// [`Self::resolve_binary_in`] against this process's `HOME` and `PATH`,
    /// falling back to the bare first name (exit 127 in the pane, loudly).
    #[must_use]
    pub fn resolve_binary(&self) -> String {
        let home = dirs_next::home_dir().unwrap_or_default();
        self.resolve_binary_in(&home, &std::env::var_os("PATH").unwrap_or_default())
            .map_or_else(|| self.binary.names[0].clone(), |p| p.to_string_lossy().into_owned())
    }

    /// Is `argv0` one of this harness's bare binary names?
    #[must_use]
    pub fn is_bare_name(&self, argv0: &str) -> bool {
        self.binary.names.iter().any(|n| n == argv0)
    }

    /// Map a hook event through `state.hooks.event_map`; `None` when the map
    /// is empty (⇒ use the Claude table) or the event isn't listed.
    #[must_use]
    pub fn map_event(&self, event: &str) -> Option<FlowEventName> {
        self.state.hooks.event_map.get(event).copied()
    }
}

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

// ── templating ────────────────────────────────────────────────────────────────

/// The values a launch/resume template is rendered with.
#[derive(Debug, Clone, Copy, Default)]
pub struct Vars<'a> {
    pub prompt: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub model: Option<&'a str>,
    pub daemon_url: Option<&'a str>,
}

impl Vars<'_> {
    fn get(&self, ph: &str) -> Option<&str> {
        let v = match ph {
            "{prompt}" => self.prompt,
            "{session_id}" => self.session_id,
            "{cwd}" => self.cwd,
            "{model}" => self.model,
            "{daemon_url}" => self.daemon_url,
            _ => None,
        };
        v.map(str::trim).filter(|v| !v.is_empty())
    }

    /// Substitute every placeholder in `s`; `None` if any is empty.
    fn render(&self, s: &str) -> Option<String> {
        let mut out = s.to_string();
        for ph in PLACEHOLDERS {
            if out.contains(ph) {
                out = out.replace(ph, self.get(ph)?);
            }
        }
        Some(out)
    }
}

/// Render an argv template: elements whose placeholder is empty are dropped,
/// and so is a bare `-flag` literal immediately before such an element.
#[must_use]
pub fn render_argv(template: &[String], vars: &Vars<'_>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(template.len());
    // Parallel to `out`: did that element come from a literal starting with `-`?
    let mut literal_flag: Vec<bool> = Vec::with_capacity(template.len());
    for el in template {
        let is_placeholder = el.contains('{');
        match vars.render(el) {
            Some(v) => {
                out.push(v);
                literal_flag.push(!is_placeholder && el.starts_with('-'));
            }
            None => {
                if literal_flag.last() == Some(&true) {
                    out.pop();
                    literal_flag.pop();
                }
            }
        }
    }
    out
}

/// Render an env template; entries whose placeholder is empty are dropped.
#[must_use]
pub fn render_env(template: &BTreeMap<String, String>, vars: &Vars<'_>) -> Vec<(String, String)> {
    template.iter().filter_map(|(k, v)| vars.render(v).map(|v| (k.clone(), v))).collect()
}

// ── scraping ──────────────────────────────────────────────────────────────────

/// Compiled `[state.scrape]` patterns.
#[derive(Debug, Clone)]
pub struct ScrapeRules {
    working: Vec<Regex>,
    idle: Vec<Regex>,
    needs_you: Vec<Regex>,
    usage_limit: Vec<Regex>,
    error: Vec<Regex>,
}

/// What a scrape saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrape {
    pub state: PaneState,
    /// The `reset` capture of the matching usage-limit pattern, if any.
    pub reset_text: Option<String>,
}

fn compile_all(field: &str, pats: &[String]) -> Result<Vec<Regex>> {
    pats.iter()
        .map(|p| Regex::new(&format!("(?i){p}")).with_context(|| format!("state.scrape.{field}: bad regex `{p}`")))
        .collect()
}

impl ScrapeRules {
    /// Compile the spec (every pattern gets `(?i)`).
    ///
    /// # Errors
    /// Naming the field and pattern that failed to compile.
    pub fn compile(spec: &ScrapeSpec) -> Result<Self> {
        Ok(Self {
            working: compile_all("working", &spec.working)?,
            idle: compile_all("idle", &spec.idle)?,
            needs_you: compile_all("needs_you", &spec.needs_you)?,
            usage_limit: compile_all("usage_limit", &spec.usage_limit)?,
            error: compile_all("error", &spec.error)?,
        })
    }

    /// Classify the visible pane with the same precedence as
    /// `smooth_tmux::detect::detect_state`: a live working hint in the tail
    /// wins, then a usage limit anywhere, then an approval, then an error,
    /// then an idle hint in the tail.
    #[must_use]
    pub fn detect(&self, pane: &str) -> Scrape {
        let tail = live_tail(pane);
        let any = |res: &[Regex], hay: &str| res.iter().any(|r| r.is_match(hay));
        if any(&self.working, &tail) {
            return Scrape {
                state: PaneState::Working,
                reset_text: None,
            };
        }
        if let Some(r) = self.usage_limit.iter().find(|r| r.is_match(pane)) {
            let reset_text = r.captures(pane).and_then(|c| c.name("reset")).map(|m| m.as_str().to_string());
            return Scrape {
                state: PaneState::UsageLimit,
                reset_text,
            };
        }
        let state = if any(&self.needs_you, pane) {
            PaneState::AwaitingApproval
        } else if any(&self.error, pane) {
            PaneState::Errored
        } else if any(&self.idle, &tail) {
            PaneState::Idle
        } else {
            PaneState::Unknown
        };
        Scrape { state, reset_text: None }
    }
}

/// The last [`LIVE_TAIL_LINES`] non-blank lines of `pane`, joined.
fn live_tail(pane: &str) -> String {
    let lines: Vec<&str> = pane.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(LIVE_TAIL_LINES);
    lines[start..].join("\n")
}

// ── registry ──────────────────────────────────────────────────────────────────

/// User preferences over the harness list: `order` first (names it lists, in
/// that order), then the rest in registry order; `hidden` are skipped by
/// pickers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub order: Vec<String>,
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// One row of `GET /api/flow/harnesses` / `flow.hello.harnesses`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessInfo {
    pub name: String,
    pub display_name: String,
    /// The session `kind` (same as `name`).
    pub kind: String,
    pub installed: bool,
    #[serde(default)]
    pub binary_path: Option<String>,
    /// `hooks` | `scrape` | `native`.
    pub state_source: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    pub order_index: usize,
    /// Why `installed` is false, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `builtin` | `user` | `project` | `package`.
    #[serde(default)]
    pub origin: String,
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if signature")]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Every manifest visible to the engine, precedence applied.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    manifests: Vec<Manifest>,
    /// Files that failed to load (path, error) — reported, never fatal.
    pub errors: Vec<(PathBuf, String)>,
}

impl Registry {
    /// Only the embedded built-ins.
    ///
    /// # Panics
    /// Never in practice: the built-ins are validated by tests.
    #[must_use]
    pub fn builtin() -> Self {
        let mut r = Self::default();
        for (name, text) in BUILTIN {
            match Manifest::parse(text) {
                Ok(m) => r.put(m),
                Err(e) => r.errors.push((PathBuf::from(format!("builtin:{name}")), e.to_string())),
            }
        }
        r
    }

    /// Built-ins, then every `.toml` in `~/.smooth/harnesses/`, then in
    /// `<project>/.smooth/harnesses/`, then `harness/<name>/harness.toml` of
    /// every package in `~/.smooth/pkg/index.toml` — later wins by `name`.
    #[must_use]
    pub fn load(home: &Path, project: Option<&Path>) -> Self {
        let mut r = Self::builtin();
        r.load_dir(&home.join(".smooth").join("harnesses"), Origin::User);
        if let Some(p) = project {
            r.load_dir(&p.join(".smooth").join("harnesses"), Origin::Project);
        }
        for root in package_roots(&home.join(".smooth").join("pkg").join("index.toml")) {
            let dir = root.join("harness");
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path().join("harness.toml")).filter(|p| p.is_file()).collect();
            files.sort();
            for f in files {
                r.load_file(&f, Origin::Package);
            }
        }
        r
    }

    fn load_dir(&mut self, dir: &Path, origin: fn(PathBuf) -> Origin) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml") && p.is_file())
            .collect();
        files.sort();
        for f in files {
            self.load_file(&f, origin);
        }
    }

    fn load_file(&mut self, file: &Path, origin: fn(PathBuf) -> Origin) {
        match std::fs::read_to_string(file)
            .map_err(|e| e.to_string())
            .and_then(|t| Manifest::parse(&t).map_err(|e| e.to_string()))
        {
            Ok(mut m) => {
                m.origin = origin(file.to_path_buf());
                self.put(m);
            }
            Err(e) => {
                tracing::warn!(file = %file.display(), error = %e, "harness manifest skipped");
                self.errors.push((file.to_path_buf(), e));
            }
        }
    }

    /// Insert or replace by name (keeping the first-seen position).
    pub fn put(&mut self, m: Manifest) {
        if let Some(slot) = self.manifests.iter_mut().find(|x| x.name == m.name) {
            *slot = m;
        } else {
            self.manifests.push(m);
        }
    }

    /// The manifest for a session kind / harness name.
    #[must_use]
    pub fn get(&self, kind: &str) -> Option<&Manifest> {
        self.manifests.iter().find(|m| m.name == kind)
    }

    /// Every manifest, registry order.
    #[must_use]
    pub fn all(&self) -> &[Manifest] {
        &self.manifests
    }

    /// Manifests in preference order (`prefs.order` first, the rest after).
    #[must_use]
    pub fn ordered(&self, prefs: &Prefs) -> Vec<&Manifest> {
        let mut out: Vec<&Manifest> = prefs.order.iter().filter_map(|n| self.get(n)).collect();
        for m in &self.manifests {
            if !out.iter().any(|x| x.name == m.name) {
                out.push(m);
            }
        }
        out
    }

    /// The wire rows: ordered, with install status resolved against `home`
    /// and `path`. `all = false` drops hidden ones (the `flow.hello` list).
    #[must_use]
    pub fn infos(&self, prefs: &Prefs, all: bool, home: &Path, path: &std::ffi::OsStr) -> Vec<HarnessInfo> {
        self.ordered(prefs)
            .into_iter()
            .enumerate()
            .filter(|(_, m)| all || !prefs.hidden.contains(&m.name))
            .map(|(i, m)| {
                let binary_path = m.resolve_binary_in(home, path).map(|p| p.to_string_lossy().into_owned());
                HarnessInfo {
                    name: m.name.clone(),
                    display_name: m.display_name.clone(),
                    kind: m.name.clone(),
                    installed: binary_path.is_some(),
                    reason: binary_path
                        .is_none()
                        .then(|| format!("`{}` not found on PATH{}", m.binary.names.join("`/`"), prefer_hint(m))),
                    binary_path,
                    state_source: match m.state.source {
                        StateSource::Hooks => "hooks",
                        StateSource::Scrape => "scrape",
                        StateSource::Native => "native",
                    }
                    .to_string(),
                    hidden: prefs.hidden.contains(&m.name),
                    order_index: i,
                    origin: m.origin.label().to_string(),
                }
            })
            .collect()
    }
}

fn prefer_hint(m: &Manifest) -> String {
    if m.binary.prefer_paths.is_empty() {
        String::new()
    } else {
        format!(" or ~/{}", m.binary.prefer_paths.join(", ~/"))
    }
}

/// The `root` of every package in a `th pkg` index (`packages.<name>.root`).
fn package_roots(index: &Path) -> Vec<PathBuf> {
    let Ok(raw) = std::fs::read_to_string(index) else { return Vec::new() };
    let Ok(doc) = raw.parse::<toml::Table>() else { return Vec::new() };
    doc.get("packages")
        .and_then(toml::Value::as_table)
        .map(|pk| {
            pk.values()
                .filter_map(|v| v.get("root").and_then(toml::Value::as_str))
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Validate a manifest file and return its parsed form (`th harness add`).
///
/// # Errors
/// When the file can't be read or fails validation.
pub fn load_file(path: &Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Manifest::parse(&text).with_context(|| format!("{}: invalid harness manifest", path.display()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn claude() -> Manifest {
        Manifest::parse(BUILTIN[0].1).unwrap()
    }

    #[test]
    fn every_builtin_parses_and_validates() {
        let r = Registry::builtin();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let names: Vec<&str> = r.all().iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["claude", "opencode", "codex", "th-code"]);
        assert_eq!(r.get("claude").unwrap().display_name, "Claude Code");
        assert_eq!(r.get("th-code").unwrap().state.source, StateSource::Native);
        assert_eq!(r.get("th-code").unwrap().map_event("turn_end"), Some(FlowEventName::Idle));
        assert_eq!(r.get("codex").unwrap().launch.session_id, SessionIdMode::Learned);
        assert_eq!(r.get("claude").unwrap().launch.session_id, SessionIdMode::Preassigned);
    }

    #[test]
    fn minimal_manifest_gets_defaults() {
        let m = Manifest::parse("name = \"pi\"\n[binary]\nnames = [\"pi\"]\n[launch]\nargv = [\"{prompt}\"]\n").unwrap();
        assert_eq!(m.display_name, "pi");
        assert_eq!(m.binary.skip_path_patterns, vec![CMUX_SHIM_DIR]);
        assert_eq!(m.launch.prompt_as, PromptAs::Argv);
        assert_eq!(m.launch.session_id, SessionIdMode::Learned);
        assert_eq!(m.resume.mode, ResumeMode::RelaunchCommand);
        assert_eq!(m.state.source, StateSource::Hooks);
        assert_eq!(m.steer.submit_key, "Enter");
        assert_eq!(m.kill.grace_ms, 3000);
        assert_eq!(m.kill.signal, "TERM");
        assert_eq!(m.origin, Origin::Builtin);
    }

    /// Every validation error path names its field.
    #[test]
    fn validation_errors_name_the_field() {
        let cases: &[(&str, &str)] = &[
            ("name = \"Bad Name\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{prompt}\"]", "name:"),
            ("name = \"x\"\n[launch]\nargv=[\"{prompt}\"]", "binary.names"),
            ("name = \"x\"\n[binary]\nnames=[\"\"]\n[launch]\nargv=[\"{prompt}\"]", "binary.names"),
            (
                "name = \"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{nope}\"]",
                "unknown placeholder `{nope}`",
            ),
            ("name = \"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{prompt\"]", "unclosed placeholder"),
            ("name = \"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[]", "needs a `{prompt}`"),
            (
                "name = \"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{prompt}\"]\nprompt_as=\"paste\"",
                "must not also take `{prompt}`",
            ),
            (
                "name = \"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{prompt}\"]\n[resume]\nmode=\"resume_session\"",
                "resume.argv",
            ),
            (
                "name = \"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{prompt}\"]\n[resume]\nmode=\"resume_session\"\nargv=[\"--resume\"]",
                "needs a `{session_id}`",
            ),
            (
                "name = \"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{prompt}\"]\n[state]\nsource=\"scrape\"",
                "state.scrape",
            ),
            (
                "name = \"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{prompt}\"]\n[state.scrape]\nidle=[\"(\"]",
                "state.scrape.idle: bad regex",
            ),
            (
                "name = \"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{prompt}\"]\n[steer]\nsubmit_key=\"\"",
                "steer.submit_key",
            ),
            (
                "name = \"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{prompt}\"]\n[kill]\nsignal=\"\"",
                "kill.signal",
            ),
            ("name = \"x\"\nbogus = 1\n[binary]\nnames=[\"x\"]", "unknown field"),
            ("name = \"x\"\n[state]\nsource=\"telepathy\"", "unknown variant"),
            ("name = \"x\"\n[state.hooks.event_map]\nfoo=\"nope\"", "unknown variant"),
            ("this is not toml", "expected"),
        ];
        for (text, needle) in cases {
            let err = Manifest::parse(text).unwrap_err().to_string();
            assert!(err.contains(needle), "for {text:?}: expected `{needle}` in `{err}`");
        }
    }

    #[test]
    fn argv_templating_drops_empty_placeholders_and_their_flags() {
        let m = claude();
        let full = Vars {
            prompt: Some("hi"),
            session_id: Some("u"),
            model: Some("opus"),
            ..Default::default()
        };
        assert_eq!(render_argv(&m.launch.argv, &full), vec!["--session-id", "u", "--model", "opus", "hi"]);
        let bare = Vars {
            prompt: Some("  "),
            ..Default::default()
        };
        assert!(render_argv(&m.launch.argv, &bare).is_empty());
        let no_model = Vars {
            prompt: Some("p"),
            session_id: Some("u"),
            ..Default::default()
        };
        assert_eq!(render_argv(&m.launch.argv, &no_model), vec!["--session-id", "u", "p"]);
        // A literal that is not a bare flag stays.
        let t = vec!["code".to_string(), "{model}".to_string()];
        assert_eq!(render_argv(&t, &Vars::default()), vec!["code"]);
        // Placeholder inside a longer element renders or drops as a unit.
        let t = vec!["--model={model}".to_string(), "--cwd={cwd}".to_string()];
        assert_eq!(
            render_argv(
                &t,
                &Vars {
                    cwd: Some("/w"),
                    ..Default::default()
                }
            ),
            vec!["--cwd=/w"]
        );
        // Two placeholder elements in a row: only the empty one drops.
        let t = vec!["--session".to_string(), "{session_id}".to_string(), "{prompt}".to_string()];
        assert_eq!(
            render_argv(
                &t,
                &Vars {
                    session_id: Some("s"),
                    ..Default::default()
                }
            ),
            vec!["--session", "s"]
        );
        assert_eq!(
            render_argv(
                &t,
                &Vars {
                    prompt: Some("p"),
                    ..Default::default()
                }
            ),
            vec!["p"],
            "the flag goes with its own empty value, not with a later one"
        );
    }

    #[test]
    fn env_templating() {
        let m = Registry::builtin().get("th-code").unwrap().clone();
        let vars = Vars {
            session_id: Some("fs-1"),
            daemon_url: Some("http://127.0.0.1:1"),
            ..Default::default()
        };
        assert_eq!(
            render_env(&m.launch.env, &vars),
            vec![
                ("SMOOTH_FLOW_SESSION".to_string(), "fs-1".to_string()),
                ("SMOOTH_URL".to_string(), "http://127.0.0.1:1".to_string())
            ]
        );
        assert_eq!(
            render_env(
                &m.launch.env,
                &Vars {
                    session_id: Some("fs-1"),
                    ..Default::default()
                }
            ),
            vec![("SMOOTH_FLOW_SESSION".to_string(), "fs-1".to_string())],
            "an env entry with an empty placeholder is dropped"
        );
    }

    #[test]
    #[cfg(unix)]
    fn resolver_prefers_homes_then_path_and_skips_shims() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mk = |rel: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let shim = mk("T/cmux-cli-shims/ABC/claude");
        mk("T/cmux-cli-shims/ABC/codex");
        let real = mk("usr/bin/claude");
        let path = std::env::join_paths([shim.parent().unwrap(), real.parent().unwrap()]).unwrap();
        let home = tmp.path().join("home");
        let r = Registry::builtin();
        let claude = r.get("claude").unwrap();
        assert_eq!(claude.resolve_binary_in(&home, &path), Some(real.clone()));
        assert_eq!(r.get("codex").unwrap().resolve_binary_in(&home, &path), None, "only a shim ⇒ nothing");
        let local = mk("home/.local/bin/claude");
        assert_eq!(claude.resolve_binary_in(&home, &path), Some(local.clone()));
        let oc = mk("home/.opencode/bin/opencode");
        assert_eq!(r.get("opencode").unwrap().resolve_binary_in(&home, &path), Some(oc));
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(claude.resolve_binary_in(&home, &path), Some(real), "a non-executable file is not a binary");
        // A custom skip pattern and a second name.
        let m = Manifest::parse("name=\"x\"\n[binary]\nnames=[\"nope\",\"claude\"]\nskip_path_patterns=[\"usr\"]\n[launch]\nargv=[\"{prompt}\"]").unwrap();
        assert_eq!(m.resolve_binary_in(&home, &path), Some(shim), "custom skip list replaces the default");
        assert!(claude.is_bare_name("claude") && !claude.is_bare_name("/x/claude"));
    }

    /// The manifest scraper classifies exactly like `smooth_tmux::detect`
    /// did for each kind's captured panes.
    #[test]
    fn scrape_rules_match_the_shared_detector_on_captured_panes() {
        use smooth_tmux::detect::detect_state;
        let r = Registry::builtin();
        let rules = |k: &str| ScrapeRules::compile(&r.get(k).unwrap().state.scrape).unwrap();
        let claude = rules("claude");
        let claude_panes = [
            "You've reached your usage limit. limit will reset at 4pm.",
            "● API Error: Server is temporarily limiting requests (not your usage limit) · Rate limited",
            "Edit file foo.rs?\n  Do you want to proceed?\n  ❯ 1. Yes\n  2. No",
            "● Thinking…\n  (esc to interrupt · 1.2k tokens)",
            "● API Error: something went wrong\n● Thinking…\n  (esc to interrupt · 200 tokens)",
            "╭─────────╮\n│ >       │\n╰─────────╯\n  ? for shortcuts",
            "just some neutral build output here",
            "USAGE LIMIT REACHED",
            "● API Error: boom\n",
        ];
        for p in claude_panes {
            assert_eq!(claude.detect(p).state, detect_state(p), "claude: {p:?}");
        }
        let mut pane = String::from("Quick safety check\n  Enter to confirm · Esc to cancel\n");
        for i in 0..20 {
            use std::fmt::Write as _;
            let _ = writeln!(pane, "output line {i}");
        }
        pane.push_str("❯ \n  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents\n");
        assert_eq!(claude.detect(&pane).state, PaneState::Idle);
        pane.push_str("● Thinking… (esc to interrupt)\n");
        assert_eq!(claude.detect(&pane).state, PaneState::Working);

        let oc = rules("opencode");
        let oc_working = "  ┃  Build · GPT-5.6 Sol OpenAI · high\n  ╹▀▀▀▀\n   ⬝⬝⬝⬝■■■■  esc interrupt        tab agents  ctrl+p commands";
        let oc_idle = "     ok\n     ▣  Build · GPT-5.6 Sol · 5.4s\n  ┃  Build · GPT-5.6 Sol OpenAI · high\n  ╹▀▀▀▀\n   /tmp/probe         12.5K (3% ctrl+p\n                       commands";
        for p in [oc_working, oc_idle] {
            assert_eq!(oc.detect(p).state, detect_state(p), "opencode: {p:?}");
        }
        let cx = rules("codex");
        let codex_trust = "  Do you trust the contents of this directory?\n› 1. Yes, continue\n  2. No, quit\n  Press enter to continue";
        let codex_hooks = "  Hooks need review\n› 1. Review hooks\n  2. Trust all and continue\n  Press enter to confirm or esc to go back";
        for p in [codex_trust, codex_hooks] {
            assert_eq!(cx.detect(p).state, detect_state(p), "codex: {p:?}");
        }
        // th code scrapes nothing but a limit.
        let th = rules("th-code");
        assert_eq!(th.detect("> \n? for shortcuts").state, PaneState::Unknown);
        assert_eq!(th.detect("usage limit reached").state, PaneState::UsageLimit);
    }

    #[test]
    fn usage_limit_reset_capture() {
        let spec = ScrapeSpec {
            usage_limit: vec![r"quota exhausted, back at (?P<reset>\d{1,2}(?::\d{2})?\s*[ap]m)".into()],
            ..Default::default()
        };
        let rules = ScrapeRules::compile(&spec).unwrap();
        let s = rules.detect("…\nquota exhausted, back at 4:30 pm\n");
        assert_eq!(s.state, PaneState::UsageLimit);
        assert_eq!(s.reset_text.as_deref(), Some("4:30 pm"));
        let claude = ScrapeRules::compile(&claude().state.scrape).unwrap();
        assert_eq!(claude.detect("limit will reset at 4pm").reset_text, None, "no capture ⇒ parse the pane");
    }

    #[test]
    fn registry_precedence_user_project_package() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        let w = |p: &Path, text: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        let toml =
            |name: &str, display: &str| format!("name=\"{name}\"\ndisplay_name=\"{display}\"\n[binary]\nnames=[\"{name}\"]\n[launch]\nargv=[\"{{prompt}}\"]\n");
        // Nothing on disk ⇒ built-ins only.
        assert_eq!(Registry::load(&home, Some(&project)).all().len(), 4);
        // User overrides a built-in and adds one; a broken file is reported, not fatal.
        w(&home.join(".smooth/harnesses/claude.toml"), &toml("claude", "User Claude"));
        w(&home.join(".smooth/harnesses/aider.toml"), &toml("aider", "Aider"));
        w(&home.join(".smooth/harnesses/broken.toml"), "name = \"broken\"\n");
        w(&home.join(".smooth/harnesses/notes.md"), "ignored");
        let r = Registry::load(&home, Some(&project));
        assert_eq!(r.get("claude").unwrap().display_name, "User Claude");
        assert!(matches!(r.get("claude").unwrap().origin, Origin::User(_)));
        assert_eq!(r.get("aider").unwrap().display_name, "Aider");
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].0.ends_with("broken.toml"));
        assert_eq!(r.all().len(), 5, "override keeps its slot, new one appends");
        assert_eq!(r.all()[0].name, "claude");
        // Project beats user.
        w(&project.join(".smooth/harnesses/aider.toml"), &toml("aider", "Project Aider"));
        let r = Registry::load(&home, Some(&project));
        assert_eq!(r.get("aider").unwrap().display_name, "Project Aider");
        assert!(matches!(r.get("aider").unwrap().origin, Origin::Project(_)));
        // A th pkg package beats project.
        let pkg = tmp.path().join("pkgroot");
        w(&pkg.join("harness/aider/harness.toml"), &toml("aider", "Pkg Aider"));
        w(&pkg.join("harness/amp/harness.toml"), &toml("amp", "Amp"));
        // Serialize the path with toml (a Windows path's backslashes would be
        // invalid escapes in a hand-written basic string).
        let index = toml::toml! {
            [packages.p]
            version = "1"
            source = "x"
            installed_at = "t"
            root = (pkg.to_string_lossy().into_owned())
            harnesses = []
        };
        w(&home.join(".smooth/pkg/index.toml"), &toml::to_string(&index).unwrap());
        let r = Registry::load(&home, Some(&project));
        assert_eq!(r.get("aider").unwrap().display_name, "Pkg Aider");
        assert!(matches!(r.get("aider").unwrap().origin, Origin::Package(_)));
        assert_eq!(r.get("amp").unwrap().origin.label(), "package");
        assert_eq!(r.all().len(), 6);
    }

    #[test]
    fn ordering_and_infos_honour_prefs() {
        let r = Registry::builtin();
        let prefs = Prefs {
            order: vec!["th-code".into(), "nope".into(), "codex".into()],
            hidden: vec!["opencode".into()],
        };
        let names: Vec<&str> = r.ordered(&prefs).iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            ["th-code", "codex", "claude", "opencode"],
            "listed first, unknown skipped, rest in registry order"
        );
        let tmp = tempfile::tempdir().unwrap();
        let empty = std::ffi::OsString::new();
        let all = r.infos(&prefs, true, tmp.path(), &empty);
        assert_eq!(all.len(), 4);
        assert_eq!(all[3].name, "opencode");
        assert!(all[3].hidden);
        assert_eq!(all[3].order_index, 3);
        assert!(!all[0].installed);
        assert!(all[0].reason.as_deref().unwrap().contains("`th` not found on PATH or ~/.cargo/bin/th"));
        assert_eq!(all[0].state_source, "native");
        assert_eq!(all[0].origin, "builtin");
        let visible = r.infos(&prefs, false, tmp.path(), &empty);
        assert_eq!(visible.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["th-code", "codex", "claude"]);
        let v: serde_json::Value = serde_json::to_value(&visible[0]).unwrap();
        assert!(v.get("hidden").is_none(), "false hidden is omitted on the wire: {v}");
    }
}
