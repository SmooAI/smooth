//! Rotating phrases per workflow phase — the little "Pondering…"
//! strings the TUI cycles while the agent is working.
//!
//! Shown in the status bar alongside the phase name and current
//! upstream model, e.g.
//!
//!     ASSESS · smooth-thinking → kimi-k2-thinking  |  Pondering…
//!
//! Each phase has a small thesaurus; the TUI picks an index
//! (`phrase_idx % phrases.len()`) and cycles on the spinner tick
//! so long phases feel alive without spamming new events.
//!
//! When a phase isn't recognized (or is empty), `phrases_for` returns
//! a generic fallback list so the status bar never renders blank.

/// Canonical phase names emitted by the coding workflow.
/// Match exactly the `phase:` field of `AgentEvent::PhaseStart`.
pub const PHASES: &[&str] = &["ASSESS", "PLAN", "EXECUTE", "VERIFY", "REVIEW", "FINALIZE"];

/// Return the rotating phrase list for a phase. Always returns at
/// least one entry — callers can do `phrases[idx % len]` without
/// bounds-checking a possibly-empty slice.
pub fn phrases_for(phase: &str) -> &'static [&'static str] {
    match phase {
        "ASSESS" => &[
            "Pondering…",
            "Examining…",
            "Studying the spec…",
            "Grokking…",
            "Taking it in…",
            "Surveying…",
            "Reading between the lines…",
            "Mapping the terrain…",
        ],
        "PLAN" => &[
            "Plotting…",
            "Architecting…",
            "Drafting the approach…",
            "Mapping it out…",
            "Scheming…",
            "Strategizing…",
            "Sketching the blueprint…",
        ],
        "EXECUTE" => &[
            "Hammering…",
            "Forging…",
            "Crafting…",
            "Sculpting…",
            "Wrangling…",
            "Building…",
            "Weaving it together…",
            "Laying down code…",
        ],
        "VERIFY" => &[
            "Running the gauntlet…",
            "Stress-testing…",
            "Rolling the dice…",
            "Kicking the tires…",
            "Putting it through its paces…",
            "Watching the tests…",
        ],
        "REVIEW" => &[
            "Scrutinizing…",
            "Nitpicking…",
            "Second-guessing…",
            "Poking holes…",
            "Roasting…",
            "Cross-examining…",
            "Sharpening the critique…",
        ],
        "FINALIZE" => &[
            "Closing the loop…",
            "Dotting i's…",
            "One more glance…",
            "Wrapping up…",
            "Final pass…",
            "Settling the verdict…",
        ],
        _ => FALLBACK_PHRASES,
    }
}

const FALLBACK_PHRASES: &[&str] = &["Working…", "Thinking…", "Cooking…"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_canonical_phase_has_phrases() {
        for p in PHASES {
            let phrases = phrases_for(p);
            assert!(!phrases.is_empty(), "phase {p} should have phrases");
            assert!(phrases.iter().all(|s| !s.is_empty()), "no empty strings in {p}");
        }
    }

    #[test]
    fn unknown_phase_falls_back_rather_than_panics() {
        let phrases = phrases_for("UNKNOWN_PHASE_DISCO");
        assert!(!phrases.is_empty());
    }

    #[test]
    fn phrases_end_with_ellipsis_for_consistent_rendering() {
        for p in PHASES {
            for phrase in phrases_for(p) {
                assert!(phrase.ends_with('…'), "phrase for {p} should end with an ellipsis: {phrase:?}");
            }
        }
    }

    #[test]
    fn phrases_are_short_enough_for_status_bar() {
        // Status bar budget is ~40 chars for the phrase slot. Keep
        // phrases short enough that they don't truncate.
        for p in PHASES {
            for phrase in phrases_for(p) {
                assert!(
                    phrase.chars().count() <= 40,
                    "phrase too long for status bar ({} chars): {phrase:?}",
                    phrase.chars().count()
                );
            }
        }
    }

    #[test]
    fn cycling_is_deterministic_by_index() {
        let phrases = phrases_for("ASSESS");
        let n = phrases.len();
        assert_eq!(phrases[0 % n], phrases[n % n]);
        assert_eq!(phrases[1 % n], phrases[(n + 1) % n]);
    }
}
