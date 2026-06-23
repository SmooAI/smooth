//! Scheduled (proactive) tasks for the always-on agent.
//!
//! A [`Schedule`] re-enters a prompt into the daemon on a cadence — the
//! hermes-style "do this every morning / every N minutes" capability. This
//! module is the **pure model + next-fire-time computation**: no storage, no
//! tick loop (those land in following slices), so the timing logic is
//! exhaustively unit-testable without a clock or a database.

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// How often a scheduled task fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleKind {
    /// Fire every `secs` seconds (minimum 1).
    EveryNSeconds { secs: u64 },
    /// Fire once per day at the given **UTC** `hour`:`minute`.
    DailyAt { hour: u8, minute: u8 },
}

impl ScheduleKind {
    /// The next fire time **strictly after** `after`.
    #[must_use]
    pub fn next_after(self, after: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Self::EveryNSeconds { secs } => after + Duration::seconds(i64::try_from(secs.max(1)).unwrap_or(i64::MAX)),
            Self::DailyAt { hour, minute } => {
                let h = u32::from(hour.min(23));
                let m = u32::from(minute.min(59));
                // Candidate today at h:m:00 UTC.
                let naive = after.date_naive().and_hms_opt(h, m, 0).unwrap_or_else(|| after.naive_utc());
                let candidate = Utc.from_utc_datetime(&naive);
                if candidate > after {
                    candidate
                } else {
                    candidate + Duration::days(1)
                }
            }
        }
    }
}

/// A scheduled task: a prompt fired on a [`ScheduleKind`] cadence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Stable id.
    pub id: String,
    /// The prompt re-entered into the daemon when this fires.
    pub prompt: String,
    /// The cadence.
    pub kind: ScheduleKind,
    /// Whether it's active (a disabled schedule never fires).
    pub enabled: bool,
    /// The next time this should fire.
    pub next_due: DateTime<Utc>,
    /// When it last fired, if ever.
    pub last_run: Option<DateTime<Utc>>,
}

impl Schedule {
    /// Create a new enabled schedule, first due at the next cadence point after
    /// `now`.
    #[must_use]
    pub fn new(id: impl Into<String>, prompt: impl Into<String>, kind: ScheduleKind, now: DateTime<Utc>) -> Self {
        Self {
            id: id.into(),
            prompt: prompt.into(),
            kind,
            enabled: true,
            next_due: kind.next_after(now),
            last_run: None,
        }
    }

    /// Whether this should fire at `now` (enabled and past its due time).
    #[must_use]
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        self.enabled && now >= self.next_due
    }

    /// Record a firing at `now` and advance `next_due` to the next cadence point.
    pub fn mark_fired(&mut self, now: DateTime<Utc>) {
        self.last_run = Some(now);
        self.next_due = self.kind.next_after(now);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn every_n_seconds_advances_by_n() {
        let k = ScheduleKind::EveryNSeconds { secs: 90 };
        assert_eq!(k.next_after(at("2026-06-23T12:00:00Z")), at("2026-06-23T12:01:30Z"));
        // Zero is clamped to 1s so the scheduler can't busy-loop.
        let z = ScheduleKind::EveryNSeconds { secs: 0 };
        assert_eq!(z.next_after(at("2026-06-23T12:00:00Z")), at("2026-06-23T12:00:01Z"));
    }

    #[test]
    fn daily_at_picks_today_then_tomorrow() {
        let k = ScheduleKind::DailyAt { hour: 9, minute: 30 };
        // Before today's 09:30 → today.
        assert_eq!(k.next_after(at("2026-06-23T08:00:00Z")), at("2026-06-23T09:30:00Z"));
        // Exactly at 09:30 (not strictly after) → tomorrow.
        assert_eq!(k.next_after(at("2026-06-23T09:30:00Z")), at("2026-06-24T09:30:00Z"));
        // After today's 09:30 → tomorrow.
        assert_eq!(k.next_after(at("2026-06-23T18:00:00Z")), at("2026-06-24T09:30:00Z"));
    }

    #[test]
    fn out_of_range_daily_components_are_clamped() {
        let k = ScheduleKind::DailyAt { hour: 99, minute: 99 };
        assert_eq!(k.next_after(at("2026-06-23T08:00:00Z")), at("2026-06-23T23:59:00Z"));
    }

    #[test]
    fn schedule_due_and_advance_lifecycle() {
        let now = at("2026-06-23T12:00:00Z");
        let mut s = Schedule::new("s1", "summarize my inbox", ScheduleKind::EveryNSeconds { secs: 60 }, now);
        assert_eq!(s.next_due, at("2026-06-23T12:01:00Z"));
        assert!(!s.is_due(now), "not due before next_due");
        assert!(s.is_due(at("2026-06-23T12:01:00Z")), "due at next_due");

        s.mark_fired(at("2026-06-23T12:01:05Z"));
        assert_eq!(s.last_run, Some(at("2026-06-23T12:01:05Z")));
        assert_eq!(s.next_due, at("2026-06-23T12:02:05Z"), "advanced from the fire time");

        // Disabled never fires.
        s.enabled = false;
        assert!(!s.is_due(at("2026-06-23T13:00:00Z")));
    }

    #[test]
    fn schedule_kind_serde_round_trips() {
        for k in [ScheduleKind::EveryNSeconds { secs: 300 }, ScheduleKind::DailyAt { hour: 7, minute: 0 }] {
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(serde_json::from_str::<ScheduleKind>(&json).unwrap(), k);
        }
    }
}
