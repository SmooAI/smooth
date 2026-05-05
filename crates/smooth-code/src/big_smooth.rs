//! Big Smooth — animated mob-boss mascot for the chat panel.
//!
//! Mirrors the web client's behaviour: he sits at the top-left of
//! the conversation when idle, then "flies down" to just below the
//! latest text when the agent starts thinking or streaming. Tiny
//! 4-row × 5-col ASCII sprite, body colour `SMOO_BLUE_400`, eyes
//! row swaps to express state.
//!
//! Driven entirely by `AppState` — no separate timer, no extra
//! events to plumb. The render loop calls `BigSmoothActor::update`
//! once per frame to advance animation, then calls
//! `BigSmoothActor::sprite_lines` to get the styled rows to draw.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme;

/// Snapshot of the bits of `AppState` Big Smooth cares about.
/// Pulling these into a small struct keeps `update` from needing
/// to borrow the full `AppState` (which would conflict with the
/// actor itself living inside it).
#[derive(Debug, Clone, Copy)]
pub struct BigSmoothInputs {
    pub thinking: bool,
    pub last_streaming: bool,
    pub any_tool_running: bool,
    pub any_tool_errored: bool,
}

impl BigSmoothInputs {
    pub fn from_state(state: &crate::state::AppState) -> Self {
        let last = state.messages.last();
        Self {
            thinking: state.thinking,
            last_streaming: last.is_some_and(|m| m.streaming),
            any_tool_running: last.is_some_and(|m| m.tool_calls.iter().any(|tc| matches!(tc.status, crate::state::ToolStatus::Running))),
            any_tool_errored: last.is_some_and(|m| m.tool_calls.iter().any(|tc| matches!(tc.status, crate::state::ToolStatus::Error))),
        }
    }
}

/// What Big Smooth is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BigSmoothState {
    /// Idle — sits at the top of the panel with a subtle bob/blink.
    Idle,
    /// Agent is processing (no streaming text yet).
    Thinking,
    /// A tool call is in flight — focused-eyes frame.
    Working,
    /// Streaming a response — mouth-flap alternation.
    Speaking,
    /// Just finished — happy-eyes flash, returns to Idle after a beat.
    Done,
    /// The last assistant turn errored — X-eyes flash, returns to Idle.
    Error,
}

/// Big Smooth's animation actor. Owned by `AppState`.
#[derive(Debug, Clone)]
pub struct BigSmoothActor {
    state: BigSmoothState,
    /// Monotonic frame counter — used for sub-state choices
    /// (idle bob, speaking mouth flap, working ellipsis offset).
    frame: u32,
    /// Frames remaining for transient states (Done, Error). When
    /// it hits zero we drop back to Idle.
    transient_remaining: u8,
    /// Last `thinking` flag we observed — used to detect rising
    /// edges into Thinking and falling edges into Done.
    last_thinking: bool,
    /// Last "any tool call running" flag — rising edge → Working.
    last_tool_running: bool,
    /// Last "any tool call errored" flag — rising edge → Error.
    last_tool_errored: bool,
}

impl BigSmoothActor {
    pub const fn new() -> Self {
        Self {
            state: BigSmoothState::Idle,
            frame: 0,
            transient_remaining: 0,
            last_thinking: false,
            last_tool_running: false,
            last_tool_errored: false,
        }
    }

    pub const fn state(&self) -> BigSmoothState {
        self.state
    }

    /// Advance the actor by one frame. Call once per render tick
    /// with a snapshot of the relevant `AppState` fields (build via
    /// `BigSmoothInputs::from_state`). Edge-triggered:
    /// - `thinking` rising/falling → enter Thinking / flash Done
    /// - tool call → Working (Running) or Error flash (Error)
    /// - streaming assistant message → Speaking
    pub fn update(&mut self, inputs: BigSmoothInputs) {
        self.frame = self.frame.wrapping_add(1);

        let any_tool_running = inputs.any_tool_running;
        let any_tool_errored = inputs.any_tool_errored;
        let streaming = inputs.last_streaming;
        let thinking = inputs.thinking;

        if self.transient_remaining > 0 {
            self.transient_remaining -= 1;
            if self.transient_remaining == 0 {
                self.state = BigSmoothState::Idle;
            }
        }

        // Edge-triggered: Error wins over Done wins over Working
        // wins over Speaking wins over Thinking wins over Idle.
        if any_tool_errored && !self.last_tool_errored {
            self.state = BigSmoothState::Error;
            self.transient_remaining = 20; // ~1s at 50ms ticks
        } else if self.last_thinking && !thinking && self.transient_remaining == 0 {
            // Falling edge of thinking with no error — celebrate briefly.
            self.state = BigSmoothState::Done;
            self.transient_remaining = 10; // ~500ms
        } else if self.transient_remaining == 0 {
            // Steady-state classification — pick the most-active label.
            self.state = if any_tool_running {
                BigSmoothState::Working
            } else if streaming {
                BigSmoothState::Speaking
            } else if thinking {
                BigSmoothState::Thinking
            } else {
                BigSmoothState::Idle
            };
        }

        self.last_thinking = thinking;
        self.last_tool_running = any_tool_running;
        self.last_tool_errored = any_tool_errored;
    }

    /// Whether Big Smooth should render at the BOTTOM of the chat
    /// (just below the latest text) vs the TOP. He sits at the top
    /// when idle and flies down when there's something to say or
    /// chew on.
    pub const fn anchored_at_bottom(&self) -> bool {
        !matches!(self.state, BigSmoothState::Idle)
    }

    /// Returns the current eyes row glyphs (5 chars wide). The
    /// Idle/Speaking states alternate sub-frames so he feels alive.
    fn eyes_row(&self) -> &'static str {
        match self.state {
            BigSmoothState::Idle => {
                // Slow bob: ~1 swap per second @ 50ms ticks (frame >> 4 = 16 frames apart).
                if (self.frame >> 4) & 1 == 0 {
                    "(•_•)"
                } else {
                    "(-_-)"
                }
            }
            BigSmoothState::Thinking => "(o_O)",
            BigSmoothState::Working => "(>_<)",
            BigSmoothState::Speaking => {
                // Quicker flap: ~3 swaps/sec.
                if (self.frame >> 2) & 1 == 0 {
                    "(•O•)"
                } else {
                    "(•_•)"
                }
            }
            BigSmoothState::Done => "(^_^)",
            BigSmoothState::Error => "(X_X)",
        }
    }

    /// Optional trailing "..." offset for Working state — scrolls
    /// one char/tick to the right of the suit row.
    fn trail(&self) -> Option<&'static str> {
        if !matches!(self.state, BigSmoothState::Working) {
            return None;
        }
        // Cycle through 4 phases of dot density.
        let phase = (self.frame >> 2) & 0b11;
        Some(match phase {
            0 => "   ",
            1 => ".  ",
            2 => ".. ",
            _ => "...",
        })
    }

    /// Returns the 4 styled rows representing the sprite. Always 4
    /// lines so layout doesn't reflow when the eyes change.
    pub fn sprite_lines(&self) -> Vec<Line<'static>> {
        let body = Style::default().fg(theme::SMOO_BLUE_400).add_modifier(Modifier::BOLD);
        let trail_style = theme::muted();
        let eyes = self.eyes_row();
        let trail = self.trail().unwrap_or("");

        vec![
            Line::from(Span::styled("  ___  ", body)),
            Line::from(Span::styled(" (===) ", body)),
            Line::from(Span::styled(format!(" {eyes} "), body)),
            Line::from(vec![Span::styled(" (_|_) ", body), Span::styled(format!(" {trail}"), trail_style)]),
        ]
    }
}

impl Default for BigSmoothActor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, ChatMessage, ToolCallState, ToolStatus};
    use std::path::PathBuf;

    fn fresh_state() -> AppState {
        AppState::new(PathBuf::from("/tmp"))
    }

    #[test]
    fn starts_idle() {
        let actor = BigSmoothActor::new();
        assert_eq!(actor.state(), BigSmoothState::Idle);
        assert!(!actor.anchored_at_bottom());
    }

    #[test]
    fn thinking_flag_drives_thinking_state() {
        let mut actor = BigSmoothActor::new();
        let mut s = fresh_state();
        s.thinking = true;
        actor.update(BigSmoothInputs::from_state(&s));
        assert_eq!(actor.state(), BigSmoothState::Thinking);
        assert!(actor.anchored_at_bottom());
    }

    #[test]
    fn streaming_message_drives_speaking() {
        let mut actor = BigSmoothActor::new();
        let mut s = fresh_state();
        let mut msg = ChatMessage::assistant("hello");
        msg.streaming = true;
        s.messages.push(msg);
        actor.update(BigSmoothInputs::from_state(&s));
        assert_eq!(actor.state(), BigSmoothState::Speaking);
    }

    #[test]
    fn running_tool_call_wins_over_speaking() {
        let mut actor = BigSmoothActor::new();
        let mut s = fresh_state();
        let mut msg = ChatMessage::assistant("");
        msg.streaming = true;
        let mut tc = ToolCallState::new("tc-1", "edit_file", &serde_json::json!({}));
        tc.status = ToolStatus::Running;
        msg.tool_calls.push(tc);
        s.messages.push(msg);
        actor.update(BigSmoothInputs::from_state(&s));
        assert_eq!(actor.state(), BigSmoothState::Working);
    }

    #[test]
    fn errored_tool_call_triggers_transient_error_flash() {
        let mut actor = BigSmoothActor::new();
        let mut s = fresh_state();
        let mut msg = ChatMessage::assistant("");
        let mut tc = ToolCallState::new("tc-1", "edit_file", &serde_json::json!({}));
        tc.status = ToolStatus::Error;
        msg.tool_calls.push(tc);
        s.messages.push(msg);
        actor.update(BigSmoothInputs::from_state(&s));
        assert_eq!(actor.state(), BigSmoothState::Error);
        s.messages.last_mut().unwrap().tool_calls.clear();
        for _ in 0..25 {
            actor.update(BigSmoothInputs::from_state(&s));
        }
        assert_eq!(actor.state(), BigSmoothState::Idle);
    }

    #[test]
    fn falling_edge_of_thinking_yields_done_flash() {
        let mut actor = BigSmoothActor::new();
        let mut s = fresh_state();
        s.thinking = true;
        actor.update(BigSmoothInputs::from_state(&s));
        assert_eq!(actor.state(), BigSmoothState::Thinking);
        s.thinking = false;
        actor.update(BigSmoothInputs::from_state(&s));
        assert_eq!(actor.state(), BigSmoothState::Done);
        for _ in 0..15 {
            actor.update(BigSmoothInputs::from_state(&s));
        }
        assert_eq!(actor.state(), BigSmoothState::Idle);
    }

    #[test]
    fn sprite_is_always_four_lines() {
        for st in [
            BigSmoothState::Idle,
            BigSmoothState::Thinking,
            BigSmoothState::Working,
            BigSmoothState::Speaking,
            BigSmoothState::Done,
            BigSmoothState::Error,
        ] {
            let actor = BigSmoothActor {
                state: st,
                frame: 0,
                transient_remaining: 0,
                last_thinking: false,
                last_tool_running: false,
                last_tool_errored: false,
            };
            assert_eq!(actor.sprite_lines().len(), 4, "state {st:?} must yield 4 sprite lines");
        }
    }

    #[test]
    fn idle_eyes_alternate_across_frames() {
        let mut actor = BigSmoothActor::new();
        actor.frame = 0;
        let a = actor.eyes_row();
        actor.frame = 16;
        let b = actor.eyes_row();
        assert_ne!(a, b, "idle bob should swap eyes between frames");
    }
}
