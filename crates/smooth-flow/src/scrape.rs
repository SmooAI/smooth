//! The declarative scrape detector (th-e77603): `[[state.scrape.rules]]`.
//!
//! Harnesses with no lifecycle hooks (aider, goose, crush, cline, …) are known
//! only through their terminal. The flat `working` / `idle` / `needs_you` /
//! `usage_limit` / `error` lists in `[state.scrape]` cover a TUI whose markers
//! are unambiguous; a real hookless CLI needs more than "a regex somewhere":
//!
//! - **order** — crush paints its "Permission Required" modal over a footer
//!   that still reads `esc cancel`, and cline keeps `⠴ Thinking… (esc to
//!   cancel)` above its approval box, so for them `needs_you` must beat
//!   `working`. The flat lists hard-code the opposite.
//! - **where** — aider's idle composer is a bare `>` on the LAST line; the same
//!   `> explain the repo` a moment later, with the model streaming below it,
//!   is not idle. A regex over the tail cannot tell those apart; `last_line` /
//!   `cursor_line` can.
//! - **time** — aider streams prose with no marker at all. The only honest
//!   signal is that the pane text keeps changing (`changed_within_ms`), and
//!   the only honest idle is a prompt that has held still (`quiet_ms`).
//! - **terminal state** — the OSC 0/2 title (`title`), the alternate screen,
//!   and spinner glyphs (braille / quarter-circle frames), the same evidence
//!   orca's `agent-title-status.ts` and cmux's title churn filter read.
//!
//! Rules are evaluated in order; the FIRST rule whose every condition holds
//! decides. Anything no rule decides falls through to the flat lists, with
//! their original precedence, so every existing manifest reads exactly as it
//! did. Evaluation is pure over a [`PaneObservation`] — the engine gathers
//! one per supervision tick; fixtures build one by hand.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use smooth_tmux::detect::PaneState;

/// Default live window for `where = "tail"` (same as `smooth_tmux::detect`).
pub const DEFAULT_TAIL_LINES: usize = 12;

/// Largest `lines` / `tail_lines` a manifest may ask for — a visible pane is
/// never taller than this in practice, and a bound keeps a typo from turning
/// the tail into "the whole scrollback".
pub const MAX_TAIL_LINES: usize = 500;

/// What a rule concludes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Working,
    Idle,
    /// An approval / question / dialog the user must answer.
    #[serde(alias = "permission")]
    NeedsYou,
    UsageLimit,
    Error,
}

impl Verdict {
    /// The shared detector's state for this verdict.
    #[must_use]
    pub const fn pane_state(self) -> PaneState {
        match self {
            Self::Working => PaneState::Working,
            Self::Idle => PaneState::Idle,
            Self::NeedsYou => PaneState::AwaitingApproval,
            Self::UsageLimit => PaneState::UsageLimit,
            Self::Error => PaneState::Errored,
        }
    }
}

/// Which part of the observation a rule's patterns run over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The last `lines` non-blank lines (default [`DEFAULT_TAIL_LINES`]).
    #[default]
    Tail,
    /// The whole visible pane.
    Pane,
    /// The last non-blank line.
    LastLine,
    /// The line the cursor is on (`#{cursor_y}`); the last non-blank line
    /// when the cursor position is unknown.
    CursorLine,
    /// The pane title (OSC 0/2, `#{pane_title}`). tmux reports the host name
    /// when the program never set one.
    Title,
}

/// One `[[state.scrape.rules]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    /// Shown in diagnostics (`th flow snapshot`, validator reports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub state: Verdict,
    /// Any-of, case-insensitive and multi-line (`^` / `$` anchor each line of
    /// the window). Empty ⇒ no text condition (a signal decides).
    /// A `usage_limit` pattern may carry `(?P<reset>…)`.
    #[serde(default, rename = "match", skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<String>,
    #[serde(default, rename = "where")]
    pub scope: Scope,
    /// Window size for `where = "tail"`; overrides `tail_lines`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<usize>,
    /// Every one of these must ALSO hit the same window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all: Vec<String>,
    /// The rule is void if any of these hits the same window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unless: Vec<String>,
    /// The rule is void if one of these hits a line BELOW the last line
    /// `match` hit — the CLI has moved past it (an answered question with the
    /// prompt printed under it).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unless_below: Vec<String>,
    /// The pane text has not changed for at least this long.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_ms: Option<u64>,
    /// The pane text changed less than this long ago.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_within_ms: Option<u64>,
    /// Require the alternate screen on (`true`) or off (`false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_screen: Option<bool>,
    /// Require (`true`) or forbid (`false`) a spinner frame in the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spinner: Option<bool>,
}

/// Everything the detector may look at for one pane, at one instant.
#[derive(Debug, Clone, Copy, Default)]
pub struct PaneObservation<'a> {
    /// The visible pane, plain text (`capture-pane -p`).
    pub text: &'a str,
    /// `#{pane_title}`.
    pub title: Option<&'a str>,
    /// `#{alternate_on}`.
    pub alternate_on: Option<bool>,
    /// `#{cursor_y}` — 0-based row of the visible pane.
    pub cursor_y: Option<usize>,
    /// How long the text has been unchanged. `None` = unknown (first look):
    /// a time condition never holds on unknown time.
    pub quiet_for: Option<Duration>,
}

impl<'a> PaneObservation<'a> {
    /// Text only — no terminal state, no time. What `ScrapeRules::detect`
    /// has always seen.
    #[must_use]
    pub const fn text(text: &'a str) -> Self {
        Self {
            text,
            title: None,
            alternate_on: None,
            cursor_y: None,
            quiet_for: None,
        }
    }
}

/// A compiled rule.
#[derive(Debug, Clone)]
pub struct Rule {
    pub name: Option<String>,
    pub state: Verdict,
    patterns: Vec<Regex>,
    scope: Scope,
    lines: usize,
    all: Vec<Regex>,
    unless: Vec<Regex>,
    unless_below: Vec<Regex>,
    quiet: Option<Duration>,
    changed_within: Option<Duration>,
    alternate_screen: Option<bool>,
    spinner: Option<bool>,
}

/// A rule that fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub state: Verdict,
    /// `rules[i].name`, or `rules[i]` when unnamed.
    pub rule: String,
    /// A `usage_limit` rule's `reset` capture.
    pub reset_text: Option<String>,
}

fn compile(field: &str, pats: &[String]) -> Result<Vec<Regex>> {
    pats.iter()
        .map(|p| Regex::new(&format!("(?im){p}")).with_context(|| format!("{field}: bad regex `{p}`")))
        .collect()
}

impl Rule {
    /// Compile `spec` (index `i` names it in errors).
    ///
    /// # Errors
    /// A bad regex, a rule with no condition at all, a window option on a
    /// scope it does not apply to, or an out-of-range `lines`.
    pub fn compile(i: usize, spec: &RuleSpec, tail_lines: usize) -> Result<Self> {
        let field = format!("state.scrape.rules[{i}]");
        let has_signal = spec.quiet_ms.is_some() || spec.changed_within_ms.is_some() || spec.alternate_screen.is_some() || spec.spinner.is_some();
        if spec.patterns.is_empty() && spec.all.is_empty() && !has_signal {
            bail!("{field}: needs `match`, `all` or a signal (quiet_ms, changed_within_ms, alternate_screen, spinner)");
        }
        if let (Some(q), Some(c)) = (spec.quiet_ms, spec.changed_within_ms) {
            if c <= q {
                bail!("{field}: quiet_ms = {q} and changed_within_ms = {c} can never both hold");
            }
        }
        if spec.lines.is_some() && spec.scope != Scope::Tail {
            bail!("{field}.lines: only applies to where = \"tail\"");
        }
        if !spec.unless_below.is_empty() && spec.scope == Scope::Title {
            bail!("{field}.unless_below: a title has no lines below it");
        }
        let lines = spec.lines.unwrap_or(tail_lines);
        if lines == 0 || lines > MAX_TAIL_LINES {
            bail!("{field}.lines: must be 1..={MAX_TAIL_LINES}, got {lines}");
        }
        Ok(Self {
            name: spec.name.clone(),
            state: spec.state,
            patterns: compile(&format!("{field}.match"), &spec.patterns)?,
            scope: spec.scope,
            lines,
            all: compile(&format!("{field}.all"), &spec.all)?,
            unless: compile(&format!("{field}.unless"), &spec.unless)?,
            unless_below: compile(&format!("{field}.unless_below"), &spec.unless_below)?,
            quiet: spec.quiet_ms.map(Duration::from_millis),
            changed_within: spec.changed_within_ms.map(Duration::from_millis),
            alternate_screen: spec.alternate_screen,
            spinner: spec.spinner,
        })
    }

    /// Does this rule fire on `obs`? Returns the `reset` capture (if any) on a hit.
    #[must_use]
    pub fn eval(&self, obs: &PaneObservation<'_>) -> Option<Option<String>> {
        // Cheap signals first.
        if let Some(want) = self.alternate_screen {
            if obs.alternate_on != Some(want) {
                return None;
            }
        }
        if let Some(q) = self.quiet {
            if !obs.quiet_for.is_some_and(|d| d >= q) {
                return None;
            }
        }
        if let Some(c) = self.changed_within {
            if !obs.quiet_for.is_some_and(|d| d < c) {
                return None;
            }
        }
        let window = window(obs, self.scope, self.lines);
        let joined = window.join("\n");
        if let Some(want) = self.spinner {
            if has_spinner(&joined) != want {
                return None;
            }
        }
        if !self.patterns.is_empty() && !self.patterns.iter().any(|r| r.is_match(&joined)) {
            return None;
        }
        if !self.all.iter().all(|r| r.is_match(&joined)) {
            return None;
        }
        if self.unless.iter().any(|r| r.is_match(&joined)) {
            return None;
        }
        if !self.unless_below.is_empty() {
            let last = |res: &[Regex]| window.iter().rposition(|l| !l.trim().is_empty() && res.iter().any(|r| r.is_match(l)));
            let below = last(&self.unless_below);
            let moved_past = match (self.patterns.is_empty(), last(&self.patterns), below) {
                // Only a multi-line match: no line to be below — it stands.
                (false, None, _) | (_, _, None) => false,
                (false, Some(hit), Some(b)) => b > hit,
                // No `match`: any unless_below line voids it.
                (true, _, Some(_)) => true,
            };
            if moved_past {
                return None;
            }
        }
        let reset = self
            .patterns
            .iter()
            .find_map(|r| r.captures(&joined).and_then(|c| c.name("reset")).map(|m| m.as_str().to_string()));
        Some(reset)
    }
}

/// Evaluate `rules` in order; the first that fires decides.
#[must_use]
pub fn first_hit(rules: &[Rule], obs: &PaneObservation<'_>) -> Option<Hit> {
    rules.iter().enumerate().find_map(|(i, r)| {
        r.eval(obs).map(|reset_text| Hit {
            state: r.state,
            rule: r.name.clone().unwrap_or_else(|| format!("rules[{i}]")),
            reset_text,
        })
    })
}

/// The lines a scope looks at.
fn window<'a>(obs: &PaneObservation<'a>, scope: Scope, lines: usize) -> Vec<&'a str> {
    let text = obs.text;
    match scope {
        Scope::Title => vec![obs.title.unwrap_or("")],
        Scope::Pane => text.lines().collect(),
        Scope::Tail => {
            let non_blank: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            let start = non_blank.len().saturating_sub(lines);
            non_blank[start..].to_vec()
        }
        Scope::LastLine => text.lines().rev().find(|l| !l.trim().is_empty()).into_iter().collect(),
        Scope::CursorLine => match obs.cursor_y {
            Some(y) => vec![text.lines().nth(y).unwrap_or("")],
            None => window(obs, Scope::LastLine, lines),
        },
    }
}

/// `text` with terminal escape sequences removed: CSI (`ESC [ … final`), OSC
/// (`ESC ] … BEL` / `ESC ] … ESC \\`), other two-byte `ESC x` sequences, and
/// carriage returns. `capture-pane -p` is already plain; a `-e` capture or a
/// raw PTY tail is not, and a colour code between `>` and the end of the
/// line must not make an idle composer unreadable.
#[must_use]
pub fn strip_ansi(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(['\u{1b}', '\r']) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {}
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    // Parameters / intermediates, then one final byte in @..~.
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // `ESC x`: a two-byte sequence (or a lone trailing ESC).
                _ => {}
            },
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// A spinner frame: braille patterns (U+2801–U+28FF; U+2800 is a blank
/// cell, not a frame), quarter / half circles ◐◑◒◓ and ◴◵◶◷. The same glyph
/// families orca's `containsAgentSpinnerGlyph` and cmux's
/// `TerminalTitleChurnFilter` treat as animation, never as identity.
#[must_use]
pub fn has_spinner(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x2801..=0x28FF | 0x25D0..=0x25D3 | 0x25F4..=0x25F7))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn rule(toml_src: &str) -> Rule {
        let spec: RuleSpec = toml::from_str(toml_src).unwrap();
        Rule::compile(0, &spec, DEFAULT_TAIL_LINES).unwrap()
    }

    fn compile_err(toml_src: &str) -> String {
        let spec: RuleSpec = toml::from_str(toml_src).unwrap();
        format!("{:#}", Rule::compile(0, &spec, DEFAULT_TAIL_LINES).unwrap_err())
    }

    #[test]
    fn verdicts_map_to_pane_states_and_permission_is_an_alias() {
        let r: RuleSpec = toml::from_str("state = \"permission\"\nmatch = [\"x\"]").unwrap();
        assert_eq!(r.state, Verdict::NeedsYou);
        assert_eq!(Verdict::NeedsYou.pane_state(), PaneState::AwaitingApproval);
        assert_eq!(Verdict::UsageLimit.pane_state(), PaneState::UsageLimit);
        assert_eq!(Verdict::Error.pane_state(), PaneState::Errored);
        assert!(toml::from_str::<RuleSpec>("state = \"busy\"\nmatch = [\"x\"]").is_err(), "unknown verdict");
        assert!(toml::from_str::<RuleSpec>("state = \"idle\"\nregex = [\"x\"]").is_err(), "unknown field");
    }

    #[test]
    fn compile_errors_name_the_field() {
        assert!(compile_err("state = \"idle\"").contains("needs `match`"));
        assert!(compile_err("state = \"idle\"\nmatch = [\"(\"]").contains("rules[0].match: bad regex"));
        assert!(compile_err("state = \"idle\"\nmatch = [\"x\"]\nunless = [\"[\"]").contains("rules[0].unless: bad regex"));
        assert!(compile_err("state = \"idle\"\nmatch = [\"x\"]\nwhere = \"pane\"\nlines = 3").contains(".lines: only applies"));
        assert!(compile_err("state = \"idle\"\nmatch = [\"x\"]\nlines = 0").contains("must be 1..="));
        assert!(compile_err("state = \"idle\"\nmatch = [\"x\"]\nwhere = \"title\"\nunless_below = [\"y\"]").contains("no lines below"));
        assert!(compile_err("state = \"idle\"\nquiet_ms = 3000\nchanged_within_ms = 2000").contains("can never both hold"));
        // A pure signal rule is fine.
        let _ = rule("state = \"working\"\nchanged_within_ms = 5000");
    }

    #[test]
    fn scopes_pick_the_right_lines() {
        let text = "header\n> explain the repo\nSure. Streaming…\n\n\n";
        let obs = PaneObservation::text(text);
        assert!(rule("state = \"idle\"\nmatch = [\"^>\"]\nwhere = \"tail\"").eval(&obs).is_some());
        assert!(rule("state = \"idle\"\nmatch = [\"^>\"]\nwhere = \"last_line\"").eval(&obs).is_none(), "the prompt is not the last line");
        let cursor_on_prompt = PaneObservation { cursor_y: Some(1), ..obs };
        assert!(rule("state = \"idle\"\nmatch = [\"^>\"]\nwhere = \"cursor_line\"").eval(&cursor_on_prompt).is_some());
        // Unknown cursor ⇒ last non-blank line.
        assert!(rule("state = \"idle\"\nmatch = [\"streaming\"]\nwhere = \"cursor_line\"").eval(&obs).is_some());
        // A cursor past the text (blank rows) sees an empty line.
        let past = PaneObservation { cursor_y: Some(30), ..obs };
        assert!(rule("state = \"idle\"\nmatch = [\"^$\"]\nwhere = \"cursor_line\"").eval(&past).is_some());
        // tail with lines = 1 is the last non-blank line.
        assert!(rule("state = \"idle\"\nmatch = [\"header\"]\nlines = 1").eval(&obs).is_none());
        assert!(rule("state = \"idle\"\nmatch = [\"header\"]\nwhere = \"pane\"").eval(&obs).is_some());
        let titled = PaneObservation { title: Some("🪿 goose"), ..obs };
        assert!(rule("state = \"idle\"\nmatch = [\"goose\"]\nwhere = \"title\"").eval(&titled).is_some());
        assert!(rule("state = \"idle\"\nmatch = [\"goose\"]\nwhere = \"title\"").eval(&obs).is_none(), "no title ⇒ no match");
    }

    #[test]
    fn all_unless_and_unless_below() {
        let r = rule("state = \"needs_you\"\nmatch = ['\\(y\\)es/\\(n\\)o']\nwhere = \"pane\"\nunless_below = ['^>\\s*$']");
        let answered = "Add .aider* to .gitignore (recommended)? (Y)es/(N)o [Yes]: y\nAdded .aider* to .gitignore\n>";
        assert!(r.eval(&PaneObservation::text(answered)).is_none(), "prompt printed below the question");
        let pending = ">\nCreate new file? (Y)es/(N)o [Yes]:";
        assert!(r.eval(&PaneObservation::text(pending)).is_some(), "the prompt is ABOVE the question");
        let both = rule("state = \"idle\"\nmatch = [\"ready\"]\nall = [\"model:\", \"directory:\"]");
        assert!(both.eval(&PaneObservation::text("ready\nmodel: x")).is_none());
        assert!(both.eval(&PaneObservation::text("ready\nmodel: x\ndirectory: /tmp")).is_some());
        let unless = rule("state = \"idle\"\nmatch = [\"ready\"]\nunless = [\"esc cancel\"]");
        assert!(unless.eval(&PaneObservation::text("> Ready\nesc cancel • tab")).is_none());
        // No `match`: any unless_below line voids.
        let bare = rule("state = \"working\"\nspinner = true\nunless_below = [\"^>\"]");
        assert!(bare.eval(&PaneObservation::text("⠋ busy")).is_some());
        assert!(bare.eval(&PaneObservation::text("⠋ busy\n> ")).is_none());
        // A match only across lines has no line to be below.
        let multi = rule("state = \"needs_you\"\nmatch = [\"proceed\\\\?\\\\n.*1\\\\. yes\"]\nwhere = \"pane\"\nunless_below = [\"^>\"]");
        assert!(multi.eval(&PaneObservation::text("do you want to proceed?\n❯ 1. Yes\n> ")).is_some());
    }

    #[test]
    fn time_and_terminal_signals() {
        let working = rule("state = \"working\"\nchanged_within_ms = 5000");
        let idle = rule("state = \"idle\"\nmatch = ['^>\\s*$']\nwhere = \"last_line\"\nquiet_ms = 1500");
        let at = |ms: Option<u64>| PaneObservation {
            quiet_for: ms.map(Duration::from_millis),
            ..PaneObservation::text("header\n>")
        };
        assert!(working.eval(&at(Some(0))).is_some());
        assert!(working.eval(&at(Some(4_999))).is_some());
        assert!(working.eval(&at(Some(5_000))).is_none());
        assert!(working.eval(&at(None)).is_none(), "unknown time never proves a change");
        assert!(idle.eval(&at(Some(1_499))).is_none());
        assert!(idle.eval(&at(Some(1_500))).is_some());
        assert!(idle.eval(&at(None)).is_none(), "unknown time never proves quiet");

        let alt = rule("state = \"idle\"\nmatch = [\"ready\"]\nalternate_screen = true");
        let base = PaneObservation::text("> Ready");
        assert!(alt.eval(&base).is_none(), "unknown alt-screen is not `true`");
        assert!(alt.eval(&PaneObservation { alternate_on: Some(true), ..base }).is_some());
        assert!(alt.eval(&PaneObservation { alternate_on: Some(false), ..base }).is_none());

        let spin = rule("state = \"working\"\nspinner = true\nlines = 8");
        assert!(spin.eval(&PaneObservation::text(" ⠸ Thinking... (esc to cancel)\n❯ Ask anything...")).is_some());
        assert!(spin.eval(&PaneObservation::text("◐  Gliding through branches...")).is_some());
        assert!(spin.eval(&PaneObservation::text("❯ Ask anything...\n⏵⏵ Auto-approve")).is_none());
        let no_spin = rule("state = \"idle\"\nmatch = [\"ask anything\"]\nspinner = false");
        assert!(no_spin.eval(&PaneObservation::text(" ⠸ Thinking...\n❯ Ask anything...")).is_none());
        assert!(no_spin.eval(&PaneObservation::text("❯ Ask anything...")).is_some());
    }

    #[test]
    fn strip_ansi_removes_csi_osc_and_cr() {
        assert!(matches!(strip_ansi("plain > "), std::borrow::Cow::Borrowed(_)));
        assert_eq!(strip_ansi("\u{1b}[38;5;15m> \u{1b}[0mEnter to send"), "> Enter to send");
        assert_eq!(strip_ansi("\u{1b}]0;🪿 goose\u{7}ready"), "ready");
        assert_eq!(strip_ansi("\u{1b}]2;title\u{1b}\\ready\r\n"), "ready\n");
        assert_eq!(strip_ansi("\u{1b}[?2004h\u{1b}=x\u{1b}"), "x");
        // An unterminated CSI at the end swallows the rest, never panics.
        assert_eq!(strip_ansi("ok\u{1b}[38;5"), "ok");
    }

    #[test]
    fn spinner_glyph_families() {
        for s in ["⠋", "⣿", "◐", "◓", "◴", "◷"] {
            assert!(has_spinner(s), "{s}");
        }
        // A blank braille cell, box drawing, the Claude idle star and bullets are not frames.
        for s in ["\u{2800}", "─│╭", "✳", "● ○", "⏵⏵", "▣"] {
            assert!(!has_spinner(s), "{s}");
        }
    }

    #[test]
    fn first_hit_is_ordered_and_names_the_rule() {
        let rules = vec![
            Rule::compile(
                0,
                &toml::from_str::<RuleSpec>("name = \"crush-permission\"\nstate = \"needs_you\"\nmatch = [\"permission required\"]\nwhere = \"pane\"").unwrap(),
                12,
            )
            .unwrap(),
            Rule::compile(1, &toml::from_str::<RuleSpec>("state = \"working\"\nmatch = [\"esc cancel\"]").unwrap(), 12).unwrap(),
        ];
        let modal = "│  Permission Required  │\n   > Processing...\n esc cancel • tab focus chat";
        let hit = first_hit(&rules, &PaneObservation::text(modal)).unwrap();
        assert_eq!((hit.state, hit.rule.as_str()), (Verdict::NeedsYou, "crush-permission"));
        let busy = "   > Thinking...\n esc cancel • tab focus chat";
        assert_eq!(first_hit(&rules, &PaneObservation::text(busy)).unwrap().rule, "rules[1]");
        assert!(first_hit(&rules, &PaneObservation::text("> Ready?")).is_none());
        assert!(first_hit(&[], &PaneObservation::text(busy)).is_none());
    }

    #[test]
    fn usage_limit_rule_carries_its_reset_capture() {
        let r = rule("state = \"usage_limit\"\nmatch = ['back at (?P<reset>\\d{1,2}(?::\\d{2})?\\s*[ap]m)']\nwhere = \"pane\"");
        assert_eq!(r.eval(&PaneObservation::text("quota gone, back at 4:30 pm")), Some(Some("4:30 pm".into())));
        let plain = rule("state = \"usage_limit\"\nmatch = [\"quota gone\"]\nwhere = \"pane\"");
        assert_eq!(plain.eval(&PaneObservation::text("quota gone")), Some(None));
    }

    /// Property-style: a tiny deterministic generator (no new dependency)
    /// throws thousands of panes built from real fragments at the rule
    /// evaluator and checks invariants that must hold for ANY pane.
    #[test]
    fn rule_evaluation_invariants_hold_on_generated_panes() {
        const FRAGMENTS: &[&str] = &[
            "",
            "   ",
            ">",
            "> ",
            "> explain the repo",
            "❯ Ask anything...",
            " ⠸ Thinking... (esc to cancel)",
            "◐  Gliding through branches...  (Ctrl+C to interrupt)",
            "Create new file? (Y)es/(N)o [Yes]:",
            "│  Permission Required  │",
            "esc cancel • tab focus chat",
            "> Ready?",
            "Sure. Here is a deliberately slow answer",
            "\u{1b}[31mnot stripped\u{1b}[0m",
            "────────────────────────────────",
            "Tokens: 769 sent, 103 received.",
        ];
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let idle_last = rule("state = \"idle\"\nmatch = ['^[>❯]\\s*']\nwhere = \"last_line\"");
        let idle_tail = rule("state = \"idle\"\nmatch = ['^[>❯]\\s*']\nwhere = \"tail\"");
        let idle_pane = rule("state = \"idle\"\nmatch = ['^[>❯]\\s*']\nwhere = \"pane\"");
        let guarded = rule("state = \"idle\"\nmatch = ['^[>❯]\\s*']\nwhere = \"pane\"\nunless = [\"esc cancel\"]");
        let quiet = rule("state = \"idle\"\nmatch = ['^[>❯]\\s*']\nwhere = \"pane\"\nquiet_ms = 1000");
        for _ in 0..4_000 {
            let n = usize::try_from(next() % 24).unwrap();
            let lines: Vec<&str> = (0..n).map(|_| FRAGMENTS[usize::try_from(next()).unwrap() % FRAGMENTS.len()]).collect();
            let text = lines.join("\n");
            let obs = PaneObservation::text(&text);
            let (l, t, p) = (idle_last.eval(&obs).is_some(), idle_tail.eval(&obs).is_some(), idle_pane.eval(&obs).is_some());
            // Windows nest: last_line ⊆ tail ⊆ pane.
            assert!(!l || t, "last_line hit ⇒ tail hit: {text:?}");
            assert!(!t || p, "tail hit ⇒ pane hit: {text:?}");
            // `unless` only ever removes hits.
            assert!(!guarded.eval(&obs).is_some() || p);
            // Time conditions only ever remove hits, and unknown time removes them all.
            assert!(quiet.eval(&obs).is_none());
            let long_quiet = PaneObservation {
                quiet_for: Some(Duration::from_secs(60)),
                ..obs
            };
            assert_eq!(quiet.eval(&long_quiet).is_some(), p);
            // Evaluation is a pure function of the observation.
            assert_eq!(idle_tail.eval(&obs), idle_tail.eval(&obs));
            // Blank lines never change the tail/last_line verdict.
            let padded = format!("\n\n{}\n\n   \n", text);
            let pobs = PaneObservation::text(&padded);
            assert_eq!(idle_last.eval(&pobs).is_some(), l, "{text:?}");
            assert_eq!(idle_tail.eval(&pobs).is_some(), t, "{text:?}");
        }
    }
}
