//! Harness conformance (pearl th-3cabf6): the contract every harness manifest
//! must pass, and the pure pieces the fake agent and the rig share.
//!
//! "Support for harness X" is only worth something if a regression is caught.
//! The conformance suite (`crates/smooth-flow/tests/harness_conformance.rs`)
//! runs every built-in manifest against `smooth-flow-fake-agent` — a scripted
//! stand-in that speaks **that manifest's** state mechanism, derived from the
//! manifest itself — through a private [`crate::Engine`]:
//!
//! resolve · launch · working · idle · steer · permission · resume · kill
//!
//! Nothing here launches a real CLI or touches the network. A manifest plugs
//! in by being listed in [`crate::harness::BUILTIN`]; a scrape manifest also
//! ships a fixture of screens captured from the real CLI
//! (`tests/conformance/<name>.toml`, see [`Fixture`]).
//!
//! How the fake learns what to do ([`FakeSpec::for_manifest`]):
//! - **argv** — it matches its own argv against `launch.argv` / `resume.argv`
//!   ([`match_argv`]), so a manifest whose template does not round-trip fails
//!   `launch`.
//! - **hooks, empty `event_map`** — Claude Code's event names, with the
//!   `PermissionRequest` long-poll.
//! - **hooks / native with an `event_map`** — the map inverted: the events it
//!   maps to working / idle / needs_you / ended. A needs_you event is answered
//!   by the approval keystroke.
//! - **scrape** — posts nothing; paints the fixture's screens verbatim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use smooth_tmux::detect::PaneState;

use crate::harness::{FlowEventName, Manifest, PromptAs, ResumeMode, ScrapeRules, SessionIdMode, StateSource, PLACEHOLDERS};

/// The env var the fake agent reads its [`FakeSpec`] path from.
pub const SPEC_ENV: &str = "SMOOTH_CONFORMANCE_SPEC";
/// A steer line containing this asks the fake for a permission turn.
pub const PERMISSION_MARKER: &str = "conformance-permission";
/// The fake's default screens for harnesses whose state is not scraped.
pub const DEFAULT_BOOT: &str = "fake agent ready (conformance)";
pub const DEFAULT_WORKING: &str = "fake agent busy (conformance)";
pub const DEFAULT_IDLE: &str = "fake agent at rest (conformance)";
pub const DEFAULT_NEEDS_YOU: &str = "fake agent waiting on an approval (conformance)";

/// The steps of the contract, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// The binary resolves under the manifest's rules: `prefer_paths` under
    /// HOME before PATH, a cmux shim first on PATH skipped.
    Resolve,
    /// The engine launched `[bin] + render(launch.argv)` and the fake could
    /// read its prompt / session id back out of that argv.
    Launch,
    /// `working` observed for the launch prompt.
    Working,
    /// The first turn reached `idle`.
    Idle,
    /// A steer went `working` → `idle`.
    Steer,
    /// A permission ask reached `needs_you`, `flow.approve` reached the
    /// harness, and the turn finished (only when the manifest claims one).
    Permission,
    /// Kill + resume relaunched with the manifest's resume argv and the
    /// resumed process drove the same row back to `idle`.
    Resume,
    /// Kill without resume: `done`, process gone, tmux session gone.
    Kill,
}

impl Step {
    /// Every step, in the order the rig runs them.
    pub const ALL: &'static [Self] = &[
        Self::Resolve,
        Self::Launch,
        Self::Working,
        Self::Idle,
        Self::Steer,
        Self::Permission,
        Self::Resume,
        Self::Kill,
    ];

    /// Human label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Launch => "launch",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Steer => "steer",
            Self::Permission => "permission",
            Self::Resume => "resume",
            Self::Kill => "kill",
        }
    }
}

/// Screens a fixture supplies — literal text captured from the real CLI.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Screens {
    #[serde(default)]
    pub boot: Option<String>,
    #[serde(default)]
    pub working: Option<String>,
    #[serde(default)]
    pub idle: Option<String>,
    #[serde(default)]
    pub needs_you: Option<String>,
}

/// `tests/conformance/<name>.toml`: what a harness provides beyond its
/// manifest. Required for `state.source = "scrape"`, optional otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    #[serde(default)]
    pub screens: Screens,
}

impl Fixture {
    /// Parse a fixture file's text.
    ///
    /// # Errors
    /// A TOML error or an unknown field.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        toml::from_str(text).map_err(|e| anyhow::anyhow!("{}", e.message()))
    }

    /// `<dir>/<name>.toml`, when it exists.
    ///
    /// # Errors
    /// When the file exists but does not parse.
    pub fn load(dir: &Path, name: &str) -> anyhow::Result<Option<Self>> {
        let path = dir.join(format!("{name}.toml"));
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        Self::parse(&text).map(Some).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
    }
}

/// How the fake reports state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Mechanism {
    /// Claude Code's hook table (empty `event_map`): `PermissionRequest`
    /// long-polls for the decision.
    ClaudeHooks,
    /// Posts the events a manifest's `event_map` names.
    Mapped {
        working: String,
        idle: String,
        #[serde(default)]
        needs_you: Option<String>,
        #[serde(default)]
        ended: Option<String>,
    },
    /// Posts nothing; the pane is the only signal.
    Scrape,
}

/// Everything the fake agent needs, written by the rig as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakeSpec {
    /// The manifest name (the hook body's `harness`).
    pub harness: String,
    pub mechanism: Mechanism,
    /// `http://127.0.0.1:<port>/api/flow/hooks`.
    #[serde(default)]
    pub hook_url: Option<String>,
    pub launch_argv: Vec<String>,
    pub resume_argv: Vec<String>,
    /// `resume.mode = "resume_session"`.
    pub resume_session: bool,
    /// Env var names whose template carries `{session_id}` (th code's
    /// `SMOOTH_FLOW_SESSION`) — where a preassigned id arrives besides argv.
    #[serde(default)]
    pub session_id_env: Vec<String>,
    /// `launch.session_id = "learned"`: the fake picks its own id.
    pub learned_session_id: bool,
    pub boot: String,
    pub working: String,
    pub idle: String,
    pub needs_you: String,
    /// How long a turn stays `working`.
    pub work_ms: u64,
    /// JSON-lines log of what the fake saw and did.
    pub log: PathBuf,
}

/// Does the manifest claim it can surface a permission request?
#[must_use]
pub fn claims_permission(m: &Manifest) -> bool {
    match m.state.source {
        StateSource::Scrape => !m.state.scrape.needs_you.is_empty(),
        StateSource::Hooks | StateSource::Native => {
            m.state.hooks.event_map.is_empty() || m.state.hooks.event_map.values().any(|v| *v == FlowEventName::NeedsYou)
        }
    }
}

fn first_event(m: &Manifest, want: FlowEventName) -> Option<String> {
    m.state.hooks.event_map.iter().find(|(_, v)| **v == want).map(|(k, _)| k.clone())
}

impl FakeSpec {
    /// Derive the fake's behaviour from `m` (+ `fixture`).
    ///
    /// # Errors
    /// A contract violation the rig reports under `launch`: an `event_map`
    /// with no working or no idle event, a scrape manifest without fixture
    /// screens for working / idle.
    pub fn for_manifest(m: &Manifest, fixture: Option<&Fixture>, hook_url: Option<String>, log: PathBuf) -> Result<Self, String> {
        let screens = fixture.map(|f| f.screens.clone()).unwrap_or_default();
        let mechanism = match m.state.source {
            StateSource::Scrape => Mechanism::Scrape,
            StateSource::Hooks | StateSource::Native if m.state.hooks.event_map.is_empty() => {
                if m.state.source == StateSource::Native {
                    return Err("state.source = \"native\" needs a [state.hooks.event_map] naming its turn events".to_string());
                }
                Mechanism::ClaudeHooks
            }
            StateSource::Hooks | StateSource::Native => Mechanism::Mapped {
                working: first_event(m, FlowEventName::Working)
                    .ok_or_else(|| "state.hooks.event_map maps no event to `working` — the engine could never see a turn start".to_string())?,
                idle: first_event(m, FlowEventName::Idle)
                    .ok_or_else(|| "state.hooks.event_map maps no event to `idle` — the engine could never see a turn end".to_string())?,
                needs_you: first_event(m, FlowEventName::NeedsYou),
                ended: first_event(m, FlowEventName::Ended),
            },
        };
        if mechanism == Mechanism::Scrape {
            let mut missing = Vec::new();
            if screens.working.is_none() && !m.state.scrape.working.is_empty() {
                missing.push("working");
            }
            if screens.idle.is_none() {
                missing.push("idle");
            }
            if screens.needs_you.is_none() && !m.state.scrape.needs_you.is_empty() {
                missing.push("needs_you");
            }
            if !missing.is_empty() {
                return Err(format!(
                    "state.source = \"scrape\" needs tests/conformance/{}.toml with [screens] {} captured from the real CLI",
                    m.name,
                    missing.join(", ")
                ));
            }
        }
        let session_id_env = m
            .launch
            .env
            .iter()
            .filter(|(_, v)| v.contains("{session_id}"))
            .map(|(k, _)| k.clone())
            .collect();
        let idle = screens.idle.unwrap_or_else(|| DEFAULT_IDLE.to_string());
        Ok(Self {
            harness: m.name.clone(),
            mechanism,
            hook_url,
            launch_argv: m.launch.argv.clone(),
            resume_argv: m.resume.argv.clone(),
            resume_session: m.resume.mode == ResumeMode::ResumeSession,
            session_id_env,
            learned_session_id: m.launch.session_id == SessionIdMode::Learned,
            boot: screens.boot.unwrap_or_else(|| DEFAULT_BOOT.to_string()),
            working: screens.working.unwrap_or_else(|| DEFAULT_WORKING.to_string()),
            idle,
            needs_you: screens.needs_you.unwrap_or_else(|| DEFAULT_NEEDS_YOU.to_string()),
            work_ms: 2000,
            log,
        })
    }
}

/// Check the screens the fake will paint against `m`'s own scrape rules.
///
/// Scrape manifests: the working screen must read `Working`, idle `Idle`,
/// needs_you `AwaitingApproval` — the fixture proves the regexes match what
/// the real CLI paints. Every manifest: no screen may read as a usage limit,
/// and only the needs_you screen may read as an approval (the engine scrapes
/// both for hooks harnesses too).
#[must_use]
pub fn screen_problems(m: &Manifest, spec: &FakeSpec) -> Vec<String> {
    let rules = match ScrapeRules::compile(&m.state.scrape) {
        Ok(r) => r,
        Err(e) => return vec![format!("{e:#}")],
    };
    let scrape = spec.mechanism == Mechanism::Scrape;
    let mut out = Vec::new();
    let mut check = |label: &str, text: &str, want: Option<PaneState>| {
        let got = rules.detect(text).state;
        if let Some(want) = want {
            if got != want {
                out.push(format!("the {label} screen scrapes as {got:?}, not {want:?}"));
            }
        } else if matches!(got, PaneState::UsageLimit | PaneState::AwaitingApproval) {
            out.push(format!("the {label} screen scrapes as {got:?}"));
        }
    };
    check("boot", &spec.boot, None);
    check(
        "working",
        &spec.working,
        (scrape && !m.state.scrape.working.is_empty()).then_some(PaneState::Working),
    );
    check("idle", &spec.idle, scrape.then_some(PaneState::Idle));
    if scrape && !m.state.scrape.needs_you.is_empty() {
        check("needs_you", &spec.needs_you, Some(PaneState::AwaitingApproval));
    }
    out
}

/// What [`match_argv`] read out of an argv.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Matched {
    /// Placeholder name (without braces) → value.
    pub vars: BTreeMap<String, String>,
}

impl Matched {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }
}

fn has_placeholder(el: &str) -> bool {
    PLACEHOLDERS.iter().any(|p| el.contains(p))
}

/// Match one template element that carries a placeholder against `tok`:
/// `--flag={x}` style prefixes/suffixes must match, the rest binds.
fn bind_element(el: &str, tok: &str, vars: &mut BTreeMap<String, String>) -> bool {
    let Some(ph) = PLACEHOLDERS.iter().find(|p| el.contains(*p)) else {
        return el == tok;
    };
    let (prefix, suffix) = el.split_once(ph).unwrap_or((el, ""));
    if has_placeholder(suffix) || !tok.starts_with(prefix) || !tok.ends_with(suffix) || tok.len() < prefix.len() + suffix.len() {
        return false;
    }
    let value = &tok[prefix.len()..tok.len() - suffix.len()];
    if value.is_empty() {
        return false;
    }
    let key = ph.trim_matches(|c| c == '{' || c == '}').to_string();
    match vars.get(&key) {
        Some(prev) if prev != value => false,
        _ => {
            vars.insert(key, value.to_string());
            true
        }
    }
}

fn match_from(template: &[String], argv: &[String], vars: &mut BTreeMap<String, String>) -> bool {
    let Some((el, rest)) = template.split_first() else {
        return argv.is_empty();
    };
    if has_placeholder(el) {
        // Bound…
        if let Some(tok) = argv.first() {
            let mut v = vars.clone();
            if bind_element(el, tok, &mut v) && match_from(rest, &argv[1..], &mut v) {
                *vars = v;
                return true;
            }
        }
        // …or dropped (its value was empty).
        return match_from(rest, argv, vars);
    }
    if argv.first().is_some_and(|t| t == el) {
        let mut v = vars.clone();
        if match_from(rest, &argv[1..], &mut v) {
            *vars = v;
            return true;
        }
    }
    // A bare `-flag` literal is dropped together with the placeholder after it.
    if el.starts_with('-') && rest.first().is_some_and(|n| has_placeholder(n)) {
        return match_from(&rest[1..], argv, vars);
    }
    false
}

/// Match `argv` (after the binary) against a manifest argv template.
///
/// The inverse of [`crate::harness::render_argv`]: literals must appear in order,
/// a placeholder element binds one token or is absent, and a bare `-flag`
/// literal may be absent together with the placeholder after it. `None` when
/// the argv could not have been rendered from `template`.
#[must_use]
pub fn match_argv(template: &[String], argv: &[String]) -> Option<Matched> {
    let mut vars = BTreeMap::new();
    match_from(template, argv, &mut vars).then_some(Matched { vars })
}

/// Whether a rendered prompt is pasted (so the fake reads it from stdin).
#[must_use]
pub fn prompt_is_pasted(m: &Manifest) -> bool {
    m.launch.prompt_as == PromptAs::Paste
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use crate::harness::{render_argv, Vars, BUILTIN};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    fn builtin(name: &str) -> Manifest {
        let (_, text) = BUILTIN.iter().find(|(n, _)| *n == name).unwrap();
        Manifest::parse(text).unwrap()
    }

    const SCRAPE: &str = r#"
name = "scrapey"
[binary]
names = ["scrapey"]
[launch]
argv = ["--message={prompt}"]
session_id = "preassigned"
[resume]
mode = "relaunch_command"
[state]
source = "scrape"
[state.scrape]
working = ["esc to interrupt"]
idle = ["^> ?$"]
needs_you = ["\\(y/n\\)"]
"#;

    #[test]
    fn match_argv_inverts_render_argv_for_every_builtin_launch_and_resume() {
        let full = Vars {
            prompt: Some("do the thing"),
            session_id: Some("sid-1"),
            model: Some("m1"),
            ..Vars::default()
        };
        let bare = Vars {
            prompt: Some("p"),
            session_id: Some("sid-2"),
            ..Vars::default()
        };
        for (name, _) in BUILTIN {
            let m = builtin(name);
            for vars in [full, bare] {
                let argv = render_argv(&m.launch.argv, &vars);
                let got = match_argv(&m.launch.argv, &argv).unwrap_or_else(|| panic!("{name}: {argv:?}"));
                if m.launch.argv.iter().any(|a| a.contains("{prompt}")) {
                    assert_eq!(got.get("prompt"), vars.prompt, "{name}");
                }
                if m.launch.argv.iter().any(|a| a.contains("{model}")) {
                    assert_eq!(got.get("model"), vars.model, "{name}");
                }
                if m.resume.mode == ResumeMode::ResumeSession {
                    let r = render_argv(&m.resume.argv, &vars);
                    assert_eq!(match_argv(&m.resume.argv, &r).unwrap().get("session_id"), vars.session_id, "{name}");
                }
            }
        }
    }

    #[test]
    fn match_argv_rejects_what_the_template_could_not_render() {
        let t = s(&["--session-id", "{session_id}", "--model", "{model}", "{prompt}"]);
        assert!(match_argv(&t, &s(&["--session-id", "x", "p", "extra"])).is_none());
        assert!(match_argv(&t, &s(&["--resume", "x"])).is_none(), "an unknown flag is not a prompt + leftovers");
        let codex_resume = s(&["resume", "{session_id}"]);
        assert!(match_argv(&codex_resume, &s(&["hello"])).is_none(), "a literal subcommand must appear");
        assert_eq!(match_argv(&codex_resume, &s(&["resume", "abc"])).unwrap().get("session_id"), Some("abc"));
        let eq = s(&["--message={prompt}"]);
        assert_eq!(match_argv(&eq, &s(&["--message=hi there"])).unwrap().get("prompt"), Some("hi there"));
        assert!(match_argv(&eq, &s(&["--msg=hi"])).is_none());
        assert!(match_argv(&eq, &[]).is_some(), "an element whose value is empty is dropped");
        assert!(match_argv(&[], &s(&["x"])).is_none());
        // The same placeholder twice must bind the same value.
        let twice = s(&["{session_id}", "--again", "{session_id}"]);
        assert!(match_argv(&twice, &s(&["a", "--again", "b"])).is_none());
        assert!(match_argv(&twice, &s(&["a", "--again", "a"])).is_some());
    }

    #[test]
    fn spec_for_the_builtins_speaks_each_mechanism() {
        let claude = FakeSpec::for_manifest(&builtin("claude"), None, None, PathBuf::from("/l")).unwrap();
        assert_eq!(claude.mechanism, Mechanism::ClaudeHooks);
        assert!(!claude.learned_session_id && claude.resume_session);
        let codex = FakeSpec::for_manifest(&builtin("codex"), None, None, PathBuf::from("/l")).unwrap();
        assert!(codex.learned_session_id);
        let th = FakeSpec::for_manifest(&builtin("th-code"), None, None, PathBuf::from("/l")).unwrap();
        assert_eq!(
            th.mechanism,
            Mechanism::Mapped {
                working: "turn_start".into(),
                idle: "turn_end".into(),
                needs_you: None,
                ended: None
            }
        );
        assert_eq!(th.session_id_env, vec!["SMOOTH_FLOW_SESSION".to_string()]);
        assert!(!th.resume_session);
        for (name, _) in BUILTIN {
            let m = builtin(name);
            let spec = FakeSpec::for_manifest(&m, None, None, PathBuf::from("/l")).unwrap();
            assert!(screen_problems(&m, &spec).is_empty(), "{name}: {:?}", screen_problems(&m, &spec));
        }
    }

    #[test]
    fn claims_permission_follows_the_manifest() {
        assert!(claims_permission(&builtin("claude")), "the Claude table has PermissionRequest");
        assert!(!claims_permission(&builtin("th-code")), "th code reports no asks");
        assert!(claims_permission(&Manifest::parse(SCRAPE).unwrap()));
        let no_ask = SCRAPE.replace("needs_you = [\"\\\\(y/n\\\\)\"]", "");
        assert!(!claims_permission(&Manifest::parse(&no_ask).unwrap()));
    }

    #[test]
    fn a_scrape_manifest_needs_captured_screens_that_its_regexes_match() {
        let m = Manifest::parse(SCRAPE).unwrap();
        let err = FakeSpec::for_manifest(&m, None, None, PathBuf::from("/l")).unwrap_err();
        assert!(
            err.contains("tests/conformance/scrapey.toml") && err.contains("working, idle, needs_you"),
            "{err}"
        );

        let good = Fixture::parse("[screens]\nworking = \"Thinking (esc to interrupt)\"\nidle = \"> \"\nneeds_you = \"Run it? (y/n)\"\n").unwrap();
        let spec = FakeSpec::for_manifest(&m, Some(&good), None, PathBuf::from("/l")).unwrap();
        assert_eq!(spec.mechanism, Mechanism::Scrape);
        assert!(screen_problems(&m, &spec).is_empty(), "{:?}", screen_problems(&m, &spec));

        let bad = Fixture::parse("[screens]\nworking = \"Thinking…\"\nidle = \"$ \"\nneeds_you = \"Run it?\"\n").unwrap();
        let spec = FakeSpec::for_manifest(&m, Some(&bad), None, PathBuf::from("/l")).unwrap();
        let problems = screen_problems(&m, &spec);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("working screen scrapes as Unknown"), "{problems:?}");

        assert!(Fixture::parse("[screens]\nbogus = \"x\"\n").is_err(), "unknown fields are errors");
    }

    #[test]
    fn an_event_map_without_turn_events_is_refused() {
        let m = builtin("th-code");
        let mut only_start = m.clone();
        only_start.state.hooks.event_map.retain(|_, v| *v == FlowEventName::Working);
        let err = FakeSpec::for_manifest(&only_start, None, None, PathBuf::from("/l")).unwrap_err();
        assert!(err.contains("`idle`"), "{err}");
        let mut empty_native = m;
        empty_native.state.hooks.event_map.clear();
        assert!(FakeSpec::for_manifest(&empty_native, None, None, PathBuf::from("/l"))
            .unwrap_err()
            .contains("native"));
    }

    #[test]
    fn fixture_load_reads_the_named_file_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Fixture::load(dir.path(), "none").unwrap().is_none());
        std::fs::write(dir.path().join("x.toml"), "[screens]\nidle = \"> \"\n").unwrap();
        assert_eq!(Fixture::load(dir.path(), "x").unwrap().unwrap().screens.idle.as_deref(), Some("> "));
        std::fs::write(dir.path().join("y.toml"), "nope = 1").unwrap();
        assert!(Fixture::load(dir.path(), "y").unwrap_err().to_string().contains("y.toml"));
    }

    #[test]
    fn spec_round_trips_as_json() {
        let spec = FakeSpec::for_manifest(&builtin("opencode"), None, Some("http://h/api/flow/hooks".into()), PathBuf::from("/l")).unwrap();
        let back: FakeSpec = serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
        assert_eq!(back, spec);
        assert!(prompt_is_pasted(&builtin("th-code")) && !prompt_is_pasted(&builtin("claude")));
        assert_eq!(Step::ALL.first().map(|s| s.label()), Some("resolve"));
    }
}
