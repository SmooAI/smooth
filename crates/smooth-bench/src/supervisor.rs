//! Supervisor for bench dispatches (pearl th-5870f8).
//!
//! Watches a running teammate's pearl-comment stream and posts
//! `[STEERING:GUIDANCE]` hints when the operator stalls or repeats
//! the same failure. Replaces the one-shot fire-and-forget bench
//! pattern with a multi-turn coach.
//!
//! Two kinds of intervention land in v1:
//!
//! 1. **Stall** — no new comment for `idle_threshold_s`. Hint:
//!    "post a [PROGRESS] update or call teammate_wait if you're
//!    waiting on a long tool call".
//! 2. **Repeat-failure** — the same error-shaped comment posted
//!    `repeat_failure_threshold` times. Hint quotes the error and
//!    asks the operator to step back.
//!
//! Both heuristics are rule-based so v1 doesn't take a hard LLM
//! dependency; an LLM-driven coach can replace `Supervisor::tick`
//! later by reading the same `Observation` shape and emitting
//! `Intervention` records. See module test for the contract.

use std::time::{Duration, Instant};

use smooth_pearls::{PearlComment, PearlStore};

/// Configuration for the supervisor heartbeat.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// Seconds without a new comment before we treat the operator
    /// as stalled.
    pub idle_threshold_s: u64,
    /// Number of identical error-shaped comments in a row that
    /// triggers a focused-fix nudge.
    pub repeat_failure_threshold: usize,
    /// Polling interval. The bench harness drives `tick` directly,
    /// so this is informational — runners can use it to sleep
    /// between ticks.
    pub poll_interval_s: u64,
    /// Cooldown after a STEERING comment is posted before another
    /// can fire. Prevents the supervisor from spamming the pearl
    /// when the operator's reaction takes a few seconds.
    pub steering_cooldown_s: u64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            idle_threshold_s: 90,
            repeat_failure_threshold: 3,
            poll_interval_s: 10,
            steering_cooldown_s: 30,
        }
    }
}

/// What the supervisor decided to do this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intervention {
    /// Nothing to do — operator is making progress or recently
    /// steered.
    None,
    /// Operator hasn't posted in `idle_threshold_s`. Body is the
    /// guidance text the supervisor will post (or already posted).
    Stall { body: String },
    /// Operator posted the same error-shape `n` times in a row.
    /// Body quotes the failure for the operator to focus on.
    RepeatFailure { body: String, occurrences: usize },
}

/// Snapshot of pearl comments + timestamps that drives `Supervisor::tick`.
/// Pulled out as a struct so tests can drive the rule engine without
/// a real pearl store.
#[derive(Debug, Clone)]
pub struct Observation {
    /// Wall-clock at the moment of observation. Used to compute
    /// idle elapsed against the latest comment timestamp.
    pub now: Instant,
    /// Comments in chronological (oldest-first) order.
    pub comments: Vec<PearlComment>,
}

/// Supervisor state. Carries cooldown trackers between ticks.
pub struct Supervisor {
    cfg: SupervisorConfig,
    /// When the last STEERING was posted. `None` until the first
    /// intervention fires.
    last_steering: Option<Instant>,
    /// Ratchet on comment count — only count consecutive identical
    /// failures from this index forward, so old failures don't keep
    /// re-firing the heuristic.
    failure_ratchet: usize,
}

impl Supervisor {
    #[must_use]
    pub fn new(cfg: SupervisorConfig) -> Self {
        Self {
            cfg,
            last_steering: None,
            failure_ratchet: 0,
        }
    }

    /// Drive one tick. Returns the chosen `Intervention` so the
    /// caller can persist it via the pearl store. The supervisor
    /// records the cooldown internally — call `confirm_posted` when
    /// the comment lands so subsequent ticks don't re-fire too
    /// quickly.
    pub fn tick(&self, obs: &Observation) -> Intervention {
        if self.in_cooldown(obs.now) {
            return Intervention::None;
        }

        // Stall: no comment recently OR no comment ever.
        if let Some(last_age) = self.idle_age(obs) {
            if last_age >= Duration::from_secs(self.cfg.idle_threshold_s) {
                return Intervention::Stall {
                    body: format!(
                        "[STEERING:GUIDANCE] You haven't posted in {}s. Post a [PROGRESS] update with what you're working on, or if you're waiting on a long tool call, call teammate_wait so the supervisor knows you're alive.",
                        last_age.as_secs()
                    ),
                };
            }
        }

        // Repeat-failure: scan from the ratchet forward for runs
        // of identical error-shaped comments.
        if let Some((body, occurrences)) = self.detect_repeat_failure(obs) {
            return Intervention::RepeatFailure { body, occurrences };
        }

        Intervention::None
    }

    /// Acknowledge that the supervisor's intervention was posted.
    /// Records the cooldown timestamp and (for repeat-failure) bumps
    /// the ratchet so the same run doesn't re-trigger.
    pub fn confirm_posted(&mut self, obs: &Observation, intervention: &Intervention) {
        match intervention {
            Intervention::None => {}
            Intervention::Stall { .. } => {
                self.last_steering = Some(obs.now);
            }
            Intervention::RepeatFailure { .. } => {
                self.last_steering = Some(obs.now);
                // Ratchet so the same failure run doesn't re-fire
                // on the next tick.
                self.failure_ratchet = obs.comments.len();
            }
        }
    }

    fn in_cooldown(&self, now: Instant) -> bool {
        let Some(last) = self.last_steering else {
            return false;
        };
        now.duration_since(last) < Duration::from_secs(self.cfg.steering_cooldown_s)
    }

    fn idle_age(&self, obs: &Observation) -> Option<Duration> {
        // We only have the comment list — there's no per-comment
        // Instant captured here. Approximate: idle since the
        // observation start, falling back to "infinitely idle"
        // when there are no comments at all (which the caller
        // treats as a stall).
        if obs.comments.is_empty() {
            return Some(Duration::from_secs(u64::MAX / 2));
        }
        // Use chrono::Utc::now - comment.created_at as a proxy.
        let last = obs.comments.last()?;
        let now_utc = chrono::Utc::now();
        let elapsed = now_utc.signed_duration_since(last.created_at);
        let secs = elapsed.num_seconds();
        if secs <= 0 {
            return Some(Duration::ZERO);
        }
        u64::try_from(secs).ok().map(Duration::from_secs)
    }

    fn detect_repeat_failure(&self, obs: &Observation) -> Option<(String, usize)> {
        let comments = obs.comments.get(self.failure_ratchet..)?;
        if comments.len() < self.cfg.repeat_failure_threshold {
            return None;
        }
        // Scan trailing window for runs of identical error-shaped
        // comments. We only care about the most-recent run.
        let mut run = 1usize;
        let mut last_err: Option<&str> = None;
        for c in comments.iter().rev() {
            if !is_error_shape(&c.content) {
                break;
            }
            match last_err {
                None => {
                    last_err = Some(&c.content);
                }
                Some(prev) => {
                    if c.content == prev {
                        run += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        let body = last_err?;
        if run >= self.cfg.repeat_failure_threshold {
            let quoted: String = body.chars().take(240).collect();
            let hint = format!(
                "[STEERING:GUIDANCE] You've reported the same failure {run} times in a row:\n> {quoted}\nStep back and look at the failure mode — re-read the relevant file, run a smaller scoped reproducer, or call teammate_message to ask for help if you're stuck."
            );
            Some((hint, run))
        } else {
            None
        }
    }
}

/// True when a pearl comment looks like a failure report — the
/// supervisor uses this to scope the repeat-failure heuristic.
/// Liberal on shape so we catch operator-formatted failures and
/// raw tool errors. The only requirement is that the supervisor
/// not match `[STEERING:*]` comments it posts itself, since that
/// would create an infinite loop.
fn is_error_shape(content: &str) -> bool {
    let trimmed = content.trim_start();
    if trimmed.starts_with("[STEERING:") {
        return false;
    }
    let lower = trimmed.to_lowercase();
    lower.starts_with("[error]")
        || lower.starts_with("[fail]")
        || lower.starts_with("error:")
        || lower.contains("test failed")
        || lower.contains("compile error")
        || lower.contains("panicked")
}

/// Async helper: pull the latest comments via `PearlStore` into
/// an `Observation` so a runner can call `Supervisor::tick` against
/// a real pearl. Pure read-only — never mutates the store.
pub fn observe(store: &PearlStore, pearl_id: &str) -> anyhow::Result<Observation> {
    let comments = store.get_comments(pearl_id)?;
    Ok(Observation { now: Instant::now(), comments })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn comment(id: &str, content: &str, ago_secs: i64) -> PearlComment {
        let now = Utc::now();
        let created_at = now - chrono::Duration::seconds(ago_secs);
        PearlComment {
            id: id.into(),
            pearl_id: "test-pearl".into(),
            content: content.into(),
            created_at,
        }
    }

    #[test]
    fn stall_detected_when_idle_threshold_exceeded() {
        let cfg = SupervisorConfig {
            idle_threshold_s: 60,
            poll_interval_s: 5,
            repeat_failure_threshold: 3,
            steering_cooldown_s: 30,
        };
        let sup = Supervisor::new(cfg);
        let obs = Observation {
            now: Instant::now(),
            comments: vec![comment("c1", "[PROGRESS] starting", 120)],
        };
        match sup.tick(&obs) {
            Intervention::Stall { body } => {
                assert!(body.contains("STEERING:GUIDANCE"));
                assert!(body.contains("teammate_wait"));
            }
            other => panic!("expected Stall, got {other:?}"),
        }
    }

    #[test]
    fn no_stall_when_recent_comment() {
        let cfg = SupervisorConfig {
            idle_threshold_s: 60,
            poll_interval_s: 5,
            repeat_failure_threshold: 3,
            steering_cooldown_s: 30,
        };
        let sup = Supervisor::new(cfg);
        let obs = Observation {
            now: Instant::now(),
            comments: vec![comment("c1", "[PROGRESS] working", 5)],
        };
        assert_eq!(sup.tick(&obs), Intervention::None);
    }

    #[test]
    fn empty_comments_treated_as_stall() {
        let sup = Supervisor::new(SupervisorConfig::default());
        let obs = Observation {
            now: Instant::now(),
            comments: vec![],
        };
        assert!(matches!(sup.tick(&obs), Intervention::Stall { .. }));
    }

    #[test]
    fn repeat_failure_fires_after_threshold() {
        let cfg = SupervisorConfig {
            idle_threshold_s: 86400, // disabled for this test
            repeat_failure_threshold: 3,
            poll_interval_s: 5,
            steering_cooldown_s: 30,
        };
        let sup = Supervisor::new(cfg);
        let err = "[ERROR] cargo test failed: 1 tests failed";
        let obs = Observation {
            now: Instant::now(),
            comments: vec![comment("c1", err, 5), comment("c2", err, 4), comment("c3", err, 3)],
        };
        match sup.tick(&obs) {
            Intervention::RepeatFailure { body, occurrences } => {
                assert_eq!(occurrences, 3);
                assert!(body.contains("Step back"));
                assert!(body.contains("cargo test failed"));
            }
            other => panic!("expected RepeatFailure, got {other:?}"),
        }
    }

    #[test]
    fn repeat_failure_skips_distinct_errors() {
        let sup = Supervisor::new(SupervisorConfig {
            idle_threshold_s: 86400,
            ..SupervisorConfig::default()
        });
        let obs = Observation {
            now: Instant::now(),
            comments: vec![
                comment("c1", "[ERROR] thing A failed", 5),
                comment("c2", "[ERROR] thing B failed", 4),
                comment("c3", "[ERROR] thing C failed", 3),
            ],
        };
        // Three errors but all distinct — no repeat-failure.
        assert_eq!(sup.tick(&obs), Intervention::None);
    }

    #[test]
    fn repeat_failure_does_not_match_steering_comments() {
        let sup = Supervisor::new(SupervisorConfig {
            idle_threshold_s: 86400,
            ..SupervisorConfig::default()
        });
        // Three of the supervisor's own STEERING comments. is_error_shape
        // must reject these or we'd fire on our own posts.
        let body = "[STEERING:GUIDANCE] You haven't posted in 90s.";
        let obs = Observation {
            now: Instant::now(),
            comments: vec![comment("c1", body, 5), comment("c2", body, 4), comment("c3", body, 3)],
        };
        assert_eq!(sup.tick(&obs), Intervention::None);
    }

    #[test]
    fn cooldown_blocks_back_to_back_steering() {
        let cfg = SupervisorConfig {
            idle_threshold_s: 1,
            steering_cooldown_s: 60,
            poll_interval_s: 5,
            repeat_failure_threshold: 3,
        };
        let mut sup = Supervisor::new(cfg);
        let obs = Observation {
            now: Instant::now(),
            comments: vec![comment("c1", "[PROGRESS] old", 30)],
        };
        let first = sup.tick(&obs);
        assert!(matches!(first, Intervention::Stall { .. }));
        sup.confirm_posted(&obs, &first);

        // Second tick within the cooldown window — must return None.
        let obs2 = Observation {
            now: Instant::now(),
            comments: vec![comment("c1", "[PROGRESS] old", 30)],
        };
        assert_eq!(sup.tick(&obs2), Intervention::None);
    }

    #[test]
    fn ratchet_prevents_re_firing_on_same_failure_run() {
        let cfg = SupervisorConfig {
            idle_threshold_s: 86400,
            repeat_failure_threshold: 3,
            steering_cooldown_s: 0,
            poll_interval_s: 5,
        };
        let mut sup = Supervisor::new(cfg);
        let err = "[ERROR] same failure";
        let obs = Observation {
            now: Instant::now(),
            comments: vec![comment("c1", err, 5), comment("c2", err, 4), comment("c3", err, 3)],
        };
        let first = sup.tick(&obs);
        assert!(matches!(first, Intervention::RepeatFailure { .. }));
        sup.confirm_posted(&obs, &first);

        // Same observation again — must NOT re-fire because the
        // ratchet now sits past the failure run.
        assert_eq!(sup.tick(&obs), Intervention::None);
    }

    #[test]
    fn is_error_shape_recognises_common_formats() {
        assert!(is_error_shape("[ERROR] something blew up"));
        assert!(is_error_shape("[FAIL] tests"));
        assert!(is_error_shape("Error: panic in foo"));
        assert!(is_error_shape("the test failed at 12 of 20"));
        assert!(is_error_shape("compile error in main.rs"));
        assert!(is_error_shape("thread 'main' panicked at line 42"));

        assert!(!is_error_shape("[PROGRESS] all good"));
        assert!(!is_error_shape("[IDLE]"));
        assert!(!is_error_shape("[STEERING:GUIDANCE] post a progress note"));
    }

    #[test]
    fn observation_struct_does_not_require_real_store() {
        // Sanity: Observation can be built from arbitrary comments
        // without a store, so an LLM-driven supervisor implementation
        // can replace tick() while keeping the same wire shape.
        let obs = Observation {
            now: Instant::now(),
            comments: vec![PearlComment {
                id: "x".into(),
                pearl_id: "test-pearl".into(),
                content: "test".into(),
                created_at: Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap(),
            }],
        };
        assert_eq!(obs.comments.len(), 1);
    }
}
