//! The deterministic half of the agentic "add a harness" (pearl th-473294):
//! `--help` text → the facts a manifest needs → ranked argv-template
//! candidates, scrape patterns derived from captured panes, and the manifest
//! skeleton an LLM then edits. Nothing here calls a model; everything is
//! unit-tested against real help texts (`tests/fixtures/help/*.txt`).
//!
//! The LLM's job (in `smooth-daemon`'s `add_harness` tool) is judgement over
//! these facts — which candidate, what the state source should be — not
//! parsing. Keeping the parsing here means a wrong guess is a test, not a
//! prompt tweak.

// Manifest placeholders (`{prompt}`, `{model}`) are literal braces, not format args.
#![allow(clippy::literal_string_with_formatting_args, reason = "manifest placeholders are literal braces")]

use std::collections::BTreeMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::harness::{Binary, HooksSpec, Install, Kill, Launch, Manifest, Origin, PromptAs, Resume, ResumeMode, ScrapeSpec, SessionIdMode, StateSource, Steer};

/// One option line of a help text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Flag {
    /// `--long` spellings (without the dashes), e.g. `["prompt-interactive"]`.
    pub long: Vec<String>,
    /// `-x` spellings (without the dash).
    pub short: Vec<String>,
    /// The value placeholder when the flag takes one (`MODEL`, `<key>`,
    /// `[chatId]`), else `None`.
    pub value: Option<String>,
    /// The description, continuation lines joined.
    pub help: String,
}

impl Flag {
    /// The spelling to use in an argv: the first long form, else the short.
    #[must_use]
    pub fn spelling(&self) -> String {
        self.long
            .first()
            .map(|l| format!("--{l}"))
            .or_else(|| self.short.first().map(|s| format!("-{s}")))
            .unwrap_or_default()
    }

    fn takes_value(&self) -> bool {
        self.value.is_some()
    }

    fn named(&self, name: &str) -> bool {
        self.long.iter().any(|l| l == name)
    }
}

/// One subcommand line.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Subcommand {
    pub name: String,
    pub help: String,
}

/// One positional argument (from a `Positionals:` / `Arguments:` section or
/// the usage line).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Positional {
    pub name: String,
    pub help: String,
}

/// Everything [`parse_help`] learns from a help text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HelpFacts {
    /// `usage:` lines, verbatim.
    pub usage: Vec<String>,
    pub flags: Vec<Flag>,
    pub subcommands: Vec<Subcommand>,
    pub positionals: Vec<Positional>,
    /// The help mentions hooks (a `hooks` subcommand or a flag about them) —
    /// the cue to prefer `state.source = "hooks"` once they are wired.
    pub mentions_hooks: bool,
}

impl HelpFacts {
    fn flag(&self, name: &str) -> Option<&Flag> {
        self.flags.iter().find(|f| f.named(name))
    }

    fn any_flag<'a>(&'a self, names: &[&str]) -> Option<&'a Flag> {
        names.iter().find_map(|n| self.flag(n))
    }
}

fn strip_ansi(s: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap_or_else(|_| Regex::new("$^").expect("fallback regex")));
    re.replace_all(s, "").into_owned()
}

/// Split an option line into its spec and help at the first run of two or
/// more spaces (`  -m, --model <m>    Model to use` → spec, help).
fn split_spec(line: &str) -> (String, String) {
    let t = line.trim_start();
    let mut idx = None;
    let bytes = t.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b' ' && bytes[i + 1] == b' ' {
            idx = Some(i);
            break;
        }
        i += 1;
    }
    idx.map_or_else(
        || (t.trim().to_string(), String::new()),
        |i| (t[..i].trim().to_string(), t[i..].trim().to_string()),
    )
}

/// Parse a flag spec (`-m, --model MODEL` / `--resume [chatId]` /
/// `--git | --no-git` / `--message COMMAND, --msg COMMAND, -m COMMAND`).
fn parse_flag_spec(spec: &str) -> Option<Flag> {
    let mut f = Flag::default();
    for tok in spec
        .split(|c: char| c == ',' || c == '|' || c.is_whitespace() || c == '=')
        .filter(|t| !t.is_empty())
    {
        if let Some(l) = tok.strip_prefix("--") {
            let l = l.trim_end_matches("...");
            if !l.is_empty() && !f.long.iter().any(|x| x == l) {
                f.long.push(l.to_string());
            }
        } else if let Some(s) = tok.strip_prefix('-') {
            if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()) && !f.short.iter().any(|x| x == s) {
                f.short.push(s.to_string());
            }
        } else if f.value.is_none() {
            let v = tok.trim_matches(|c| matches!(c, '<' | '>' | '[' | ']' | '.'));
            if !v.is_empty() {
                f.value = Some(v.to_string());
            }
        }
    }
    (!f.long.is_empty() || !f.short.is_empty()).then_some(f)
}

/// yargs-style type tags in the help column: `[string]` / `[array]` /
/// `[number]` mean a value; `[boolean]` means none.
fn apply_type_tag(f: &mut Flag) {
    let h = f.help.to_ascii_lowercase();
    if h.contains("[boolean]") {
        f.value = None;
    } else if f.value.is_none() && (h.contains("[string]") || h.contains("[array]") || h.contains("[number]")) {
        f.value = Some("VALUE".to_string());
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Section {
    None,
    Options,
    Commands,
    Positionals,
}

fn section_of(line: &str) -> Option<Section> {
    let t = line.trim().trim_end_matches(':').to_ascii_lowercase();
    if line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }
    match t.as_str() {
        "options" | "optional arguments" | "flags" => Some(Section::Options),
        "commands" | "subcommands" => Some(Section::Commands),
        "positionals" | "arguments" | "positional arguments" => Some(Section::Positionals),
        _ if t.ends_with(" options") || t.ends_with(" arguments") => Some(Section::Options),
        _ if t.ends_with(" commands") => Some(Section::Commands),
        _ => None,
    }
}

/// Parse a help text. Tolerant of clap, commander, yargs and argparse
/// layouts; anything it can't place is ignored, never an error.
///
/// # Panics
/// Never in practice: the only `expect` is a constant regex.
#[must_use]
#[allow(clippy::too_many_lines, reason = "one pass over the text; splitting it would scatter the section state")]
pub fn parse_help(text: &str) -> HelpFacts {
    let text = strip_ansi(text);
    let mut facts = HelpFacts::default();
    let mut section = Section::None;
    // Index of the entry a deeper-indented line continues, and that entry's
    // indent (continuation lines are indented past the entry they belong to).
    let mut last: Option<(usize, usize)> = None;
    let mut program: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        let indent = line.len() - line.trim_start().len();
        let lower = line.trim().to_ascii_lowercase();
        if lower.starts_with("usage:") || lower.starts_with("usage ") {
            facts.usage.push(line.trim().to_string());
            let rest = line.trim()[6..].trim();
            program = rest.split_whitespace().next().map(|p| p.trim_matches(':').to_string());
            section = Section::None;
            last = None;
            continue;
        }
        if let Some(s) = section_of(line) {
            section = s;
            last = None;
            continue;
        }
        if line.trim().is_empty() {
            last = None;
            continue;
        }
        let t = line.trim();
        let continues = last.is_some_and(|(_, at)| indent > at);
        // Option lines can appear outside an Options: header (argparse puts
        // them under "options:" / "optional arguments:"; some tools have none).
        if t.starts_with('-') && section != Section::Positionals {
            let (spec, help) = split_spec(line);
            // yargs indents long-only flags past the `-x, --long` ones; a line
            // with its own help column is a flag even when indented deeper. A
            // dash-led continuation line (`--resume <id>, continues that`)
            // falls through to the continuation handling below.
            let own_help = !help.is_empty() || !continues;
            if let Some(mut f) = parse_flag_spec(&spec).filter(|_| own_help) {
                f.help = help;
                apply_type_tag(&mut f);
                facts.flags.push(f);
                last = Some((facts.flags.len() - 1, indent));
                if section == Section::Commands {
                    section = Section::Options;
                }
                continue;
            }
        }
        match section {
            Section::Commands if indent > 0 => {
                if continues {
                    if let Some(sc) = last.and_then(|(i, _)| facts.subcommands.get_mut(i)) {
                        sc.help.push(' ');
                        sc.help.push_str(t);
                    }
                    continue;
                }
                let (spec, help) = split_spec(line);
                let mut spec = spec.as_str();
                if let Some(p) = &program {
                    spec = spec.strip_prefix(p.as_str()).map_or(spec, str::trim_start);
                }
                let first = spec.split_whitespace().next().unwrap_or("");
                // `[query..]` in a command list is the default positional, not a command.
                if first.is_empty() || first.starts_with('[') || first.starts_with('<') {
                    continue;
                }
                facts.subcommands.push(Subcommand {
                    name: first.split('|').next().unwrap_or(first).to_string(),
                    help,
                });
                last = Some((facts.subcommands.len() - 1, indent));
            }
            Section::Positionals if indent > 0 => {
                if continues {
                    if let Some(p) = last.and_then(|(i, _)| facts.positionals.get_mut(i)) {
                        p.help.push(' ');
                        p.help.push_str(t);
                    }
                    continue;
                }
                let (spec, help) = split_spec(line);
                let name = spec
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches(|c| matches!(c, '[' | ']' | '<' | '>' | '.'));
                if name.is_empty() {
                    continue;
                }
                facts.positionals.push(Positional { name: name.to_string(), help });
                last = Some((facts.positionals.len() - 1, indent));
            }
            Section::Options | Section::None if continues => {
                // Continuation of the previous option's help.
                if let Some(f) = last.and_then(|(i, _)| facts.flags.get_mut(i)) {
                    if !f.help.is_empty() {
                        f.help.push(' ');
                    }
                    f.help.push_str(t);
                    apply_type_tag(f);
                }
            }
            _ => {}
        }
    }
    // A positional named in the usage line only (`[prompt...]`, `[query..]`).
    if facts.positionals.is_empty() {
        static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"\[([a-zA-Z_-]+)\.{2,}\]").expect("usage positional regex"));
        for u in &facts.usage {
            for c in re.captures_iter(u) {
                facts.positionals.push(Positional {
                    name: c[1].to_string(),
                    help: String::new(),
                });
            }
        }
    }
    let lower = text.to_ascii_lowercase();
    facts.mentions_hooks = facts.subcommands.iter().any(|s| s.name == "hooks" || s.name == "hook")
        || facts.flags.iter().any(|f| f.long.iter().any(|l| l.contains("hook")))
        || lower.contains("hooks.json")
        || lower.contains("hooks.toml");
    facts
}

// ── candidates ───────────────────────────────────────────────────────────────

/// One way to launch + resume the harness, ranked by `confidence`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub launch_argv: Vec<String>,
    pub prompt_as: PromptAs,
    pub session_id: SessionIdMode,
    pub resume_argv: Vec<String>,
    pub resume_mode: ResumeMode,
    /// 0–100: how sure the heuristics are.
    pub confidence: u8,
    /// Why — one line per decision, shown to the LLM and in the report.
    pub rationale: Vec<String>,
}

/// Words in a flag's help that mean "this is a batch/headless mode, not the
/// interactive session SmoothFlow supervises".
const BATCH_WORDS: &[&str] = &[
    "non-interactive",
    "headless",
    "then exit",
    "and exit",
    "print",
    "script",
    "pipe",
    "disables chat",
    "stdout",
];

fn is_batch(f: &Flag) -> bool {
    let h = f.help.to_ascii_lowercase();
    BATCH_WORDS.iter().any(|w| h.contains(w))
}

const PROMPT_WORDS: &[&str] = &["prompt", "query", "message", "task", "instruction", "question"];

fn prompt_like(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    PROMPT_WORDS.iter().any(|w| l.contains(w))
}

/// Rank the ways the initial prompt could be handed over.
fn prompt_slots(facts: &HelpFacts) -> Vec<(Vec<String>, PromptAs, u8, String)> {
    let mut out = Vec::new();
    // 1. An interactive positional described as the prompt/query.
    if let Some(p) = facts.positionals.iter().find(|p| prompt_like(&p.name) || prompt_like(&p.help)) {
        let batch = BATCH_WORDS.iter().any(|w| p.help.to_ascii_lowercase().contains(w)) && !p.help.to_ascii_lowercase().contains("interactive mode by default");
        if !batch {
            out.push((
                vec!["{prompt}".to_string()],
                PromptAs::Argv,
                80,
                format!("positional `{}` is the initial prompt ({})", p.name, one_line(&p.help)),
            ));
        }
    }
    // 2. A flag that says "interactive" and takes the prompt.
    for f in &facts.flags {
        let h = f.help.to_ascii_lowercase();
        if f.takes_value() && h.contains("interactive") && !h.contains("non-interactive") && (prompt_like(&f.spelling()) || prompt_like(&f.help)) {
            out.push((
                vec![f.spelling(), "{prompt}".to_string()],
                PromptAs::Argv,
                70,
                format!("`{}` runs the prompt then stays interactive ({})", f.spelling(), one_line(&f.help)),
            ));
        }
    }
    // 3. A plain prompt/message/task flag that is not a batch mode.
    for name in ["prompt", "message", "task", "query", "initial-prompt"] {
        if let Some(f) = facts.flag(name) {
            if f.takes_value() && !is_batch(f) && !out.iter().any(|(a, ..)| a.first() == Some(&f.spelling())) {
                out.push((
                    vec![f.spelling(), "{prompt}".to_string()],
                    PromptAs::Argv,
                    60,
                    format!("`{}` takes the prompt ({})", f.spelling(), one_line(&f.help)),
                ));
            }
        }
    }
    // Batch-mode flags are called out so the LLM does not pick them.
    let skipped: Vec<String> = facts
        .flags
        .iter()
        .filter(|f| f.takes_value() && is_batch(f) && (prompt_like(&f.spelling()) || prompt_like(&f.help)))
        .map(Flag::spelling)
        .collect();
    // 4. Nothing — paste into the composer once the TUI is up.
    let mut why = "no interactive prompt flag or positional found; the prompt is pasted into the composer ~4s after launch".to_string();
    if !skipped.is_empty() {
        use std::fmt::Write as _;
        let _ = write!(why, " (skipped batch-mode flags: {})", skipped.join(", "));
    }
    out.push((Vec::new(), PromptAs::Paste, 40, why));
    out
}

/// The session-id / resume shape the help documents.
fn session_shape(facts: &HelpFacts) -> (Vec<String>, SessionIdMode, Vec<String>, ResumeMode, Vec<String>) {
    let mut launch = Vec::new();
    let mut why = Vec::new();
    let mut session_id = SessionIdMode::Learned;
    if let Some(f) = facts.any_flag(&["session-id", "session_id"]).filter(|f| f.takes_value()) {
        launch.push(f.spelling());
        launch.push("{session_id}".to_string());
        session_id = SessionIdMode::Preassigned;
        why.push(format!("`{}` lets the engine pre-assign the session id", f.spelling()));
    }
    let mut resume = Vec::new();
    let mut mode = ResumeMode::RelaunchCommand;
    if let Some(f) = facts
        .any_flag(&["resume", "session", "continue", "chat-id", "conversation"])
        .filter(|f| f.takes_value())
    {
        resume.push(f.spelling());
        resume.push("{session_id}".to_string());
        mode = ResumeMode::ResumeSession;
        why.push(format!("`{} <id>` resumes a session ({})", f.spelling(), one_line(&f.help)));
    } else if let Some(sc) = facts.subcommands.iter().find(|s| s.name == "resume") {
        resume.push("resume".to_string());
        resume.push("{session_id}".to_string());
        mode = ResumeMode::ResumeSession;
        why.push(format!("`resume <id>` subcommand resumes a session ({})", one_line(&sc.help)));
    } else if let Some(f) = facts.any_flag(&["restore-chat-history", "restore", "continue"]) {
        why.push(format!(
            "no `<id>` resume; `{}` restores the last history — relaunching the original command (consider adding it to launch.argv)",
            f.spelling()
        ));
    } else {
        why.push("no resume flag documented — a resume relaunches the original command".to_string());
    }
    if session_id == SessionIdMode::Learned && mode == ResumeMode::ResumeSession {
        why.push("the session id must be learned from a hook (state.source = hooks); until one lands, resume relaunches".to_string());
    }
    (launch, session_id, resume, mode, why)
}

/// Ranked argv candidates for `facts`. Never empty: the last resort is
/// "paste the prompt, relaunch to resume".
#[must_use]
pub fn argv_candidates(facts: &HelpFacts) -> Vec<Candidate> {
    let model = facts.flag("model").filter(|f| f.takes_value()).map(Flag::spelling);
    let (sess_launch, session_id, resume_argv, resume_mode, sess_why) = session_shape(facts);
    let mut out = Vec::new();
    for (prompt_argv, prompt_as, conf, why) in prompt_slots(facts) {
        let mut launch = sess_launch.clone();
        if let Some(m) = &model {
            launch.push(m.clone());
            launch.push("{model}".to_string());
        }
        launch.extend(prompt_argv);
        let mut rationale = vec![why];
        if let Some(m) = &model {
            rationale.push(format!("`{m} {{model}}` (dropped when no model is chosen)"));
        }
        rationale.extend(sess_why.iter().cloned());
        out.push(Candidate {
            launch_argv: launch,
            prompt_as,
            session_id,
            resume_argv: resume_argv.clone(),
            resume_mode,
            confidence: conf,
            rationale,
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.confidence));
    out
}

fn one_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 90 {
        format!("{}…", s.chars().take(89).collect::<String>())
    } else {
        s
    }
}

// ── scrape derivation ─────────────────────────────────────────────────────────

/// Shared usage-limit / approval / error patterns every drafted manifest
/// starts with (the same ones the built-ins carry).
pub const DEFAULT_NEEDS_YOU: &[&str] = &[
    "do you want to proceed",
    "\\(y/n\\)",
    "\\[y/n\\]",
    "\\(y\\)es/\\(n\\)o",
    "1\\. yes",
    "allow once",
    "press enter to confirm",
    // First-run dialogs (gemini's folder trust + auth method, any select
    // dialog): a real session must surface them as needs_you.
    "do you trust",
    "use enter to select",
    "press any key",
];
pub const DEFAULT_USAGE_LIMIT: &[&str] = &[
    "usage limit reached",
    "approaching usage limit",
    "limit will reset",
    "limit resets at",
    "out of credits",
    "quota exceeded",
    "rate limit",
];
pub const DEFAULT_ERROR: &[&str] = &[
    "api error",
    "fatal error",
    "request failed",
    "execution error",
    "unhandled exception",
    "traceback \\(most recent call last\\)",
];

/// Lines in a pane that read as a "the model is busy" status.
const WORKING_WORDS: &[&str] = &[
    "esc to interrupt",
    "esc to cancel",
    "esc interrupt",
    "interrupt",
    "thinking",
    "generating",
    "working",
    "running",
    "loading",
];

fn tail_lines(pane: &str, n: usize) -> Vec<String> {
    let lines: Vec<String> = pane.lines().map(|l| l.trim_end().to_string()).filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

/// The idle pattern: the harness's prompt marker. A short bare prompt line
/// (`> `, `❯`, `$`) becomes an anchored-ish literal; otherwise a footer hint
/// (`? for shortcuts`, `ctrl+p commands`), else the last line's tail.
fn idle_pattern(idle_pane: &str) -> Option<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let tail = tail_lines(idle_pane, 12);
    let last = tail.last()?;
    let t = last.trim();
    if !t.is_empty() && t.chars().count() <= 3 {
        return Some(regex::escape(t));
    }
    let re = RE.get_or_init(|| Regex::new(r"(?i)(\? for (?:help|shortcuts)|ctrl\+[a-z] [a-z]+|shift\+tab[^|]*|type your message[^|]*)").expect("footer regex"));
    for l in tail.iter().rev() {
        if let Some(m) = re.find(l) {
            return Some(regex::escape(m.as_str().trim()));
        }
    }
    // A prompt-looking prefix on the last line (`> `, `aider> `, `❯ `).
    let prefix: String = t.chars().take_while(|c| !c.is_alphanumeric() || t.chars().count() <= 12).collect();
    let prefix = prefix.trim();
    if !prefix.is_empty() && prefix.chars().count() <= 12 {
        return Some(regex::escape(prefix));
    }
    let short: String = t.chars().rev().take(24).collect::<Vec<_>>().into_iter().rev().collect();
    Some(regex::escape(short.trim()))
}

/// The working pattern: a status phrase in the working pane's tail that the
/// idle pane does not show.
fn working_pattern(idle_pane: &str, working_pane: &str) -> Option<String> {
    let idle: Vec<String> = tail_lines(idle_pane, 12).into_iter().map(|l| l.to_ascii_lowercase()).collect();
    for l in tail_lines(working_pane, 12).iter().rev() {
        let low = l.to_ascii_lowercase();
        if idle.iter().any(|i| i == &low) {
            continue;
        }
        if let Some(w) = WORKING_WORDS.iter().find(|w| low.contains(**w)) {
            // Prefer the longer canonical phrase when a shorter word matched.
            let phrase = WORKING_WORDS.iter().filter(|p| low.contains(**p)).max_by_key(|p| p.len()).unwrap_or(w);
            return Some(regex::escape(phrase));
        }
    }
    None
}

/// Derive `[state.scrape]` from a captured idle pane and (optionally) a
/// captured working pane. Defaults for the rest.
#[must_use]
pub fn scrape_from_panes(idle_pane: &str, working_pane: Option<&str>) -> ScrapeSpec {
    let idle = idle_pattern(idle_pane).into_iter().collect();
    let working = working_pane
        .and_then(|w| working_pattern(idle_pane, w))
        .map_or_else(|| vec!["esc to interrupt".to_string(), "esc to cancel".to_string()], |w| vec![w]);
    ScrapeSpec {
        working,
        idle,
        needs_you: DEFAULT_NEEDS_YOU.iter().map(ToString::to_string).collect(),
        usage_limit: DEFAULT_USAGE_LIMIT.iter().map(ToString::to_string).collect(),
        error: DEFAULT_ERROR.iter().map(ToString::to_string).collect(),
    }
}

// ── the skeleton ─────────────────────────────────────────────────────────────

/// What the skeleton is built from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Skeleton {
    pub name: String,
    pub display_name: String,
    pub binary_names: Vec<String>,
    pub prefer_paths: Vec<String>,
    pub candidate: Option<Candidate>,
    pub scrape: ScrapeSpec,
    pub state_source: StateSource,
    pub hooks_install: String,
    pub launch_env: BTreeMap<String, String>,
}

/// Build a validated-shape [`Manifest`] from a skeleton — the thing the LLM
/// edits, and the fallback when it produces nothing usable.
#[must_use]
pub fn draft_manifest(sk: &Skeleton) -> Manifest {
    let c = sk.candidate.clone().unwrap_or(Candidate {
        launch_argv: Vec::new(),
        prompt_as: PromptAs::Paste,
        session_id: SessionIdMode::Learned,
        resume_argv: Vec::new(),
        resume_mode: ResumeMode::RelaunchCommand,
        confidence: 0,
        rationale: Vec::new(),
    });
    Manifest {
        name: sk.name.clone(),
        display_name: if sk.display_name.trim().is_empty() {
            sk.name.clone()
        } else {
            sk.display_name.clone()
        },
        binary: Binary {
            names: sk.binary_names.clone(),
            prefer_paths: sk.prefer_paths.clone(),
            ..Binary::default()
        },
        launch: Launch {
            argv: c.launch_argv,
            prompt_as: c.prompt_as,
            session_id: c.session_id,
            env: sk.launch_env.clone(),
        },
        resume: Resume {
            argv: c.resume_argv,
            mode: c.resume_mode,
        },
        state: crate::harness::StateSpec {
            source: sk.state_source,
            hooks: HooksSpec {
                install: sk.hooks_install.clone(),
                event_map: BTreeMap::new(),
            },
            scrape: sk.scrape.clone(),
        },
        steer: Steer::default(),
        kill: Kill::default(),
        install: Install::default(),
        origin: Origin::Builtin,
    }
}

/// Render a manifest as the TOML `th harness add` accepts, with a provenance
/// header. Round-trips through [`Manifest::parse`].
///
/// # Errors
/// When the manifest cannot be serialized (never for a validated one) or the
/// rendered text fails to parse back.
pub fn render_toml(m: &Manifest, header: &str) -> anyhow::Result<String> {
    let body = toml::to_string_pretty(m)?;
    let mut out = String::new();
    for line in header.lines() {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&body);
    Manifest::parse(&out)?;
    Ok(out)
}

/// Pull the manifest TOML out of an LLM reply: the first fenced block
/// (```toml … ``` or ``` … ```) if there is one, else the whole text.
#[must_use]
pub fn extract_toml(reply: &str) -> String {
    let mut in_fence = false;
    let mut buf = String::new();
    for line in reply.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            if in_fence {
                return buf;
            }
            in_fence = true;
            continue;
        }
        if in_fence {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if in_fence && !buf.trim().is_empty() {
        return buf;
    }
    reply.trim().to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    const GEMINI: &str = include_str!("../tests/fixtures/help/gemini.txt");
    const AIDER: &str = include_str!("../tests/fixtures/help/aider.txt");
    const CURSOR: &str = include_str!("../tests/fixtures/help/cursor-agent.txt");
    const CLAUDE: &str = include_str!("../tests/fixtures/help/claude.txt");

    #[test]
    fn gemini_help_yields_positional_prompt_session_id_and_resume() {
        let facts = parse_help(GEMINI);
        assert!(facts.usage[0].starts_with("Usage: gemini"));
        assert!(facts.subcommands.iter().any(|s| s.name == "hooks"), "{:?}", facts.subcommands);
        assert!(facts.mentions_hooks);
        assert_eq!(facts.positionals[0].name, "query");
        let model = facts.flag("model").unwrap();
        assert_eq!(model.short, vec!["m"]);
        assert!(model.value.is_some(), "yargs [string] tag ⇒ takes a value");
        assert!(facts.flag("yolo").unwrap().value.is_none(), "[boolean] ⇒ no value");
        assert!(facts.flag("prompt").unwrap().help.contains("non-interactive"));

        let cands = argv_candidates(&facts);
        let best = &cands[0];
        assert_eq!(best.launch_argv, ["--session-id", "{session_id}", "--model", "{model}", "{prompt}"]);
        assert_eq!(best.prompt_as, PromptAs::Argv);
        assert_eq!(best.session_id, SessionIdMode::Preassigned);
        assert_eq!(best.resume_argv, ["--resume", "{session_id}"]);
        assert_eq!(best.resume_mode, ResumeMode::ResumeSession);
        assert_eq!(best.confidence, 80);
        // The headless `-p/--prompt` is never a candidate's prompt slot…
        assert!(cands.iter().all(|c| !c.launch_argv.contains(&"--prompt".to_string())), "{cands:?}");
        // …and the interactive `-i/--prompt-interactive` is offered second.
        assert!(cands.iter().any(|c| c.launch_argv.contains(&"--prompt-interactive".to_string())));
        // Paste is always the last resort.
        assert_eq!(cands.last().unwrap().prompt_as, PromptAs::Paste);
    }

    #[test]
    fn aider_help_yields_paste_and_relaunch() {
        let facts = parse_help(AIDER);
        let msg = facts.flag("message").unwrap();
        assert_eq!(msg.value.as_deref(), Some("COMMAND"));
        assert!(is_batch(msg), "`--message` exits after one reply: {}", msg.help);
        assert!(facts.flag("model").unwrap().takes_value());
        let cands = argv_candidates(&facts);
        let best = &cands[0];
        assert_eq!(best.prompt_as, PromptAs::Paste, "{best:?}");
        assert_eq!(best.launch_argv, ["--model", "{model}"]);
        assert_eq!(best.resume_mode, ResumeMode::RelaunchCommand);
        assert!(
            best.rationale.iter().any(|r| r.contains("--message")),
            "batch flag named in the rationale: {:?}",
            best.rationale
        );
        assert!(best.rationale.iter().any(|r| r.contains("restore-chat-history")), "{:?}", best.rationale);
        assert!(!facts.mentions_hooks);
    }

    #[test]
    fn cursor_agent_help_yields_positional_prompt_and_resume_flag() {
        let facts = parse_help(CURSOR);
        assert_eq!(facts.positionals[0].name, "prompt");
        let resume = facts.flag("resume").unwrap();
        assert_eq!(resume.value.as_deref(), Some("chatId"), "optional value in [brackets] still counts");
        assert!(
            facts.subcommands.iter().any(|s| s.name == "status"),
            "`status|whoami` keeps the first spelling: {:?}",
            facts.subcommands
        );
        let best = &argv_candidates(&facts)[0];
        assert_eq!(best.launch_argv, ["--model", "{model}", "{prompt}"]);
        assert_eq!(best.session_id, SessionIdMode::Learned);
        assert_eq!(best.resume_argv, ["--resume", "{session_id}"]);
        assert!(best.rationale.iter().any(|r| r.contains("learned from a hook")));
        // `-p/--print` is batch and never the prompt slot.
        assert!(!best.launch_argv.contains(&"--print".to_string()));
    }

    #[test]
    fn claude_help_matches_the_builtin_manifest_shape() {
        let facts = parse_help(CLAUDE);
        let best = &argv_candidates(&facts)[0];
        // The built-in claude.toml is exactly this argv.
        assert_eq!(
            best.launch_argv,
            ["--session-id", "{session_id}", "--model", "{model}", "{prompt}"],
            "{:?}",
            best.rationale
        );
        assert_eq!(best.resume_argv, ["--resume", "{session_id}"]);
        assert_eq!(best.session_id, SessionIdMode::Preassigned);
    }

    #[test]
    fn parse_flag_spec_handles_the_four_layouts() {
        let f = parse_flag_spec("-m, --model <model>").unwrap();
        assert_eq!(
            (f.short.clone(), f.long.clone(), f.value),
            (vec!["m".into()], vec!["model".into()], Some("model".into()))
        );
        let f = parse_flag_spec("--message COMMAND, --msg COMMAND, -m COMMAND").unwrap();
        assert_eq!(f.long, vec!["message", "msg"]);
        assert_eq!(f.value.as_deref(), Some("COMMAND"));
        let f = parse_flag_spec("--git | --no-git").unwrap();
        assert_eq!(f.long, vec!["git", "no-git"]);
        assert!(f.value.is_none());
        let f = parse_flag_spec("--resume [chatId]").unwrap();
        assert_eq!(f.value.as_deref(), Some("chatId"));
        assert!(parse_flag_spec("not a flag").is_none());
    }

    #[test]
    fn ansi_is_stripped_and_garbage_is_ignored() {
        let facts = parse_help("\x1b[1mUsage:\x1b[0m tool [options]\n\nOptions:\n  \x1b[32m--model <m>\x1b[0m   pick a model\n\nrandom trailing prose\n");
        assert_eq!(facts.usage, vec!["Usage: tool [options]"]);
        assert_eq!(facts.flags.len(), 1);
        assert_eq!(facts.flags[0].help, "pick a model");
        assert!(!facts.mentions_hooks);
    }

    #[test]
    fn empty_help_still_yields_the_paste_candidate() {
        let cands = argv_candidates(&parse_help(""));
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].prompt_as, PromptAs::Paste);
        assert!(cands[0].launch_argv.is_empty());
        assert_eq!(cands[0].resume_mode, ResumeMode::RelaunchCommand);
    }

    #[test]
    fn scrape_from_a_bare_prompt_pane() {
        let idle = "welcome to tool\n\nsome banner\n> ";
        let working = "welcome to tool\n\nsome banner\n> do it\n⠋ Thinking… (esc to interrupt)";
        let s = scrape_from_panes(idle, Some(working));
        assert_eq!(s.idle, vec![">"]);
        assert_eq!(s.working, vec!["esc to interrupt"]);
        assert!(!s.needs_you.is_empty() && !s.usage_limit.is_empty() && !s.error.is_empty());
        // First-run dialogs are needs_you by default (th-473294: gemini's trust
        // + auth dialogs, aider's (Y)es/(N)o, cursor's "press any key").
        for p in ["do you trust", "use enter to select", "press any key", "\\(y\\)es/\\(n\\)o"] {
            assert!(s.needs_you.iter().any(|n| n == p), "{p} missing from {:?}", s.needs_you);
        }
        // Every derived pattern compiles under the manifest validator.
        crate::harness::ScrapeRules::compile(&s).unwrap();
    }

    #[test]
    fn scrape_prefers_a_footer_hint_over_a_long_last_line() {
        let idle = "x\n~/proj main\n? for shortcuts        ctrl+p commands";
        let s = scrape_from_panes(idle, None);
        assert_eq!(s.idle, vec!["\\? for shortcuts"], "the leftmost footer hint wins");
        assert_eq!(s.working, vec!["esc to interrupt", "esc to cancel"], "no working pane ⇒ the shared defaults");
    }

    #[test]
    fn scrape_prompt_prefix_and_escaping() {
        let idle = "banner\naider> ";
        let s = scrape_from_panes(idle, None);
        assert_eq!(s.idle, vec!["aider>"]);
        let idle = "banner\n(y/n) [main] $ ";
        let s = scrape_from_panes(idle, None);
        assert!(Regex::new(&s.idle[0]).is_ok(), "{:?}", s.idle);
        assert!(s.idle[0].contains("\\$") || s.idle[0].contains("\\("), "escaped: {:?}", s.idle);
    }

    #[test]
    fn working_pattern_ignores_lines_the_idle_pane_also_shows() {
        let idle = "status: running fine\n> ";
        let working = "status: running fine\n> go\nGenerating response";
        let s = scrape_from_panes(idle, Some(working));
        assert_eq!(s.working, vec!["generating"]);
    }

    #[test]
    fn skeleton_renders_and_round_trips() {
        let facts = parse_help(GEMINI);
        let sk = Skeleton {
            name: "gemini".into(),
            display_name: "Gemini CLI".into(),
            binary_names: vec!["gemini".into()],
            prefer_paths: vec![".local/bin/gemini".into()],
            candidate: argv_candidates(&facts).into_iter().next(),
            scrape: scrape_from_panes("banner\n> ", None),
            state_source: StateSource::Scrape,
            hooks_install: String::new(),
            launch_env: BTreeMap::new(),
        };
        let m = draft_manifest(&sk);
        let toml = render_toml(&m, "drafted by add_harness\nsecond line").unwrap();
        assert!(toml.starts_with("# drafted by add_harness\n# second line\n"));
        let back = Manifest::parse(&toml).unwrap();
        assert_eq!(back.name, "gemini");
        assert_eq!(back.launch.argv, m.launch.argv);
        assert_eq!(back.state.source, StateSource::Scrape);
        assert_eq!(back.resume.mode, ResumeMode::ResumeSession);
        assert_eq!(back.display_name, "Gemini CLI");
    }

    #[test]
    fn skeleton_without_a_candidate_is_paste_and_still_valid() {
        let sk = Skeleton {
            name: "x".into(),
            binary_names: vec!["x".into()],
            scrape: scrape_from_panes("> ", None),
            state_source: StateSource::Scrape,
            ..Skeleton::default()
        };
        let m = draft_manifest(&sk);
        assert_eq!(m.launch.prompt_as, PromptAs::Paste);
        assert_eq!(m.display_name, "x");
        render_toml(&m, "").unwrap();
    }

    #[test]
    fn render_refuses_an_invalid_manifest() {
        let mut m = draft_manifest(&Skeleton {
            name: "x".into(),
            binary_names: vec!["x".into()],
            state_source: StateSource::Scrape,
            scrape: scrape_from_panes("> ", None),
            ..Skeleton::default()
        });
        m.launch.prompt_as = PromptAs::Argv; // no {prompt} in argv
        assert!(render_toml(&m, "").is_err());
    }

    #[test]
    fn extract_toml_prefers_the_fenced_block() {
        let reply = "Here you go:\n```toml\nname = \"x\"\n```\ntrailing notes";
        assert_eq!(extract_toml(reply), "name = \"x\"\n");
        assert_eq!(extract_toml("name = \"y\"\n"), "name = \"y\"");
        assert_eq!(
            extract_toml("```\nname = \"z\"\n"),
            "name = \"z\"\n",
            "unterminated fence still yields its body"
        );
    }
}
