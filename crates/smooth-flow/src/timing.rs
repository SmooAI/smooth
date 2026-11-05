//! Stage timings of opening a session (SMOODEV-4267).
//!
//! Every stage between `flow.new` and an agent ready for input logs one line
//! at target [`TARGET`], `session` + `stage` + `ms`, so a slow open can be
//! read off the daemon log: `RUST_LOG=flow_timing=info` shows just these.
//! `docs/Architecture/SmoothFlow.md` (§ Opening a session) names the stages.

use std::time::Instant;

/// The tracing target of every stage line.
pub const TARGET: &str = "flow_timing";

/// Whole milliseconds since `since`, saturating.
#[must_use]
pub fn ms_since(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Log that `stage` of session `id` (`"-"` before the row exists) took from
/// `since` until now, and return now, ready to time the next stage.
pub fn stage(id: &str, stage: &str, since: Instant) -> Instant {
    tracing::info!(target: TARGET, session = %id, stage, ms = ms_since(since), "flow: open stage");
    Instant::now()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn ms_since_counts_whole_milliseconds() {
        let then = Instant::now().checked_sub(Duration::from_millis(25)).unwrap();
        assert!(ms_since(then) >= 25);
    }

    #[test]
    fn stage_hands_back_a_fresh_start() {
        let then = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();
        let next = stage("fs-1", "test", then);
        assert!(next > then);
        assert!(next.elapsed() < Duration::from_secs(5));
    }
}
