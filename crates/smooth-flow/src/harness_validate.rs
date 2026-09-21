//! Validate a drafted harness manifest by running it (pearl th-473294): a
//! fixed state machine over a [`FlowDriver`] — launch → working → idle, steer
//! → working → idle, kill+resume → back — that records what it could PROVE
//! and what it could not, never "passed" by assumption.
//!
//! The driver is a trait so the machine is unit-tested against a scripted
//! fake with a fake clock; [`EngineDriver`] is the real one: a **private**
//! [`Engine`] (its own SQLite file, its own tmux socket, a scratch `$HOME`
//! holding only the draft manifest, a scratch git repo as the worktree) so a
//! validation run never touches the user's `flow.db`, tmux server or
//! `~/.smooth/harnesses/`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, EngineConfig, NewRequest};
use crate::store::SessionState;
use crate::tmux;

/// The prompt every validation session is launched with: short, no tools,
/// a one-word answer — so "reached idle" means "did a turn", not "did work".
pub const LAUNCH_PROMPT: &str = "Reply with exactly the single word READY and nothing else. Do not use any tools.";
/// The steer message sent once the first turn is idle.
pub const STEER_PROMPT: &str = "Reply with exactly the single word STEERED and nothing else. Do not use any tools.";

/// One observation of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub state: SessionState,
    /// The argv the engine launched (or relaunched) — how a resume is proven.
    pub argv: Vec<String>,
    pub agent_session_id: Option<String>,
    /// `inferred` | `hooks` | `native`.
    pub state_source: String,
    pub exit_code: Option<i32>,
}

/// What the machine drives. `wait` is the ONLY place time passes (the real
/// driver sleeps and ticks the supervisor; the fake advances a script).
pub trait FlowDriver {
    /// Launch a session of `kind` with `prompt`; returns the session id.
    ///
    /// # Errors
    /// When the engine refuses the launch (no manifest, no binary, tmux down).
    fn launch(&mut self, kind: &str, prompt: &str) -> Result<String>;
    /// # Errors
    /// When the session is unknown.
    fn probe(&mut self, id: &str) -> Result<Probe>;
    /// The visible pane, plain text.
    ///
    /// # Errors
    /// When the pane is gone.
    fn snapshot(&mut self, id: &str) -> Result<String>;
    /// Steer: paste `text` + submit.
    ///
    /// # Errors
    /// When the pane is gone.
    fn send(&mut self, id: &str, text: &str) -> Result<()>;
    /// Answer a dialog: press one named key (`Enter`, `n`) — no paste, no
    /// submit, no event row. Used only on first-run prompts.
    ///
    /// # Errors
    /// When the pane is gone.
    fn press(&mut self, id: &str, key: &str) -> Result<()>;
    /// Kill; with `resume`, relaunch per the manifest's resume rule.
    ///
    /// # Errors
    /// When the relaunch fails.
    fn kill(&mut self, id: &str, resume: bool) -> Result<Probe>;
    /// Best-effort teardown of the session.
    fn cleanup(&mut self, id: &str);
    /// Let `d` pass (and let the supervisor observe the pane).
    fn wait(&mut self, d: Duration);
    /// Monotonic time for the budget clock.
    fn now(&self) -> Instant;
}

/// Time budget for one validation run.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// From launch (or relaunch) to the first `working`/`idle`.
    pub boot: Duration,
    /// From `working` to `idle` for a turn.
    pub turn: Duration,
    /// Between probes.
    pub poll: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            boot: Duration::from_secs(60),
            turn: Duration::from_secs(120),
            poll: Duration::from_secs(1),
        }
    }
}

/// First-run prompts a validation run answers by itself, the way a person
/// would on a fresh machine: `(pattern, keys)`.
///
/// Patterns are matched case-insensitively against every visible line, first
/// rule wins; keys are tmux key names pressed in order. `Enter` accepts the
/// highlighted default —
/// what the CLI's author chose as safe (gemini's folder-trust + auth-method
/// dialogs, aider's `.gitignore` question). The one exception is "open the
/// docs in your browser?", whose default would pop a window on the user's
/// desk mid-validation. Sign-in prompts are deliberately NOT here — nobody
/// can answer them for the user, and `Enter` on one opens a browser login
/// (cursor-agent, gemini's "Sign in with Google") — see [`SIGN_IN`]; gemini's
/// auth-method dialog is answered only when it says an API key was detected,
/// because that is then the highlighted default.
pub const FIRST_RUN_PROMPTS: &[(&str, &[&str])] = &[
    // Questions first (they name the dialog), generic hints last — the order
    // only breaks ties on ONE line; the line nearest the cursor wins.
    (r"open (the )?(documentation|docs)( url)?", &["n", "Enter"]),
    (r"see what'?s new|release notes\?", &["n", "Enter"]),
    (r"do you trust", &["Enter"]),
    (r"trust (this |the )?(folder|workspace|directory|project)", &["Enter"]),
    (r"existing api key detected", &["Enter"]),
    (r"(select|choose) (a |your )?theme", &["Enter"]),
    (r"\(y\)es/\(n\)o", &["Enter"]),
    (r"\[y/n\]", &["Enter"]),
    (r"\(y/n\)", &["Enter"]),
    (r"press enter to (continue|confirm|select|proceed)", &["Enter"]),
    (r"use enter to select", &["Enter"]),
];

/// A sign-in is on screen: nothing in the window is answered (unless the
/// dialog also says an API key was detected — gemini lists "Sign in with
/// Google" as an option there, but highlights the key). Pressing a key on a
/// sign-in prompt starts a browser login on the user's desk.
const SIGN_IN: &str = r"sign[ -]?in|log[ -]?in|login";

/// How many first-run prompts one run answers before calling the harness
/// blocked: two dialogs is the most any known CLI shows (gemini), four is room.
pub const MAX_ANSWERS: usize = 4;

/// After an answer, how long an unchanged pane may sit before the prompt is
/// declared unanswerable (the key did nothing).
const ANSWER_SETTLE: Duration = Duration::from_secs(10);

/// Consecutive polls a scraped `idle` must hold before the boot / first turn
/// accepts it: gemini paints its composer a beat before the folder-trust
/// dialog pops over it, and one poll of `>` in the tail is not an idle turn.
const IDLE_CONFIRM_POLLS: usize = 3;

/// How many non-blank lines from the bottom a PENDING prompt may sit in: a
/// select dialog's `(Use Enter to select)` hint is ~3 up, its question ~9
/// (the question is only the label — see [`first_run_prompt`]).
const PROMPT_WINDOW: usize = 8;

/// A prompt is remembered by this many leading characters of its line, so a
/// scrolling CLI that echoes the typed answer onto the same line (`… [Yes]:
/// n`) does not look like a new prompt.
const PROMPT_KEY_CHARS: usize = 40;

fn first_run_rules() -> &'static [(regex::Regex, &'static [&'static str])] {
    static RULES: std::sync::OnceLock<Vec<(regex::Regex, &'static [&'static str])>> = std::sync::OnceLock::new();
    RULES.get_or_init(|| {
        FIRST_RUN_PROMPTS
            .iter()
            .map(|(p, keys)| {
                (
                    regex::RegexBuilder::new(p)
                        .case_insensitive(true)
                        .build()
                        .expect("FIRST_RUN_PROMPTS are valid regexes"),
                    *keys,
                )
            })
            .collect()
    })
}

/// Strip TUI box-drawing, padding and the `? ` / `● ` dialog markers from a
/// pane line.
fn clean_line(l: &str) -> String {
    let l = l.trim_matches(|c: char| c.is_whitespace() || "│┃║╭╮╰╯─┌┐└┘┏┓┗┛".contains(c));
    let l = l.trim_start_matches(|c: char| c.is_whitespace() || "?●○❯>".contains(c));
    l.trim().to_string()
}

/// A first-run prompt visible at the bottom of a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// What the user would read: the matched line, or the nearest question
    /// above a bare hint like `(Use Enter to select)`.
    pub label: String,
    /// Identity while it stays on screen: [`PROMPT_KEY_CHARS`] of the label
    /// and of the matched line — two select dialogs share the `(Use Enter to
    /// select)` hint but not the question; an echoed answer changes the tail
    /// of the line but not its head.
    pub key: (String, String),
    /// The keys that answer it.
    pub keys: &'static [&'static str],
}

/// The last 12 pane lines that still say something once cleaned (a
/// dialog's border-only rows are not lines).
fn tail_lines(pane: &str) -> Vec<String> {
    let lines: Vec<String> = pane.lines().map(clean_line).filter(|l| !l.is_empty()).collect();
    let start = lines.len().saturating_sub(12);
    lines[start..].to_vec()
}

fn prompt_key(line: &str) -> String {
    line.chars().take(PROMPT_KEY_CHARS).collect()
}

/// The first-run prompt pending in `pane`, if any: the matching line nearest
/// the cursor within the last [`PROMPT_WINDOW`] lines (a scrolling CLI keeps
/// its answered questions on screen above the live one), rules in priority
/// order on that line — and none at all while a sign-in is on screen.
#[must_use]
pub fn first_run_prompt(pane: &str) -> Option<Prompt> {
    static SIGN_IN_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let sign_in = SIGN_IN_RE.get_or_init(|| {
        regex::RegexBuilder::new(SIGN_IN)
            .case_insensitive(true)
            .build()
            .expect("SIGN_IN is a valid regex")
    });
    let tail = tail_lines(pane);
    let window_start = tail.len().saturating_sub(PROMPT_WINDOW);
    let window = &tail[window_start..];
    let (key_rule, _) = &first_run_rules()[4];
    if window.iter().any(|l| sign_in.is_match(l)) && !window.iter().any(|l| key_rule.is_match(l)) {
        return None;
    }
    for i in (window_start..tail.len()).rev() {
        if let Some(&(_, keys)) = first_run_rules().iter().find(|(re, _)| re.is_match(&tail[i])) {
            let label = if tail[i].contains('?') {
                tail[i].clone()
            } else {
                tail[..i].iter().rev().find(|l| l.ends_with('?')).cloned().unwrap_or_else(|| tail[i].clone())
            };
            return Some(Prompt {
                key: (prompt_key(&label), prompt_key(&tail[i])),
                label,
                keys,
            });
        }
    }
    None
}

/// The answers one run has given so far (only before the first idle and
/// after a resume — never during a steer, where a prompt is the harness's
/// real answer).
#[derive(Debug, Default)]
struct Answering {
    /// `"<prompt line> → <keys>"`, in order.
    answered: Vec<String>,
    /// Keys of answered prompts still on screen — not pending. Forgotten once
    /// they scroll away, so the same question after a relaunch is answered
    /// again.
    on_screen: Vec<(String, String)>,
    last_at: Option<Instant>,
}

impl Answering {
    /// The prompt pending in `pane`, if it is one this run has not answered
    /// while it stayed visible. Only [`FIRST_RUN_PROMPTS`] are answered: a
    /// `needs_you` the table does not name is the harness asking something
    /// nobody should guess at, and it stays the blocking reason.
    fn pending(&mut self, pane: &str) -> Option<Prompt> {
        let tail = tail_lines(pane);
        let shown = |k: &str| tail.iter().any(|l| l.starts_with(k));
        self.on_screen.retain(|(label, line)| shown(label) && shown(line));
        let prompt = first_run_prompt(pane)?;
        (!self.on_screen.contains(&prompt.key)).then_some(prompt)
    }
}

/// The steps a run tries to prove, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// The engine launched a session (binary resolved, tmux pane up).
    Launch,
    /// The session was observed `working` after launch.
    Working,
    /// The first turn reached `idle`.
    Idle,
    /// A steer was acknowledged (state went `working`, or the pane changed).
    Steer,
    /// The steered turn reached `idle`.
    SteerIdle,
    /// Kill + resume relaunched and the session came back (`working`/`idle`).
    Resume,
    /// The relaunch argv carried the harness session id (a true resume, not a
    /// fresh session).
    ResumeUsedSessionId,
}

impl Default for Probe {
    fn default() -> Self {
        Self {
            state: SessionState::Starting,
            argv: Vec::new(),
            agent_session_id: None,
            state_source: "inferred".to_string(),
            exit_code: None,
        }
    }
}

impl Step {
    /// All steps, in the order they run.
    pub const ALL: &'static [Self] = &[
        Self::Launch,
        Self::Working,
        Self::Idle,
        Self::Steer,
        Self::SteerIdle,
        Self::Resume,
        Self::ResumeUsedSessionId,
    ];
}

/// What a step ended as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "detail")]
pub enum Outcome {
    Proven,
    /// Could not be shown either way (e.g. no session id was ever learned).
    Unproven(String),
    /// Shown NOT to work.
    Failed(String),
    /// Not attempted because an earlier step failed.
    Skipped,
}

/// One proof line of the verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub step: Step,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// The result of one validation run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Verdict {
    pub session_id: Option<String>,
    pub proofs: Vec<Proof>,
    /// The last visible pane (the LLM's evidence for the next draft).
    pub pane_tail: String,
    /// `inferred` | `hooks` | `native` as the engine reported it.
    pub state_source: String,
    /// Launch + first idle proven — the manifest can run a session.
    pub usable: bool,
    /// Every step proven.
    pub complete: bool,
    /// Wall-clock seconds the run took.
    pub elapsed_secs: u64,
    /// First-run prompts the run answered by itself (`"<line> → <keys>"`),
    /// in order — a real session shows these to the user, so the manifest's
    /// `needs_you` should match them.
    #[serde(default)]
    pub answered: Vec<String>,
}

impl Verdict {
    fn set(&mut self, step: Step, outcome: Outcome) {
        if let Some(p) = self.proofs.iter_mut().find(|p| p.step == step) {
            p.outcome = outcome;
        } else {
            self.proofs.push(Proof { step, outcome });
        }
    }

    fn outcome(&self, step: Step) -> Option<&Outcome> {
        self.proofs.iter().find(|p| p.step == step).map(|p| &p.outcome)
    }

    /// Steps that were proven.
    #[must_use]
    pub fn proven(&self) -> Vec<Step> {
        self.proofs.iter().filter(|p| p.outcome == Outcome::Proven).map(|p| p.step).collect()
    }

    /// Steps that were not proven (unproven, failed or skipped), with why.
    #[must_use]
    pub fn not_proven(&self) -> Vec<(Step, String)> {
        self.proofs
            .iter()
            .filter_map(|p| match &p.outcome {
                Outcome::Proven => None,
                Outcome::Unproven(d) | Outcome::Failed(d) => Some((p.step, d.clone())),
                Outcome::Skipped => Some((p.step, "not attempted".to_string())),
            })
            .collect()
    }

    /// Failures only — what the next draft must fix.
    #[must_use]
    pub fn failures(&self) -> Vec<(Step, String)> {
        self.proofs
            .iter()
            .filter_map(|p| match &p.outcome {
                Outcome::Failed(d) => Some((p.step, d.clone())),
                _ => None,
            })
            .collect()
    }

    /// One line per step, for a report, plus one per first-run prompt the
    /// run answered.
    #[must_use]
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .proofs
            .iter()
            .map(|p| {
                let (glyph, detail) = match &p.outcome {
                    Outcome::Proven => ("●", String::new()),
                    Outcome::Unproven(d) => ("◐", format!(" — unproven: {d}")),
                    Outcome::Failed(d) => ("○", format!(" — FAILED: {d}")),
                    Outcome::Skipped => ("○", " — skipped".to_string()),
                };
                format!("{glyph} {}{detail}", step_label(p.step))
            })
            .collect();
        for a in &self.answered {
            lines.push(format!("◐ answered a first-run prompt by pressing its default: {a}"));
        }
        lines
    }
}

/// Human label for a step.
#[must_use]
pub const fn step_label(step: Step) -> &'static str {
    match step {
        Step::Launch => "launch (binary resolved, pane up)",
        Step::Working => "working observed after launch",
        Step::Idle => "first turn reached idle",
        Step::Steer => "steer acknowledged",
        Step::SteerIdle => "steered turn reached idle",
        Step::Resume => "kill + resume came back",
        Step::ResumeUsedSessionId => "resume reused the harness session id",
    }
}

/// The last 12 non-blank lines of a pane.
fn tail(pane: &str) -> String {
    let lines: Vec<&str> = pane.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(12);
    lines[start..].join("\n")
}

/// Poll until the session is in one of `want`, a terminal/blocked state, or
/// `budget` runs out. Returns the last probe and whether `working` was seen.
///
/// With `answering` (boot, first turn, the boot after a resume), a visible
/// first-run prompt is checked BEFORE the scraped state is believed — a
/// modal dialog over an idle-looking composer is not idle — and answered with
/// its default key, once while it stays on screen, at most [`MAX_ANSWERS`]
/// times. A `needs_you` the table does not name, or one the key did not move
/// past within [`ANSWER_SETTLE`], ends the wait as blocked; a scraped `idle`
/// must hold for [`IDLE_CONFIRM_POLLS`] polls.
fn wait_for(
    driver: &mut dyn FlowDriver,
    id: &str,
    want: &[SessionState],
    budget: Duration,
    poll: Duration,
    mut answering: Option<&mut Answering>,
) -> Result<(Probe, bool)> {
    let start = driver.now();
    let mut saw_working = false;
    let mut idle_streak = 0usize;
    loop {
        let p = driver.probe(id)?;
        if p.state == SessionState::Working {
            saw_working = true;
        }
        if matches!(p.state, SessionState::Limited | SessionState::Done | SessionState::Dead) {
            return Ok((p, saw_working));
        }
        let Some(a) = answering.as_deref_mut() else {
            if want.contains(&p.state) || p.state == SessionState::NeedsYou {
                return Ok((p, saw_working));
            }
            if driver.now().duration_since(start) >= budget {
                return Ok((p, saw_working));
            }
            driver.wait(poll);
            continue;
        };
        let pane = driver.snapshot(id).unwrap_or_default();
        if let Some(prompt) = a.pending(&pane) {
            idle_streak = 0;
            if a.answered.len() >= MAX_ANSWERS {
                return Ok((p, saw_working));
            }
            for k in prompt.keys {
                driver.press(id, k)?;
            }
            a.answered.push(format!("{} → {}", prompt.label, prompt.keys.join(" ")));
            a.on_screen.push(prompt.key);
            a.last_at = Some(driver.now());
            driver.wait(poll);
            continue;
        } else if p.state == SessionState::NeedsYou {
            // Either nothing was ever pressed, or the answer had its time to
            // land: the harness is still asking.
            if a.last_at.is_none_or(|t| driver.now().duration_since(t) >= ANSWER_SETTLE) {
                return Ok((p, saw_working));
            }
        } else if want.contains(&p.state) {
            if p.state == SessionState::Idle {
                idle_streak += 1;
                if idle_streak >= IDLE_CONFIRM_POLLS {
                    return Ok((p, saw_working));
                }
            } else {
                return Ok((p, saw_working));
            }
        } else {
            idle_streak = 0;
        }
        if driver.now().duration_since(start) >= budget {
            return Ok((p, saw_working));
        }
        driver.wait(poll);
    }
}

/// Why a run stopped on `p`, when it did; `ans` is what the run already
/// pressed, so the reason says the prompt survived that.
fn blocked_reason(p: &Probe, pane: &str, ans: &Answering) -> Option<String> {
    let answered = &ans.answered;
    let pressed = if answered.is_empty() {
        String::new()
    } else {
        format!(
            " (already pressed the default on {} first-run prompt(s): {})",
            answered.len(),
            answered.join("; ")
        )
    };
    if !answered.is_empty() && !matches!(p.state, SessionState::Limited | SessionState::Done | SessionState::Dead) {
        // A NEW prompt still pending (not one already answered and merely
        // still on screen) is what the run could not get past.
        if let Some(prompt) = first_run_prompt(pane).filter(|pr| !ans.on_screen.contains(&pr.key)) {
            return Some(format!(
                "a first-run prompt the validator could not get past{pressed}: {}\n{}",
                prompt.label,
                tail(pane)
            ));
        }
    }
    match p.state {
        SessionState::NeedsYou => Some(format!(
            "the harness is waiting on an approval/auth/trust prompt the engine cannot answer for you{pressed}:\n{}",
            tail(pane)
        )),
        SessionState::Limited => Some(format!("the harness reported a usage limit:\n{}", tail(pane))),
        SessionState::Done => Some(format!("the process exited (code {:?}) before the turn finished:\n{}", p.exit_code, tail(pane))),
        SessionState::Dead => Some(format!("the process died (exit {:?}) and could not be resumed:\n{}", p.exit_code, tail(pane))),
        _ => None,
    }
}

/// Run the state machine for harness `kind` over `driver`.
#[allow(clippy::too_many_lines, reason = "the steps read best as one linear machine")]
pub fn validate(driver: &mut dyn FlowDriver, kind: &str, budget: &Budget) -> Verdict {
    let started = driver.now();
    let mut v = Verdict::default();
    for s in Step::ALL {
        v.set(*s, Outcome::Skipped);
    }
    let mut ans = Answering::default();
    let finish = |mut v: Verdict, driver: &mut dyn FlowDriver, ans: &Answering| {
        v.answered.clone_from(&ans.answered);
        v.elapsed_secs = driver.now().duration_since(started).as_secs();
        v.usable = v.outcome(Step::Launch) == Some(&Outcome::Proven) && v.outcome(Step::Idle) == Some(&Outcome::Proven);
        v.complete = v.proofs.iter().all(|p| p.outcome == Outcome::Proven);
        v
    };

    // 1. Launch.
    let id = match driver.launch(kind, LAUNCH_PROMPT) {
        Ok(id) => id,
        Err(e) => {
            v.set(Step::Launch, Outcome::Failed(format!("{e:#}")));
            return finish(v, driver, &ans);
        }
    };
    v.session_id = Some(id.clone());
    v.set(Step::Launch, Outcome::Proven);

    // 2. Boot → working/idle.
    let (p, saw_working) = match wait_for(
        driver,
        &id,
        &[SessionState::Working, SessionState::Idle],
        budget.boot,
        budget.poll,
        Some(&mut ans),
    ) {
        Ok(x) => x,
        Err(e) => {
            v.set(Step::Working, Outcome::Failed(format!("{e:#}")));
            driver.cleanup(&id);
            return finish(v, driver, &ans);
        }
    };
    let pane = driver.snapshot(&id).unwrap_or_default();
    v.pane_tail = tail(&pane);
    v.state_source.clone_from(&p.state_source);
    if let Some(why) = blocked_reason(&p, &pane, &ans) {
        v.set(Step::Working, if saw_working { Outcome::Proven } else { Outcome::Failed(why.clone()) });
        v.set(Step::Idle, Outcome::Failed(why));
        driver.cleanup(&id);
        return finish(v, driver, &ans);
    }
    // 3. First turn → idle.
    let (p, saw_working2) = if p.state == SessionState::Idle {
        (p, saw_working)
    } else {
        match wait_for(driver, &id, &[SessionState::Idle], budget.turn, budget.poll, Some(&mut ans)) {
            Ok((p2, w)) => (p2, saw_working || w),
            Err(e) => {
                v.set(Step::Idle, Outcome::Failed(format!("{e:#}")));
                driver.cleanup(&id);
                return finish(v, driver, &ans);
            }
        }
    };
    let pane = driver.snapshot(&id).unwrap_or_default();
    v.pane_tail = tail(&pane);
    v.state_source.clone_from(&p.state_source);
    v.set(
        Step::Working,
        if saw_working2 {
            Outcome::Proven
        } else {
            Outcome::Unproven("never observed `working` — the turn finished within one poll, or the working pattern does not match this harness".to_string())
        },
    );
    if p.state != SessionState::Idle {
        let why = blocked_reason(&p, &pane, &ans).unwrap_or_else(|| {
            format!(
                "never reached idle within {}s (state stayed `{}`): the idle pattern probably does not match this harness's prompt, or the prompt never reached it\n{}",
                budget.boot.as_secs() + budget.turn.as_secs(),
                p.state,
                tail(&pane)
            )
        });
        v.set(Step::Idle, Outcome::Failed(why));
        driver.cleanup(&id);
        return finish(v, driver, &ans);
    }
    v.set(Step::Idle, Outcome::Proven);

    // 4. Steer.
    let before = pane;
    match driver.send(&id, STEER_PROMPT) {
        Ok(()) => {
            let (p, w) = wait_for(driver, &id, &[SessionState::Working], budget.boot, budget.poll, None).unwrap_or_else(|_| (Probe::default(), false));
            let after = driver.snapshot(&id).unwrap_or_default();
            let changed = after != before;
            if w || p.state == SessionState::Working {
                v.set(Step::Steer, Outcome::Proven);
            } else if changed {
                v.set(
                    Step::Steer,
                    Outcome::Unproven("the pane changed after the steer but `working` was never observed".to_string()),
                );
            } else {
                v.set(
                    Step::Steer,
                    Outcome::Failed(format!(
                        "nothing happened after pasting a message + Enter (steer method / submit key?):\n{}",
                        tail(&after)
                    )),
                );
            }
            if !matches!(v.outcome(Step::Steer), Some(Outcome::Failed(_))) {
                let (p2, _) = wait_for(driver, &id, &[SessionState::Idle], budget.turn, budget.poll, None).unwrap_or((p, false));
                let pane = driver.snapshot(&id).unwrap_or_default();
                v.pane_tail = tail(&pane);
                if p2.state == SessionState::Idle {
                    v.set(Step::SteerIdle, Outcome::Proven);
                } else {
                    v.set(
                        Step::SteerIdle,
                        blocked_reason(&p2, &pane, &Answering::default()).map_or_else(
                            || Outcome::Failed(format!("the steered turn never reached idle (state `{}`)\n{}", p2.state, tail(&pane))),
                            Outcome::Failed,
                        ),
                    );
                }
            }
        }
        Err(e) => v.set(Step::Steer, Outcome::Failed(format!("{e:#}"))),
    }

    // 5. Kill + resume.
    let agent_id = driver.probe(&id).ok().and_then(|p| p.agent_session_id);
    match driver.kill(&id, true) {
        Ok(relaunched) => {
            let (p, _) = wait_for(
                driver,
                &id,
                &[SessionState::Working, SessionState::Idle],
                budget.boot,
                budget.poll,
                Some(&mut ans),
            )
            .unwrap_or_else(|_| (relaunched.clone(), false));
            let pane = driver.snapshot(&id).unwrap_or_default();
            v.pane_tail = tail(&pane);
            if matches!(p.state, SessionState::Working | SessionState::Idle) {
                v.set(Step::Resume, Outcome::Proven);
            } else {
                v.set(
                    Step::Resume,
                    Outcome::Failed(
                        blocked_reason(&p, &pane, &ans).unwrap_or_else(|| format!("the relaunch never came up (state `{}`)\n{}", p.state, tail(&pane))),
                    ),
                );
            }
            match &agent_id {
                Some(sid) if relaunched.argv.iter().any(|a| a.contains(sid.as_str())) => v.set(Step::ResumeUsedSessionId, Outcome::Proven),
                Some(sid) => v.set(
                    Step::ResumeUsedSessionId,
                    Outcome::Unproven(format!(
                        "the relaunch argv did not carry the session id {sid} (resume.mode = relaunch_command, or resume.argv lacks {{session_id}})"
                    )),
                ),
                None => v.set(
                    Step::ResumeUsedSessionId,
                    Outcome::Unproven(
                        "no harness session id was learned (state.source is not hooks, or no hook landed) — the resume relaunched the original command"
                            .to_string(),
                    ),
                ),
            }
        }
        Err(e) => v.set(Step::Resume, Outcome::Failed(format!("{e:#}"))),
    }
    driver.cleanup(&id);
    finish(v, driver, &ans)
}

// ── the real driver ──────────────────────────────────────────────────────────

/// A private engine: its own db, tmux socket, scratch `$HOME` (holding only
/// the draft manifest) and a scratch git repo as the worktree.
pub struct EngineDriver {
    engine: Engine,
    socket: String,
    workspace: PathBuf,
    model: Option<String>,
    _scratch: tempfile::TempDir,
}

impl EngineDriver {
    /// Build one for `manifest_toml` (named `name`). `model` is passed through
    /// to `{model}`; `None` lets the harness pick its default.
    ///
    /// # Errors
    /// When the scratch dirs, the manifest file or the engine cannot be set up,
    /// or tmux is not installed.
    pub fn private(name: &str, manifest_toml: &str, model: Option<String>) -> Result<Self> {
        if !tmux::tmux_available() {
            anyhow::bail!("tmux is not installed — SmoothFlow needs it to run a session");
        }
        let scratch = tempfile::Builder::new().prefix("smooth-harness-validate-").tempdir().context("scratch dir")?;
        let home = scratch.path().join("home");
        let manifests = home.join(".smooth").join("harnesses");
        std::fs::create_dir_all(&manifests)?;
        std::fs::write(manifests.join(format!("{name}.toml")), manifest_toml)?;
        let workspace = scratch.path().join("ws");
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(workspace.join("README.md"), "# harness validation scratch\n")?;
        // Some harnesses insist on a git repo; a throwaway one costs nothing.
        let _ = std::process::Command::new("git").args(["init", "-q"]).current_dir(&workspace).output();
        let _ = std::process::Command::new("git")
            .args(["-c", "user.email=validate@smoo.ai", "-c", "user.name=validate", "add", "-A"])
            .current_dir(&workspace)
            .output();
        let _ = std::process::Command::new("git")
            .args(["-c", "user.email=validate@smoo.ai", "-c", "user.name=validate", "commit", "-q", "-m", "scratch"])
            .current_dir(&workspace)
            .output();
        let socket = format!(
            "smooth-flow-validate-{}-{}",
            std::process::id(),
            &uuid::Uuid::new_v4().simple().to_string()[..6]
        );
        let engine = Engine::open(EngineConfig {
            db_path: scratch.path().join("flow.db"),
            version: "validate".into(),
            machine_label: "harness-validate".into(),
            home,
            daemon_url: None,
            ..EngineConfig::new(workspace.clone())
        })?;
        Ok(Self {
            engine,
            socket,
            workspace,
            model,
            _scratch: scratch,
        })
    }

    /// The scratch worktree the harness runs in.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
}

impl Drop for EngineDriver {
    fn drop(&mut self) {
        tmux::kill_server(&self.socket);
    }
}

fn probe_of(s: &crate::store::Session) -> Probe {
    Probe {
        state: s.state,
        argv: s.argv.clone(),
        agent_session_id: s.agent_session_id.clone(),
        state_source: s.state_source.clone(),
        exit_code: s.exit_code,
    }
}

impl FlowDriver for EngineDriver {
    fn launch(&mut self, kind: &str, prompt: &str) -> Result<String> {
        let kind: crate::store::SessionKind = kind.parse()?;
        let s = self.engine.new_session(NewRequest {
            kind,
            worktree: Some(self.workspace.to_string_lossy().into_owned()),
            prompt: Some(prompt.to_string()),
            model: self.model.clone(),
            tmux_socket: Some(self.socket.clone()),
            title: Some("harness validation".into()),
            ..NewRequest::default()
        })?;
        Ok(s.id)
    }

    fn probe(&mut self, id: &str) -> Result<Probe> {
        self.engine
            .get(id)?
            .map(|s| probe_of(&s))
            .ok_or_else(|| anyhow::anyhow!("no such session {id}"))
    }

    fn snapshot(&mut self, id: &str) -> Result<String> {
        match self.engine.snapshot(id)? {
            crate::protocol::ServerFrame::Screen { text, .. } => Ok(text),
            _ => Ok(String::new()),
        }
    }

    fn send(&mut self, id: &str, text: &str) -> Result<()> {
        self.engine.send(id, text)
    }

    fn press(&mut self, id: &str, key: &str) -> Result<()> {
        self.engine.send_key(id, key)
    }

    fn kill(&mut self, id: &str, resume: bool) -> Result<Probe> {
        self.engine.kill(id, resume).map(|s| probe_of(&s))
    }

    fn cleanup(&mut self, id: &str) {
        if let Ok(Some(s)) = self.engine.get(id) {
            if !s.state.is_terminal() {
                let _ = self.engine.kill(id, false);
            }
        }
        let _ = self.engine.remove(id);
    }

    fn wait(&mut self, d: Duration) {
        std::thread::sleep(d);
        if let Err(e) = self.engine.supervise_tick() {
            tracing::warn!(error = %e, "harness validation: supervise tick");
        }
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A scripted session: each `wait` pops the next state; `send` and
    /// `kill(resume)` push their own scripted follow-ups.
    struct Fake {
        script: VecDeque<SessionState>,
        after_send: VecDeque<SessionState>,
        after_resume: VecDeque<SessionState>,
        state: SessionState,
        pane: String,
        clock: Instant,
        agent_id: Option<String>,
        relaunch_argv: Vec<String>,
        launch_err: Option<String>,
        send_err: Option<String>,
        cleaned: bool,
        sends: Vec<String>,
        pane_changes_on_send: bool,
        /// Each `press` shows the next pane (a dialog chain), then the script
        /// continues with `after_answer`; an empty queue leaves the pane as is.
        answer_panes: VecDeque<String>,
        after_answer: VecDeque<SessionState>,
        presses: Vec<String>,
    }

    impl Fake {
        fn new(script: &[SessionState]) -> Self {
            Self {
                script: script.iter().copied().collect(),
                after_send: VecDeque::new(),
                after_resume: VecDeque::new(),
                state: SessionState::Starting,
                pane: "banner\n> ".into(),
                clock: Instant::now(),
                agent_id: None,
                relaunch_argv: vec!["tool".into()],
                launch_err: None,
                send_err: None,
                cleaned: false,
                sends: Vec::new(),
                pane_changes_on_send: true,
                answer_panes: VecDeque::new(),
                after_answer: VecDeque::new(),
                presses: Vec::new(),
            }
        }
    }

    impl FlowDriver for Fake {
        fn launch(&mut self, _kind: &str, _prompt: &str) -> Result<String> {
            if let Some(e) = &self.launch_err {
                anyhow::bail!("{e}");
            }
            Ok("fs-1".into())
        }
        fn probe(&mut self, _id: &str) -> Result<Probe> {
            Ok(Probe {
                state: self.state,
                argv: self.relaunch_argv.clone(),
                agent_session_id: self.agent_id.clone(),
                state_source: "inferred".into(),
                exit_code: None,
            })
        }
        fn snapshot(&mut self, _id: &str) -> Result<String> {
            Ok(self.pane.clone())
        }
        fn send(&mut self, _id: &str, text: &str) -> Result<()> {
            if let Some(e) = &self.send_err {
                anyhow::bail!("{e}");
            }
            self.sends.push(text.to_string());
            if self.pane_changes_on_send {
                self.pane.push_str("\nsteered");
            }
            self.script = std::mem::take(&mut self.after_send);
            Ok(())
        }
        fn press(&mut self, _id: &str, key: &str) -> Result<()> {
            self.presses.push(key.to_string());
            if let Some(p) = self.answer_panes.pop_front() {
                self.pane = p;
                // The last dialog answered ⇒ the harness gets on with the turn.
                if self.answer_panes.is_empty() && !self.after_answer.is_empty() {
                    self.script = std::mem::take(&mut self.after_answer);
                }
            }
            Ok(())
        }
        fn kill(&mut self, _id: &str, resume: bool) -> Result<Probe> {
            self.state = if resume { SessionState::Starting } else { SessionState::Done };
            self.script = std::mem::take(&mut self.after_resume);
            self.probe("fs-1")
        }
        fn cleanup(&mut self, _id: &str) {
            self.cleaned = true;
        }
        fn wait(&mut self, d: Duration) {
            self.clock += d;
            if let Some(s) = self.script.pop_front() {
                self.state = s;
            }
        }
        fn now(&self) -> Instant {
            self.clock
        }
    }

    fn budget() -> Budget {
        Budget {
            boot: Duration::from_secs(5),
            turn: Duration::from_secs(5),
            poll: Duration::from_secs(1),
        }
    }

    use SessionState::{Idle, NeedsYou, Starting, Working};

    #[test]
    fn happy_path_proves_everything_with_hooks_session_id() {
        let mut f = Fake::new(&[Starting, Working, Idle]);
        f.after_send = [Working, Idle].into();
        f.after_resume = [Starting, Idle].into();
        f.agent_id = Some("sid-123".into());
        f.relaunch_argv = vec!["tool".into(), "--resume".into(), "sid-123".into()];
        let v = validate(&mut f, "tool", &budget());
        assert!(v.complete, "{v:#?}");
        assert!(v.usable);
        assert_eq!(v.proven().len(), Step::ALL.len());
        assert_eq!(v.session_id.as_deref(), Some("fs-1"));
        assert_eq!(f.sends, vec![STEER_PROMPT]);
        assert!(f.cleaned, "the session is torn down");
        assert!(v.summary_lines().iter().all(|l| l.starts_with('●')), "{:?}", v.summary_lines());
    }

    #[test]
    fn launch_failure_skips_the_rest() {
        let mut f = Fake::new(&[]);
        f.launch_err = Some("no manifest".into());
        let v = validate(&mut f, "tool", &budget());
        assert!(!v.usable && !v.complete);
        assert!(matches!(v.outcome(Step::Launch), Some(Outcome::Failed(d)) if d.contains("no manifest")));
        assert_eq!(v.failures().len(), 1);
        assert_eq!(v.not_proven().len(), Step::ALL.len());
        assert!(v.session_id.is_none());
    }

    #[test]
    fn never_idle_fails_idle_with_the_pane_tail_and_is_unusable() {
        // Working forever: boot sees working, the turn budget runs out.
        let mut f = Fake::new(&[
            Working, Working, Working, Working, Working, Working, Working, Working, Working, Working, Working, Working,
        ]);
        f.pane = "spinner\nesc to interrupt".into();
        let v = validate(&mut f, "tool", &budget());
        assert!(!v.usable);
        assert_eq!(v.outcome(Step::Working), Some(&Outcome::Proven));
        assert!(
            matches!(v.outcome(Step::Idle), Some(Outcome::Failed(d)) if d.contains("idle pattern") && d.contains("esc to interrupt")),
            "{v:#?}"
        );
        assert_eq!(v.outcome(Step::Steer), Some(&Outcome::Skipped));
        assert!(f.cleaned);
        assert_eq!(v.pane_tail, "spinner\nesc to interrupt");
    }

    /// A prompt the default key does not move past (the pane never changes)
    /// is reported as the blocker — with what was pressed, so the drafter
    /// does not press it again.
    #[test]
    fn approval_prompt_blocks_with_a_clear_reason() {
        let mut f = Fake::new(&[Starting, NeedsYou, NeedsYou, NeedsYou, NeedsYou, NeedsYou, NeedsYou, NeedsYou]);
        f.pane = "Trust this folder? (y/n)".into();
        let v = validate(&mut f, "tool", &budget());
        assert!(
            matches!(v.outcome(Step::Idle), Some(Outcome::Failed(d)) if d.contains("approval/auth/trust") && d.contains("(y/n)") && d.contains("already pressed")),
            "{v:#?}"
        );
        assert!(matches!(v.outcome(Step::Working), Some(Outcome::Failed(_))));
        assert!(!v.usable);
        assert_eq!(f.presses, vec!["Enter"], "answered once, then left alone");
        assert_eq!(v.answered, vec!["Trust this folder? (y/n) → Enter"]);
    }

    /// A sign-in prompt is never pressed — not when the manifest's
    /// `needs_you` flags it (cursor-agent: "press any key" ⇒ NeedsYou), not
    /// when it doesn't: the pane tail carries the reason.
    #[test]
    fn a_sign_in_prompt_is_never_answered() {
        for script in [&[Starting; 6][..], &[Starting, NeedsYou, NeedsYou, NeedsYou, NeedsYou, NeedsYou][..]] {
            let mut f = Fake::new(script);
            f.pane = "CURSOR AGENT\nPress any key to sign in...".into();
            let v = validate(&mut f, "tool", &budget());
            assert!(f.presses.is_empty(), "{:?}", f.presses);
            assert!(v.answered.is_empty());
            assert!(
                matches!(v.outcome(Step::Idle), Some(Outcome::Failed(d)) if d.contains("Press any key to sign in")),
                "{v:#?}"
            );
        }
    }

    /// A `needs_you` the table does not name (an approval, an unknown
    /// dialog) is the harness asking — it is reported, never guessed at.
    #[test]
    fn an_unknown_needs_you_is_reported_not_pressed() {
        let mut f = Fake::new(&[Starting, NeedsYou, NeedsYou, NeedsYou]);
        f.pane = "Allow the agent to run `rm -rf build`?\n❯ 1. Yes\n  2. No".into();
        let v = validate(&mut f, "tool", &budget());
        assert!(f.presses.is_empty(), "{:?}", f.presses);
        assert!(
            matches!(v.outcome(Step::Idle), Some(Outcome::Failed(d)) if d.contains("approval/auth/trust") && d.contains("rm -rf build")),
            "{v:#?}"
        );
    }

    #[test]
    fn first_run_dialog_chain_is_answered_then_the_turn_runs() {
        let mut f = Fake::new(&[Starting, Starting, Starting, Starting, Starting, Starting]);
        f.pane = "│ ? Do you trust the files in this folder?\n│ ● 1. Trust folder\n│ (Use Enter to select)".into();
        f.answer_panes = [
            "│ ? Get started\n│ How would you like to authenticate for this project?\n│ 1. Sign in with Google\n│ ● 2. Use Gemini API Key\n│ Existing API key detected (GEMINI_API_KEY). Select \"Gemini API Key\" option to use it.\n│ (Use Enter to select)".to_string(),
            "banner\n> ".to_string(),
        ]
        .into();
        f.after_answer = [Working, Idle].into();
        f.after_send = [Working, Idle].into();
        f.after_resume = [Idle].into();
        let v = validate(&mut f, "tool", &budget());
        assert!(v.usable, "{v:#?}");
        assert_eq!(v.outcome(Step::Idle), Some(&Outcome::Proven));
        assert_eq!(f.presses, vec!["Enter", "Enter"]);
        assert_eq!(
            v.answered,
            vec![
                "Do you trust the files in this folder? → Enter",
                "How would you like to authenticate for this project? → Enter"
            ]
        );
        assert!(
            v.summary_lines()
                .iter()
                .any(|l| l.contains("answered a first-run prompt") && l.contains("Do you trust")),
            "{:?}",
            v.summary_lines()
        );
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["answered"].as_array().unwrap().len(), 2);
    }

    /// An endless chain of dialogs stops at the cap instead of pressing Enter
    /// forever.
    #[test]
    fn first_run_answers_are_capped() {
        let mut f = Fake::new(&[Starting; 12]);
        f.pane = "Select a theme (Use Enter to select)".into();
        f.answer_panes = (1..=8).map(|i| format!("Dialog {i} (Use Enter to select)")).collect();
        let v = validate(&mut f, "tool", &budget());
        assert_eq!(f.presses.len(), MAX_ANSWERS);
        assert_eq!(v.answered.len(), MAX_ANSWERS);
        assert!(!v.usable);
        assert!(
            matches!(v.outcome(Step::Idle), Some(Outcome::Failed(d)) if d.contains("could not get past")),
            "{v:#?}"
        );
    }

    /// The docs-in-a-browser question is declined, not accepted.
    #[test]
    fn open_docs_prompt_is_declined() {
        let mut f = Fake::new(&[Starting, Starting, Starting, Starting]);
        f.pane = "Warning for openai/x: Unknown context window size\nOpen documentation url for more info? (Y)es/(N)o/(D)on't ask again [Yes]:".into();
        f.answer_panes = ["> ".to_string()].into();
        f.after_answer = [Idle].into();
        f.after_send = [Working, Idle].into();
        f.after_resume = [Idle].into();
        let v = validate(&mut f, "tool", &budget());
        assert_eq!(f.presses, vec!["n", "Enter"]);
        assert!(v.usable, "{v:#?}");
        assert!(v.answered[0].ends_with("→ n Enter"), "{:?}", v.answered);
    }

    /// A prompt during the STEER is the harness's real answer to the steer —
    /// it is never pressed through.
    #[test]
    fn a_prompt_during_the_steer_is_not_answered() {
        let mut f = Fake::new(&[Starting, Working, Idle]);
        f.after_send = [NeedsYou, NeedsYou, NeedsYou].into();
        f.answer_panes = ["> ".to_string()].into();
        let v = validate(&mut f, "tool", &budget());
        assert!(f.presses.is_empty(), "{:?}", f.presses);
        assert!(
            matches!(v.outcome(Step::SteerIdle), Some(Outcome::Failed(d)) if d.contains("approval/auth/trust")),
            "{v:#?}"
        );
    }

    /// gemini paints `>` in the composer, then pops the trust dialog over it:
    /// the dialog wins over the scraped idle, and idle must hold to count.
    #[test]
    fn a_modal_dialog_over_an_idle_looking_pane_is_answered_first() {
        let mut f = Fake::new(&[Idle; 12]);
        f.pane = "Tips for getting started\n > Reply with READY\n│ ? Do you trust the files in this folder?\n│ (Use Enter to select)".into();
        f.answer_panes = ["> Reply with READY\n✦ READY\n> ".to_string()].into();
        f.after_send = [Working, Idle].into();
        f.after_resume = [Idle].into();
        let v = validate(&mut f, "tool", &budget());
        assert_eq!(f.presses, vec!["Enter"]);
        assert!(v.usable, "{v:#?}");
        assert_eq!(v.answered, vec!["Do you trust the files in this folder? → Enter"]);
        assert_eq!(v.outcome(Step::Steer), Some(&Outcome::Proven), "the steer landed on a real composer");
    }

    /// One poll of idle is a blip, not a turn: it has to hold for
    /// IDLE_CONFIRM_POLLS consecutive polls before the boot accepts it.
    #[test]
    fn scraped_idle_must_hold_for_consecutive_polls() {
        // idle, then not, then idle for good — the first idle is not believed.
        let mut f = Fake::new(&[Idle, Starting, Idle, Idle, Idle, Idle, Idle, Idle]);
        f.after_send = [Working, Idle].into();
        f.after_resume = [Idle].into();
        let start = f.clock;
        let v = validate(&mut f, "tool", &budget());
        assert!(v.usable, "{v:#?}");
        // boot: polls at 0..=2 (idle, starting, idle) can't confirm; 3 consecutive idles by t=5.
        assert!(f.clock.duration_since(start) >= Duration::from_secs(4), "{:?}", f.clock.duration_since(start));
    }

    /// aider: the answered questions stay on screen (with the typed answer
    /// echoed onto the line) while the CLI boots on to its prompt — they are
    /// answered ONCE, not until the cap.
    #[test]
    fn a_scrolling_cli_keeps_answered_prompts_visible_without_re_answering() {
        let q1 = "Add .aider* to .gitignore (recommended)? (Y)es/(N)o [Yes]:";
        let q2 = "Open documentation url for more info? (Y)es/(N)o/(D)on't ask again [Yes]:";
        let mut f = Fake::new(&[Starting; 12]);
        f.pane = format!("banner\n{q1}");
        f.answer_panes = [
            format!("banner\n{q1}\nAdded .aider* to .gitignore\nWarning for openai/x: Unknown context window\n{q2}"),
            format!("banner\n{q1}\nAdded .aider* to .gitignore\nWarning for openai/x: Unknown context window\n{q2} n\nAider v0.86.2\nMain model: x\n> "),
        ]
        .into();
        f.after_answer = [Idle; 6].into();
        f.after_send = [Working, Idle].into();
        f.after_resume = [Idle].into();
        let v = validate(&mut f, "tool", &budget());
        assert_eq!(f.presses, vec!["Enter", "n", "Enter"], "{v:#?}");
        assert_eq!(v.answered.len(), 2);
        assert!(v.usable, "{v:#?}");
    }

    #[test]
    fn first_run_prompt_recognizes_real_dialogs_only() {
        // gemini 0.59's dialog verbatim: border-only rows between the lines,
        // no "(Use Enter to select)" hint, the question 7 rows up.
        let gemini_trust = " ╭──────╮\n │      │\n │ Do you trust the files in this folder?      │\n │      │\n │ Trusting a folder allows Gemini CLI to load its local configurations, including custom commands, hooks, MCP servers, agent skills, and │\n │ settings. These configurations could execute code on your behalf or change the behavior of the CLI.   │\n │      │\n │      │\n │ ● 1. Trust folder (ws)     │\n │   2. Trust parent folder (probe2)   │\n │   3. Don't trust      │\n │      │\n ╰──────╯";
        let pr = first_run_prompt(gemini_trust).unwrap();
        assert_eq!(pr.label, "Do you trust the files in this folder?");
        assert_eq!(pr.keys, &["Enter"]);
        assert_eq!(tail_lines(gemini_trust).len(), 6, "border-only rows are not lines");
        // gemini's auth dialog: the question sits 9 lines up (outside the
        // pending window); the hint is what's pending, the question the label.
        let gemini_auth = "│ ? Get started\n│ How would you like to authenticate for this project?\n│ 1. Sign in with Google\n│ ● 2. Use Gemini API Key\n│ 3. Vertex AI\n│ Existing API key detected (GEMINI_API_KEY). Select \"Gemini API Key\" option to use it.\n│ (Use Enter to select)\n│ Terms of Services and Privacy Notice for Gemini CLI\n│ https://geminicli.com/docs/resources/tos-privacy/\n╰─────╯";
        let pr = first_run_prompt(gemini_auth).unwrap();
        assert_eq!(pr.label, "How would you like to authenticate for this project?");
        assert!(
            pr.key.0.starts_with("How would you like to authenticate") && pr.key.1.starts_with("(Use Enter"),
            "{pr:?}"
        );
        let aider = "You can skip this check with --no-gitignore\nAdd .aider* to .gitignore (recommended)? (Y)es/(N)o [Yes]:";
        assert_eq!(
            first_run_prompt(aider).unwrap().label,
            "Add .aider* to .gitignore (recommended)? (Y)es/(N)o [Yes]:"
        );
        let docs = "Open documentation url for more info? (Y)es/(N)o/(D)on't ask again [Yes]:";
        assert_eq!(first_run_prompt(docs).unwrap().keys, &["n", "Enter"]);
        // aider run 6: the answered docs question is still on screen (with the
        // pasted launch prompt mangled into it) ABOVE the live "what's new?"
        // — the line nearest the cursor is the pending one, and it is declined.
        let aider_whats_new = "Please answer with one of: yes, no, skip, all, don't\nOpen documentation url for more info? (Y)es/(N)o/(D)on't ask again [Yes]: n\nAider v0.86.2\nModel: openai/deepseek-v4-flash with whole edit format\nGit repo: .git with 1 files\nRepo-map: using 1024 tokens, auto refresh\nhttps://aider.chat/HISTORY.html#release-notes\nWould you like to see what's new in this version? (Y)es/(N)o [Yes]:";
        let pr = first_run_prompt(aider_whats_new).unwrap();
        assert!(pr.label.starts_with("Would you like to see what's new"), "{pr:?}");
        assert_eq!(pr.keys, &["n", "Enter"]);
        // An answered prompt that scrolled up past the window is not pending.
        let scrolled = format!("{docs}\n{}", (1..=9).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"));
        assert!(first_run_prompt(&scrolled).is_none());
        assert!(first_run_prompt("CURSOR AGENT\nPress any key to sign in...").is_none());
        // gemini's auth dialog WITHOUT a detected key highlights "Sign in with
        // Google": Enter would open a browser — never answered.
        let gemini_auth_no_key = "│ How would you like to authenticate for this project?\n│ ● 1. Sign in with Google\n│ 2. Use Gemini API Key\n│ 3. Vertex AI\n│ (Use Enter to select)";
        assert!(first_run_prompt(gemini_auth_no_key).is_none());
        assert!(first_run_prompt("Log in to continue\nPress Enter to continue").is_none());
        assert!(first_run_prompt("Tips for getting started:\n> ").is_none());
        assert!(first_run_prompt("").is_none());
        assert_eq!(FIRST_RUN_PROMPTS.len(), first_run_rules().len());
    }

    #[test]
    fn idle_without_working_is_usable_but_working_is_unproven() {
        let mut f = Fake::new(&[Idle]);
        f.after_send = [Idle].into();
        f.after_resume = [Idle].into();
        f.pane_changes_on_send = true;
        let v = validate(&mut f, "tool", &budget());
        assert!(v.usable, "{v:#?}");
        assert!(!v.complete);
        assert!(matches!(v.outcome(Step::Working), Some(Outcome::Unproven(_))));
        // Steer: pane changed but no working → unproven, and the steered idle still proves.
        assert!(matches!(v.outcome(Step::Steer), Some(Outcome::Unproven(d)) if d.contains("pane changed")));
        assert_eq!(v.outcome(Step::SteerIdle), Some(&Outcome::Proven));
        assert_eq!(v.outcome(Step::Resume), Some(&Outcome::Proven));
        assert!(matches!(v.outcome(Step::ResumeUsedSessionId), Some(Outcome::Unproven(d)) if d.contains("no harness session id")));
    }

    #[test]
    fn steer_that_changes_nothing_fails_and_skips_steer_idle() {
        let mut f = Fake::new(&[Working, Idle]);
        f.after_send = [Idle, Idle, Idle, Idle, Idle, Idle].into();
        f.after_resume = [Idle].into();
        f.pane_changes_on_send = false;
        let v = validate(&mut f, "tool", &budget());
        assert!(
            matches!(v.outcome(Step::Steer), Some(Outcome::Failed(d)) if d.contains("nothing happened")),
            "{v:#?}"
        );
        assert_eq!(v.outcome(Step::SteerIdle), Some(&Outcome::Skipped));
        // Resume is still attempted — it is independent of steering.
        assert_eq!(v.outcome(Step::Resume), Some(&Outcome::Proven));
        assert!(v.usable);
    }

    #[test]
    fn resume_that_never_comes_back_fails_resume() {
        let mut f = Fake::new(&[Working, Idle]);
        f.after_send = [Working, Idle].into();
        f.after_resume = [Starting, Starting, Starting, Starting, Starting, Starting, Starting].into();
        f.agent_id = Some("sid-9".into());
        let v = validate(&mut f, "tool", &budget());
        assert!(
            matches!(v.outcome(Step::Resume), Some(Outcome::Failed(d)) if d.contains("never came up")),
            "{v:#?}"
        );
        assert!(matches!(v.outcome(Step::ResumeUsedSessionId), Some(Outcome::Unproven(d)) if d.contains("did not carry")));
        assert!(v.usable);
        assert!(!v.complete);
    }

    #[test]
    fn send_error_is_a_steer_failure_not_a_panic() {
        let mut f = Fake::new(&[Working, Idle]);
        f.send_err = Some("pane gone".into());
        f.after_resume = [Idle].into();
        let v = validate(&mut f, "tool", &budget());
        assert!(matches!(v.outcome(Step::Steer), Some(Outcome::Failed(d)) if d.contains("pane gone")));
        assert_eq!(v.outcome(Step::SteerIdle), Some(&Outcome::Skipped));
    }

    #[test]
    fn budget_clock_bounds_the_boot_wait() {
        // Starting forever: the boot budget (5s at 1s polls) ends the wait.
        let mut f = Fake::new(&[Starting; 50]);
        let start = f.clock;
        let v = validate(&mut f, "tool", &budget());
        assert!(
            f.clock.duration_since(start) <= Duration::from_secs(12),
            "the fake clock advanced only within the budgets"
        );
        assert!(matches!(v.outcome(Step::Idle), Some(Outcome::Failed(_))));
        assert!(v.elapsed_secs >= 5);
    }

    #[test]
    fn verdict_serializes_with_flat_outcomes() {
        let mut v = Verdict::default();
        v.set(Step::Launch, Outcome::Proven);
        v.set(Step::Idle, Outcome::Failed("x".into()));
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["proofs"][0]["step"], "launch");
        assert_eq!(j["proofs"][0]["outcome"], "proven");
        assert_eq!(j["proofs"][1]["outcome"], "failed");
        assert_eq!(j["proofs"][1]["detail"], "x");
        let back: Verdict = serde_json::from_value(j).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn step_labels_are_distinct() {
        let labels: std::collections::HashSet<&str> = Step::ALL.iter().map(|s| step_label(*s)).collect();
        assert_eq!(labels.len(), Step::ALL.len());
    }

    /// The real driver against the daemon's `fake-claude` fixture — needs
    /// tmux + curl; the fixture only posts hooks when a daemon address exists,
    /// so here it runs scrape-only (no `.flow-e2e-addr`), which is exactly the
    /// path a freshly drafted manifest takes. Its turn is instantaneous, so
    /// there is no working-only phrase to scrape: idle is the bare `>` prompt
    /// (tmux trims the trailing space; `(?m)` anchors per line).
    #[test]
    #[ignore = "live: launches tmux + the fake-claude fixture; run with --ignored"]
    fn engine_driver_runs_fake_claude_scrape_only() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../smooth-daemon/tests/fixtures/fake-claude");
        let fixture = fixture.canonicalize().unwrap();
        let toml = format!(
            "name = \"fakeclaude\"\n[binary]\nnames = [\"{}\"]\n[launch]\nargv = [\"--session-id\", \"{{session_id}}\", \"{{prompt}}\"]\nsession_id = \"preassigned\"\n[resume]\nargv = [\"--resume\", \"{{session_id}}\"]\nmode = \"resume_session\"\n[state]\nsource = \"scrape\"\n[state.scrape]\nidle = [\"(?m)^>\\\\s*$\"]\n",
            fixture.display()
        );
        let mut d = EngineDriver::private("fakeclaude", &toml, None).unwrap();
        let v = validate(
            &mut d,
            "fakeclaude",
            &Budget {
                boot: Duration::from_secs(15),
                turn: Duration::from_secs(15),
                poll: Duration::from_millis(500),
            },
        );
        eprintln!("{}", v.summary_lines().join("\n"));
        assert!(v.usable, "{v:#?}");
        assert_eq!(v.outcome(Step::Resume), Some(&Outcome::Proven));
        assert_eq!(v.outcome(Step::ResumeUsedSessionId), Some(&Outcome::Proven));
    }
}
