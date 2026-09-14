//! Golden pane fixtures for the scrape detector (th-e77603).
//!
//! Every `tests/fixtures/scrape/<harness>/<case>.pane` is a real capture of a
//! real CLI (`tmux capture-pane -p`, a few with `-e`) taken with a fresh
//! scratch `$HOME` and a local mock OpenAI-compatible server, plus the
//! terminal state the engine reads alongside it. The header says what the
//! built-in manifest must conclude and which rule must decide it:
//!
//! ```text
//! # harness: aider
//! # expect: working            (working | idle | needs_you | usage_limit | error | unknown | none)
//! # rule: aider-waiting        (optional)
//! # title: marvin
//! # alternate_on: false
//! # cursor_y: 19
//! # quiet_ms: 0                (optional — absent means "first look, unknown")
//! # source: …
//! # note: …
//! ---
//! <the pane>
//! ```
//!
//! `expect: none` marks a sign-in wall of a CLI with no built-in manifest: no
//! built-in may read it as idle or working.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use smooth_flow::harness::{Registry, ScrapeRules, ScrapeSpec, StateSource};
use smooth_flow::scrape::PaneObservation;
use smooth_tmux::detect::{detect_state, PaneState};

struct Fixture {
    path: PathBuf,
    harness: String,
    expect: Option<PaneState>,
    rule: Option<String>,
    title: String,
    alternate_on: bool,
    cursor_y: Option<usize>,
    quiet: Option<Duration>,
    text: String,
}

impl Fixture {
    fn obs(&self) -> PaneObservation<'_> {
        PaneObservation {
            text: &self.text,
            title: Some(&self.title),
            alternate_on: Some(self.alternate_on),
            cursor_y: self.cursor_y,
            quiet_for: self.quiet,
        }
    }
}

fn parse_state(s: &str) -> Option<PaneState> {
    Some(match s {
        "working" => PaneState::Working,
        "idle" => PaneState::Idle,
        "needs_you" => PaneState::AwaitingApproval,
        "usage_limit" => PaneState::UsageLimit,
        "error" => PaneState::Errored,
        "unknown" => PaneState::Unknown,
        "none" => return None,
        other => panic!("unknown expect `{other}`"),
    })
}

fn load(path: &Path) -> Fixture {
    let raw = std::fs::read_to_string(path).unwrap();
    let (header, text) = raw.split_once("\n---\n").unwrap_or_else(|| panic!("{}: no `---` separator", path.display()));
    let mut h: BTreeMap<&str, &str> = BTreeMap::new();
    for line in header.lines() {
        let line = line.strip_prefix("# ").unwrap_or_else(|| panic!("{}: header line `{line}`", path.display()));
        let (k, v) = line.split_once(": ").unwrap_or((line.trim_end_matches(':'), ""));
        h.insert(k, v);
    }
    let expect_word = h["expect"].split_whitespace().next().unwrap();
    Fixture {
        path: path.to_path_buf(),
        harness: h["harness"].to_string(),
        expect: parse_state(expect_word),
        rule: h.get("rule").map(ToString::to_string),
        title: h.get("title").copied().unwrap_or_default().to_string(),
        alternate_on: h.get("alternate_on") == Some(&"true"),
        cursor_y: h.get("cursor_y").and_then(|c| c.parse().ok()),
        quiet: h.get("quiet_ms").map(|q| Duration::from_millis(q.parse().unwrap())),
        text: text.to_string(),
    }
}

fn fixtures() -> Vec<Fixture> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scrape");
    let mut out = Vec::new();
    for dir in std::fs::read_dir(&root).unwrap() {
        let dir = dir.unwrap().path();
        for f in std::fs::read_dir(&dir).unwrap() {
            let f = f.unwrap().path();
            if f.extension().is_some_and(|e| e == "pane") {
                out.push(load(&f));
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    assert!(out.len() >= 40, "fixtures went missing: {}", out.len());
    out
}

fn rules_for(harness: &str) -> ScrapeRules {
    let r = Registry::builtin();
    let m = r.get(harness).unwrap_or_else(|| panic!("no built-in manifest `{harness}`"));
    ScrapeRules::compile(&m.state.scrape).unwrap()
}

/// The core golden test: each built-in manifest reads each real capture as
/// the header says, decided by the named rule.
#[test]
fn every_fixture_reads_as_its_header_says() {
    let mut failures = Vec::new();
    for fx in fixtures().iter().filter(|f| f.expect.is_some()) {
        let got = rules_for(&fx.harness).detect_observation(&fx.obs());
        let want = fx.expect.unwrap();
        if got.state != want || (fx.rule.is_some() && got.rule != fx.rule) {
            failures.push(format!(
                "{}: want {want:?} by {:?}, got {:?} by {:?}",
                fx.path.display(),
                fx.rule,
                got.state,
                got.rule
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Every scraped built-in has a fixture for each of working / idle / needs_you.
#[test]
fn every_scraped_builtin_has_working_idle_and_needs_you_fixtures() {
    let all = fixtures();
    let r = Registry::builtin();
    for m in r.all().iter().filter(|m| m.state.source == StateSource::Scrape) {
        for state in [PaneState::Working, PaneState::Idle, PaneState::AwaitingApproval] {
            assert!(
                all.iter().any(|f| f.harness == m.name && f.expect == Some(state)),
                "{}: no {state:?} fixture",
                m.name
            );
        }
    }
}

/// A sign-in wall (auggie, kiro) must never read as a ready or busy agent
/// under ANY built-in's rules — a fresh install is not a working session.
#[test]
fn sign_in_walls_are_never_idle_or_working() {
    let r = Registry::builtin();
    for fx in fixtures().iter().filter(|f| f.expect.is_none()) {
        for m in r.all() {
            let got = ScrapeRules::compile(&m.state.scrape).unwrap().detect_observation(&fx.obs());
            assert!(
                !matches!(got.state, PaneState::Idle | PaneState::Working),
                "{} under `{}`: {:?} by {:?}",
                fx.path.display(),
                m.name,
                got.state,
                got.rule
            );
        }
    }
}

/// Property-style checks over every real capture: things that must not
/// change a verdict.
#[test]
fn verdicts_survive_resize_padding_and_longer_quiet() {
    for fx in fixtures().iter().filter(|f| f.expect.is_some()) {
        let rules = rules_for(&fx.harness);
        let base = rules.detect_observation(&fx.obs());

        // A taller pane: blank rows appended at the bottom (cursor row unchanged).
        let taller = format!("{}\n\n\n\n\n", fx.text);
        let got = rules.detect_observation(&PaneObservation { text: &taller, ..fx.obs() });
        assert_eq!(got.state, base.state, "{}: taller pane", fx.path.display());

        // Trailing spaces on every line (tmux pads cells with -N / -e captures).
        let padded: String = fx.text.lines().map(|l| format!("{l}    \n")).collect();
        let got = rules.detect_observation(&PaneObservation { text: &padded, ..fx.obs() });
        assert_eq!(got.state, base.state, "{}: space-padded lines", fx.path.display());

        // Colour: wrapping every non-blank line in SGR codes changes nothing.
        let coloured: String = fx
            .text
            .lines()
            .map(|l| if l.trim().is_empty() { format!("{l}\n") } else { format!("\u{1b}[38;5;15m{l}\u{1b}[0m\n") })
            .collect();
        let got = rules.detect_observation(&PaneObservation { text: &coloured, ..fx.obs() });
        assert_eq!(got.state, base.state, "{}: SGR-coloured", fx.path.display());

        // An idle verdict that needed quiet holds for any LONGER quiet.
        if base.state == PaneState::Idle {
            let got = rules.detect_observation(&PaneObservation {
                quiet_for: Some(Duration::from_secs(3600)),
                ..fx.obs()
            });
            assert_eq!(got.state, PaneState::Idle, "{}: idle must hold as quiet grows", fx.path.display());
        }
        // A needs-you verdict never depends on time.
        if base.state == PaneState::AwaitingApproval {
            for q in [None, Some(Duration::ZERO), Some(Duration::from_secs(3600))] {
                let got = rules.detect_observation(&PaneObservation { quiet_for: q, ..fx.obs() });
                assert_eq!(got.state, PaneState::AwaitingApproval, "{}: needs_you at quiet {q:?}", fx.path.display());
            }
        }
    }
}

/// The idle composer is NOT believed while the pane is still changing: every
/// idle fixture with the clock reset to "just changed" reads as not-idle.
#[test]
fn idle_needs_the_pane_to_hold_still_for_scrolling_clis() {
    for fx in fixtures().iter().filter(|f| f.expect == Some(PaneState::Idle) && f.harness == "aider") {
        let got = rules_for("aider").detect_observation(&PaneObservation {
            quiet_for: Some(Duration::ZERO),
            ..fx.obs()
        });
        assert_ne!(got.state, PaneState::Idle, "{}: idle on a pane that just changed", fx.path.display());
    }
}

// ── Claude Code's shared heuristics, expressed as rules ─────────────────────

/// `smooth_tmux::detect::detect_state` written as `[[state.scrape.rules]]`.
const DETECT_RS_AS_RULES: &str = r#"
[[rules]]
state = "working"
match = ["esc to interrupt", "esc to cancel", "\\(running", "tokens · esc", "esc interrupt"]
where = "tail"
lines = 12

[[rules]]
state = "usage_limit"
match = ["usage limit reached", "approaching usage limit", "limit will reset", "limit resets at", "out of credits"]
where = "pane"

[[rules]]
state = "needs_you"
match = ["do you want to proceed", "do you want to make this edit", "❯ 1\\. yes", "1\\. yes", "would you like to proceed", "press enter to confirm"]
where = "pane"

[[rules]]
state = "error"
match = ["api error", "fatal error", "request failed", "execution error"]
where = "pane"

[[rules]]
state = "idle"
match = ["\\? for shortcuts", "for shortcuts", "shift\\+tab to cycle", "> ", "ctrl\\+p"]
where = "tail"
lines = 12
"#;

fn detect_rs_rules() -> ScrapeRules {
    ScrapeRules::compile(&toml::from_str::<ScrapeSpec>(DETECT_RS_AS_RULES).unwrap()).unwrap()
}

/// The claim in the design: the shared Claude Code heuristics need nothing
/// the rule language lacks. Checked on detect.rs's own panes and on
/// thousands of generated ones.
#[test]
fn claude_detect_rs_is_expressible_as_rules() {
    let rules = detect_rs_rules();
    let mut panes: Vec<String> = [
        "You've reached your usage limit. limit will reset at 4pm.",
        "● API Error: Server is temporarily limiting requests (not your usage limit) · Rate limited",
        "Edit file foo.rs?\n  Do you want to proceed?\n  ❯ 1. Yes\n  2. No",
        "● Thinking…\n  (esc to interrupt · 1.2k tokens)",
        "● API Error: something went wrong\n● Thinking…\n  (esc to interrupt · 200 tokens)",
        "╭─────────╮\n│ >       │\n╰─────────╯\n  ? for shortcuts",
        "just some neutral build output here",
        "USAGE LIMIT REACHED",
        "  ┃  Build · GPT-5.6 Sol OpenAI · high\n  ╹▀▀▀▀\n   ⬝⬝⬝⬝■■■■  esc interrupt        tab agents  ctrl+p commands",
        "  Hooks need review\n› 1. Review hooks\n  2. Trust all and continue\n  Press enter to confirm or esc to go back",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    const FRAGMENTS: &[&str] = &[
        "",
        "output line",
        "● Thinking… (esc to interrupt)",
        "(running tool)",
        "Do you want to proceed?",
        "❯ 1. Yes",
        "● API Error: boom",
        "limit will reset at 4pm",
        "? for shortcuts",
        "⏵⏵ auto mode on (shift+tab to cycle)",
        "❯ ",
        "> ",
        "Press enter to confirm",
        "ctrl+p commands",
        "Quick safety check · Esc to cancel",
        "   ",
    ];
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..5_000 {
        let n = usize::try_from(next() % 30).unwrap();
        panes.push((0..n).map(|_| FRAGMENTS[usize::try_from(next()).unwrap() % FRAGMENTS.len()]).collect::<Vec<_>>().join("\n"));
    }
    for p in &panes {
        assert_eq!(rules.detect(p).state, detect_state(p), "{p:?}");
    }
    // …and the real captured panes from every harness, too.
    for fx in fixtures() {
        assert_eq!(rules.detect(&fx.text).state, detect_state(&fx.text), "{}", fx.path.display());
    }
}
