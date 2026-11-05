//! How much a row's state can be believed (SMOODEV-4268).
//!
//! Two questions a fleet view has to answer honestly, kept pure here so they
//! can be tested without a terminal:
//!
//! - **Reconcile.** A harness's hooks are the best evidence there is, but they
//!   are not complete: Claude Code and Codex fire no `Stop` when you press Esc,
//!   Codex fires nothing when you deny a prompt, and a resumed Codex says
//!   `SessionStart` and then nothing at all. A row that stopped reading its pane
//!   the moment one hook spoke stayed `working` or `starting` forever. So the
//!   pane may overrule a hook, but only on sustained evidence and never right
//!   after a hook spoke ([`reconcile`]).
//! - **Confidence.** A state nothing has confirmed for a while must not look
//!   live ([`confidence`]). `needs_you` is exempt on purpose: an agent waiting on
//!   a person keeps waiting however long the person takes, and it does not get
//!   greyed out for being patient.
//!
//! The engine gathers the evidence on each supervision pass ([`Track`]).

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use smooth_tmux::detect::PaneState;

use crate::store::SessionState;

/// A hook spoke this recently: the pane has no say. A hook can arrive before
/// the terminal repaints, so a pane read right after one is out of date.
pub const HOOK_GRACE: Duration = Duration::from_secs(4);
/// How long the pane must keep reading the same way before it overrules a
/// hook. One read can catch a frame between two screens.
pub const SUSTAIN: Duration = Duration::from_millis(1500);
/// A pane that says idle must also have held still this long to overrule a
/// hook's `working`. A busy harness animates; this is what tells a stale hook
/// from a footer that happens to match the idle pattern.
pub const QUIET: Duration = Duration::from_secs(4);
/// How long after an ask a pane change still counts as the prompt's own paint.
///
/// An ask whose prompt was never recognised on screen is only over once the
/// pane changes later than this: the hook usually beats that paint, and the
/// paint is not an answer.
pub const ASK_PAINT_GRACE: Duration = Duration::from_secs(3);
/// Default: `working` / `idle` with no confirmation for this long reads stale.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(120);
/// Default: `starting` with no evidence for this long reads unknown.
pub const DEFAULT_STARTING_AFTER: Duration = Duration::from_secs(45);

/// How far a row's state can be believed right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Recent evidence backs the state.
    #[default]
    Confirmed,
    /// The last evidence is old: this is what the row last was, not what it is.
    Stale,
    /// Nothing has ever backed it (`starting` with no sign of life).
    Unknown,
}

impl Confidence {
    /// The wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }
}

/// The windows [`confidence`] applies. On [`crate::EngineConfig`] so a test can
/// shrink them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusWindows {
    pub stale_after: Duration,
    pub starting_after: Duration,
}

impl Default for StatusWindows {
    fn default() -> Self {
        Self {
            stale_after: DEFAULT_STALE_AFTER,
            starting_after: DEFAULT_STARTING_AFTER,
        }
    }
}

/// How far to believe `state`, last confirmed `age` ago.
///
/// `needs_you`, `limited`, `done` and `dead` never fade: the first two are
/// waiting on something that does not expire, the last two are final.
#[must_use]
pub fn confidence(state: SessionState, age: Duration, w: StatusWindows) -> Confidence {
    match state {
        SessionState::Starting if age >= w.starting_after => Confidence::Unknown,
        SessionState::Working | SessionState::Idle if age >= w.stale_after => Confidence::Stale,
        _ => Confidence::Confirmed,
    }
}

/// What is known about the open ask of a `needs_you` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ask {
    pub at: Instant,
    /// The manifest recognised the prompt on screen at some point since.
    pub saw_prompt: bool,
}

/// One read of a row, for [`reconcile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub state: SessionState,
    pub verdict: PaneState,
    /// How long the pane has read `verdict`.
    pub sustained: Duration,
    /// How long the pane text has held still; `None` on the first look.
    pub quiet: Option<Duration>,
    /// Since the last hook; `None` if none spoke in this daemon's life.
    pub since_hook: Option<Duration>,
    /// Since the prompt was raised and whether it was recognised.
    pub ask: Option<AskReading>,
}

/// [`Ask`] as of one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AskReading {
    pub age: Duration,
    pub saw_prompt: bool,
}

/// The state the pane says this row is really in, when that evidence is strong
/// enough to overrule what the hooks last said. `None` keeps the row as it is.
///
/// An undecided pane (`Unknown`) never moves a row: honest doubt is reported
/// through [`confidence`], not by guessing a transition.
#[must_use]
pub fn reconcile(r: &Reading) -> Option<SessionState> {
    if r.since_hook.is_some_and(|d| d < HOOK_GRACE) || r.sustained < SUSTAIN {
        return None;
    }
    let quiet = r.quiet.unwrap_or_default();
    match (r.state, r.verdict) {
        (SessionState::Starting, PaneState::Idle) => Some(SessionState::Idle),
        (SessionState::Starting, PaneState::Working) => Some(SessionState::Working),
        (SessionState::Working, PaneState::Idle) if quiet >= QUIET => Some(SessionState::Idle),
        // Started moving, on a pane looked at before, and after the last hook
        // had settled: text painted around a `Stop` is that turn's last output,
        // and leaves a marker frozen on screen that is not work.
        (SessionState::Idle, PaneState::Working) if r.quiet.is_some_and(|q| q < QUIET) && r.since_hook.is_none_or(|d| quiet + HOOK_GRACE < d) => {
            Some(SessionState::Working)
        }
        (SessionState::NeedsYou, PaneState::Idle | PaneState::Working) => {
            let ask = r.ask?;
            // Recognised on screen and now gone: answered. Never recognised:
            // the pane has to have moved on after the prompt's own paint, or
            // a prompt this manifest cannot read would be cleared unanswered.
            let moved_on = ask.age.saturating_sub(quiet) > ASK_PAINT_GRACE;
            (ask.saw_prompt || moved_on).then_some(if r.verdict == PaneState::Working {
                SessionState::Working
            } else {
                SessionState::Idle
            })
        }
        _ => None,
    }
}

/// The evidence the engine keeps per session, in memory: after a restart the
/// row's `updated_at` stands in until the next supervision pass.
#[derive(Debug, Clone, Default)]
pub struct Track {
    verdict: Option<(PaneState, Instant)>,
    /// Hash of the pane without spinner frames, and when that last changed.
    solid: Option<(u64, Instant)>,
    last_hook: Option<Instant>,
    confirmed: Option<Instant>,
    /// When `confirmed` was last true on the wall clock, for clients.
    confirmed_at: Option<chrono::DateTime<chrono::Utc>>,
    ask: Option<Ask>,
    /// What clients were last told, so a change can be broadcast once.
    pub published: Confidence,
}

impl Track {
    /// A hook spoke: it is the best evidence of life there is.
    pub fn note_hook(&mut self, now: Instant) {
        self.last_hook = Some(now);
        self.confirm(now);
    }

    /// The state moved, or something else proved it just now.
    pub fn confirm(&mut self, now: Instant) {
        self.confirmed = Some(now);
        self.confirmed_at = Some(chrono::Utc::now());
    }

    /// The row entered (`true`) or left (`false`) `needs_you`.
    pub fn set_asking(&mut self, asking: bool, now: Instant) {
        match (asking, &self.ask) {
            (true, None) => self.ask = Some(Ask { at: now, saw_prompt: false }),
            (false, _) => self.ask = None,
            (true, Some(_)) => {}
        }
    }

    /// Record one pane read; returns how long the pane has read this way.
    pub fn observe(&mut self, verdict: PaneState, now: Instant) -> Duration {
        if verdict == PaneState::AwaitingApproval {
            if let Some(a) = &mut self.ask {
                a.saw_prompt = true;
            }
        }
        match self.verdict {
            Some((v, since)) if v == verdict => now.saturating_duration_since(since),
            _ => {
                self.verdict = Some((verdict, now));
                Duration::ZERO
            }
        }
    }

    /// How long the pane's text has held still, ignoring spinner frames: Codex
    /// leaves a spinner turning in its status line after an interrupted turn,
    /// so a turning spinner alone is no sign of work. `None` on the first look.
    pub fn observe_text(&mut self, text: &str, now: Instant) -> Option<Duration> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for c in text.chars().filter(|c| !('\u{2800}'..='\u{28ff}').contains(c)) {
            c.hash(&mut h);
        }
        let hash = h.finish();
        match &mut self.solid {
            Some((prev, changed)) if *prev == hash => Some(now.saturating_duration_since(*changed)),
            Some(entry) => {
                *entry = (hash, now);
                Some(Duration::ZERO)
            }
            None => {
                self.solid = Some((hash, now));
                None
            }
        }
    }

    /// What the pane just showed for `state`. A busy harness animates, so
    /// `working` is backed by the pane changing and not by a marker that could
    /// be frozen on screen; `idle` is backed by a pane that has not started
    /// claiming to be busy.
    pub fn confirm_from_pane(&mut self, state: SessionState, verdict: PaneState, quiet: Option<Duration>, now: Instant) {
        let proven = match state {
            SessionState::Working => quiet.is_some_and(|q| q.is_zero()),
            SessionState::Idle => matches!(verdict, PaneState::Idle | PaneState::Unknown),
            SessionState::Starting => matches!(verdict, PaneState::Working | PaneState::Idle),
            _ => false,
        };
        if proven {
            self.confirm(now);
        }
    }

    /// Since the last hook, for [`Reading::since_hook`].
    #[must_use]
    pub fn since_hook(&self, now: Instant) -> Option<Duration> {
        self.last_hook.map(|h| now.saturating_duration_since(h))
    }

    /// The open ask as of `now`, for [`Reading::ask`].
    #[must_use]
    pub fn ask(&self, now: Instant) -> Option<AskReading> {
        self.ask.map(|a| AskReading {
            age: now.saturating_duration_since(a.at),
            saw_prompt: a.saw_prompt,
        })
    }

    /// Since the state was last confirmed; `None` if not in this daemon's life.
    #[must_use]
    pub fn age(&self, now: Instant) -> Option<Duration> {
        self.confirmed.map(|c| now.saturating_duration_since(c))
    }

    /// The wall-clock time of the last confirmation.
    #[must_use]
    pub const fn confirmed_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.confirmed_at
    }

    /// A relaunch starts over: what the old process's pane said is not
    /// evidence about the new one.
    pub fn restart(&mut self, now: Instant) {
        self.verdict = None;
        self.solid = None;
        self.last_hook = None;
        self.ask = None;
        self.confirm(now);
    }
}

/// `Claude Code`'s `AskUserQuestion` reaches the hook as a permission request,
/// but it is a question. The question text, if `payload` carries one.
#[must_use]
pub fn asked_question(payload: &serde_json::Value) -> Option<String> {
    let tool = payload.get("tool_name").and_then(serde_json::Value::as_str)?;
    if !tool.eq_ignore_ascii_case("AskUserQuestion") {
        return None;
    }
    let text = payload
        .pointer("/tool_input/questions/0/question")
        .or_else(|| payload.pointer("/tool_input/question"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| "a question is waiting for you".to_string(), str::to_string);
    Some(text)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;

    const SECS: fn(u64) -> Duration = Duration::from_secs;

    fn reading(state: SessionState, verdict: PaneState) -> Reading {
        Reading {
            state,
            verdict,
            sustained: SECS(10),
            quiet: Some(SECS(10)),
            since_hook: Some(SECS(10)),
            ask: None,
        }
    }

    #[test]
    fn a_stuck_starting_row_follows_the_pane_even_after_a_hook() {
        assert_eq!(reconcile(&reading(SessionState::Starting, PaneState::Idle)), Some(SessionState::Idle));
        assert_eq!(reconcile(&reading(SessionState::Starting, PaneState::Working)), Some(SessionState::Working));
        assert_eq!(reconcile(&reading(SessionState::Starting, PaneState::Unknown)), None);
    }

    #[test]
    fn an_interrupted_turn_with_no_stop_hook_goes_idle_once_the_pane_is_still() {
        let mut r = reading(SessionState::Working, PaneState::Idle);
        assert_eq!(reconcile(&r), Some(SessionState::Idle));
        r.quiet = Some(QUIET.checked_sub(Duration::from_millis(1)).unwrap());
        assert_eq!(reconcile(&r), None, "an idle-looking footer on a pane still changing is not a stopped turn");
        r.quiet = None;
        assert_eq!(reconcile(&r), None, "the first look has no quiet clock");
    }

    #[test]
    fn work_nobody_reported_needs_a_pane_that_is_moving() {
        let mut r = reading(SessionState::Idle, PaneState::Working);
        assert_eq!(reconcile(&r), None, "a frozen marker on a still pane is not work");
        // Painted just after the Stop, then frozen, with the clock lagging the
        // hook by a tick: past the grace, but it moved inside it.
        r.quiet = Some(SECS(3));
        r.since_hook = Some(SECS(5));
        assert_eq!(reconcile(&r), None, "the turn's last paint around a Stop is not new work");
        r.quiet = None;
        r.since_hook = None;
        assert_eq!(reconcile(&r), None, "the first look after a restart has seen nothing move");
        r.quiet = Some(Duration::ZERO);
        r.since_hook = Some(SECS(6));
        assert_eq!(reconcile(&r), Some(SessionState::Working));
    }

    #[test]
    fn a_hook_that_just_spoke_outranks_the_pane() {
        for state in [SessionState::Starting, SessionState::Working, SessionState::NeedsYou] {
            let mut r = reading(state, PaneState::Idle);
            r.ask = Some(AskReading {
                age: SECS(60),
                saw_prompt: true,
            });
            r.since_hook = Some(HOOK_GRACE.checked_sub(Duration::from_millis(1)).unwrap());
            assert_eq!(reconcile(&r), None, "{state}");
            r.since_hook = Some(HOOK_GRACE);
            assert!(reconcile(&r).is_some(), "{state}");
        }
    }

    #[test]
    fn one_frame_is_not_sustained_evidence() {
        let mut r = reading(SessionState::Working, PaneState::Idle);
        r.sustained = SUSTAIN.checked_sub(Duration::from_millis(1)).unwrap();
        assert_eq!(reconcile(&r), None);
    }

    #[test]
    fn a_recognised_prompt_that_left_the_screen_was_answered() {
        for (verdict, want) in [(PaneState::Idle, SessionState::Idle), (PaneState::Working, SessionState::Working)] {
            let mut r = reading(SessionState::NeedsYou, verdict);
            r.ask = Some(AskReading {
                age: SECS(1),
                saw_prompt: true,
            });
            assert_eq!(reconcile(&r), Some(want));
        }
    }

    #[test]
    fn a_prompt_the_manifest_cannot_read_stays_needs_you_until_the_pane_moves() {
        let mut r = reading(SessionState::NeedsYou, PaneState::Idle);
        r.ask = Some(AskReading {
            age: SECS(600),
            saw_prompt: false,
        });
        r.quiet = Some(SECS(599));
        assert_eq!(reconcile(&r), None, "painted right after the ask, then nothing: still waiting");
        r.quiet = Some(SECS(10));
        assert_eq!(reconcile(&r), Some(SessionState::Idle), "the pane moved long after the ask");
    }

    #[test]
    fn needs_you_on_screen_or_undecided_never_clears() {
        for verdict in [PaneState::AwaitingApproval, PaneState::Unknown, PaneState::Errored, PaneState::UsageLimit] {
            let mut r = reading(SessionState::NeedsYou, verdict);
            r.ask = Some(AskReading {
                age: SECS(600),
                saw_prompt: true,
            });
            assert_eq!(reconcile(&r), None, "{verdict:?}");
        }
    }

    #[test]
    fn needs_you_with_no_known_ask_does_not_clear() {
        assert_eq!(reconcile(&reading(SessionState::NeedsYou, PaneState::Idle)), None);
    }

    #[test]
    fn final_and_limit_states_are_not_reconciled() {
        for state in [SessionState::Done, SessionState::Dead, SessionState::Limited] {
            for verdict in [PaneState::Idle, PaneState::Working] {
                assert_eq!(reconcile(&reading(state, verdict)), None);
            }
        }
    }

    #[test]
    fn only_working_idle_and_starting_fade() {
        let w = StatusWindows {
            stale_after: SECS(10),
            starting_after: SECS(20),
        };
        assert_eq!(confidence(SessionState::Working, SECS(9), w), Confidence::Confirmed);
        assert_eq!(confidence(SessionState::Working, SECS(10), w), Confidence::Stale);
        assert_eq!(confidence(SessionState::Idle, SECS(10), w), Confidence::Stale);
        assert_eq!(confidence(SessionState::Starting, SECS(19), w), Confidence::Confirmed);
        assert_eq!(confidence(SessionState::Starting, SECS(20), w), Confidence::Unknown);
        let forever = SECS(10 * 24 * 3600);
        for state in [SessionState::NeedsYou, SessionState::Limited, SessionState::Done, SessionState::Dead] {
            assert_eq!(confidence(state, forever, w), Confidence::Confirmed, "{state}");
        }
    }

    #[test]
    fn track_times_a_verdict_from_its_first_read() {
        let t0 = Instant::now();
        let mut t = Track::default();
        assert_eq!(t.observe(PaneState::Idle, t0), Duration::ZERO);
        assert_eq!(t.observe(PaneState::Idle, t0 + SECS(3)), SECS(3));
        assert_eq!(t.observe(PaneState::Working, t0 + SECS(4)), Duration::ZERO);
    }

    #[test]
    fn a_turning_spinner_is_not_a_changing_pane() {
        let t0 = Instant::now();
        let mut t = Track::default();
        assert_eq!(t.observe_text("idle ⠦", t0), None);
        assert_eq!(t.observe_text("idle ⠇", t0 + SECS(5)), Some(SECS(5)));
        assert_eq!(t.observe_text("idle more ⠇", t0 + SECS(6)), Some(Duration::ZERO));
        assert_eq!(t.observe_text("idle more ⠦", t0 + SECS(7)), Some(SECS(1)));
    }

    #[test]
    fn track_remembers_an_ask_was_seen_on_screen_until_it_ends() {
        let t0 = Instant::now();
        let mut t = Track::default();
        t.observe(PaneState::AwaitingApproval, t0);
        assert_eq!(t.ask(t0), None, "no ask open: nothing to remember");
        t.set_asking(true, t0);
        assert!(!t.ask(t0).unwrap().saw_prompt);
        t.observe(PaneState::AwaitingApproval, t0 + SECS(1));
        t.set_asking(true, t0 + SECS(5));
        let ask = t.ask(t0 + SECS(6)).unwrap();
        assert!(ask.saw_prompt);
        assert_eq!(ask.age, SECS(6), "a repeated ask is the same ask");
        t.set_asking(false, t0 + SECS(7));
        assert_eq!(t.ask(t0 + SECS(7)), None);
    }

    #[test]
    fn what_the_pane_can_prove_depends_on_the_state() {
        let t0 = Instant::now();
        let proven = |state, verdict, quiet| {
            let mut t = Track::default();
            t.confirm_from_pane(state, verdict, quiet, t0);
            t.age(t0).is_some()
        };
        assert!(proven(SessionState::Working, PaneState::Working, Some(Duration::ZERO)));
        assert!(
            !proven(SessionState::Working, PaneState::Working, Some(SECS(5))),
            "a frozen spinner proves nothing"
        );
        assert!(!proven(SessionState::Working, PaneState::Working, None));
        assert!(proven(SessionState::Idle, PaneState::Idle, Some(SECS(500))));
        assert!(proven(SessionState::Idle, PaneState::Unknown, Some(SECS(500))));
        assert!(!proven(SessionState::Idle, PaneState::Working, Some(SECS(500))));
        assert!(proven(SessionState::Starting, PaneState::Idle, None));
        assert!(
            !proven(SessionState::Starting, PaneState::Unknown, Some(Duration::ZERO)),
            "a splash is not a sign of readiness"
        );
        assert!(!proven(SessionState::NeedsYou, PaneState::Idle, None));
    }

    #[test]
    fn a_relaunch_forgets_the_old_process() {
        let t0 = Instant::now();
        let mut t = Track::default();
        t.note_hook(t0);
        t.set_asking(true, t0);
        t.observe(PaneState::Idle, t0);
        t.restart(t0 + SECS(1));
        assert_eq!(t.since_hook(t0 + SECS(2)), None);
        assert_eq!(t.ask(t0 + SECS(2)), None);
        assert_eq!(t.observe(PaneState::Idle, t0 + SECS(2)), Duration::ZERO);
        assert_eq!(t.age(t0 + SECS(2)), Some(SECS(1)));
        assert!(t.confirmed_at().is_some());
    }

    #[test]
    fn ask_user_question_is_a_question_with_its_text() {
        let q = serde_json::json!({"tool_name": "AskUserQuestion", "tool_input": {"questions": [{"question": "Which one?"}]}});
        assert_eq!(asked_question(&q).as_deref(), Some("Which one?"));
        let bare = serde_json::json!({"tool_name": "AskUserQuestion"});
        assert_eq!(asked_question(&bare).as_deref(), Some("a question is waiting for you"));
        assert_eq!(asked_question(&serde_json::json!({"tool_name": "Bash"})), None);
        assert_eq!(asked_question(&serde_json::json!({})), None);
    }

    #[test]
    fn confidence_reads_as_its_wire_string() {
        for c in [Confidence::Confirmed, Confidence::Stale, Confidence::Unknown] {
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
    }
}
