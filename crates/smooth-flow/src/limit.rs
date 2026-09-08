//! Usage-limit handling: parse the reset time out of Claude Code's pane text
//! so the engine can *schedule* a resume instead of giving up (rule 3).
//!
//! Detection of the limit itself is `smooth_tmux::detect` (shared with
//! `th claude`); this module only turns "resets at 4pm" into an instant.

use chrono::{DateTime, Duration, Local, NaiveTime, TimeZone, Utc};
use regex::Regex;
use std::sync::OnceLock;

/// Fallback wait when the pane says "limit" but no time can be parsed.
pub const DEFAULT_RESET_WAIT: Duration = Duration::minutes(60);

fn clock_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // "resets at 4pm", "reset at 4:30 pm", "resets 11am (America/Chicago)"
    RE.get_or_init(|| Regex::new(r"(?i)reset(?:s|ting)?\s+(?:at\s+)?(\d{1,2})(?::(\d{2}))?\s*(am|pm)").unwrap_or_else(|_| unreachable!()))
}

fn relative_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // "resets in 2 hours", "reset in 45 minutes", "in 1h 20m"
    RE.get_or_init(|| {
        Regex::new(r"(?i)reset(?:s|ting)?\s+in\s+(?:(\d+)\s*(?:h|hours?|hr))?\s*(?:(\d+)\s*(?:m|min|minutes?))?").unwrap_or_else(|_| unreachable!())
    })
}

/// Parse the reset instant from `pane` relative to `now`.
///
/// Clock times are read as the next occurrence in the local timezone; a time
/// already passed today means tomorrow. `None` when no time is present.
#[must_use]
#[allow(clippy::needless_pass_by_value)]
pub fn parse_reset_at<Tz: TimeZone>(pane: &str, now: DateTime<Tz>) -> Option<DateTime<Utc>> {
    let now_local = now.with_timezone(&Local);
    if let Some(c) = clock_re().captures(pane) {
        let mut hour: u32 = c.get(1)?.as_str().parse().ok()?;
        let minute: u32 = c.get(2).map_or(0, |m| m.as_str().parse().unwrap_or(0));
        let pm = c.get(3)?.as_str().eq_ignore_ascii_case("pm");
        if hour > 12 || minute > 59 {
            return None;
        }
        if hour == 12 {
            hour = 0;
        }
        if pm {
            hour += 12;
        }
        let t = NaiveTime::from_hms_opt(hour, minute, 0)?;
        let mut candidate = now_local.date_naive().and_time(t);
        if candidate <= now_local.naive_local() {
            candidate += Duration::days(1);
        }
        let local = Local
            .from_local_datetime(&candidate)
            .single()
            .or_else(|| Local.from_local_datetime(&candidate).earliest())?;
        return Some(local.with_timezone(&Utc));
    }
    if let Some(c) = relative_re().captures(pane) {
        let h: i64 = c.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        let m: i64 = c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        if h == 0 && m == 0 {
            return None;
        }
        return Some(now.with_timezone(&Utc) + Duration::hours(h) + Duration::minutes(m));
    }
    None
}

/// The instant to resume: parsed, else `now + DEFAULT_RESET_WAIT`. Always
/// at least one minute in the future so a stale pane can't spin the loop.
#[must_use]
pub fn resume_at(pane: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    let at = parse_reset_at(pane, now).unwrap_or(now + DEFAULT_RESET_WAIT);
    at.max(now + Duration::minutes(1))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 7, h, m, 0).unwrap()
    }

    #[test]
    fn clock_time_later_today() {
        let got = parse_reset_at("Usage limit reached. Your limit will reset at 4pm.", at(10, 0)).unwrap();
        assert_eq!(got.with_timezone(&Local), at(16, 0));
    }

    #[test]
    fn clock_time_with_minutes_and_space() {
        let got = parse_reset_at("limit resets at 4:30 PM (America/Chicago)", at(10, 0)).unwrap();
        assert_eq!(got.with_timezone(&Local), at(16, 30));
    }

    #[test]
    fn clock_time_already_passed_means_tomorrow() {
        let got = parse_reset_at("resets at 9am", at(10, 0)).unwrap();
        assert_eq!(got.with_timezone(&Local), Local.with_ymd_and_hms(2026, 9, 8, 9, 0, 0).unwrap());
    }

    #[test]
    fn twelve_am_pm_edges() {
        assert_eq!(parse_reset_at("resets 12pm", at(10, 0)).unwrap().with_timezone(&Local), at(12, 0));
        assert_eq!(
            parse_reset_at("resets 12am", at(10, 0)).unwrap().with_timezone(&Local),
            Local.with_ymd_and_hms(2026, 9, 8, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn relative_forms() {
        let now = at(10, 0);
        assert_eq!(parse_reset_at("resets in 2 hours", now).unwrap().with_timezone(&Local), at(12, 0));
        assert_eq!(parse_reset_at("reset in 45 minutes", now).unwrap().with_timezone(&Local), at(10, 45));
        assert_eq!(parse_reset_at("resets in 1h 20m", now).unwrap().with_timezone(&Local), at(11, 20));
    }

    #[test]
    fn garbage_and_missing_time_are_none() {
        assert!(parse_reset_at("You've reached your usage limit.", at(10, 0)).is_none());
        assert!(parse_reset_at("resets at 25pm", at(10, 0)).is_none());
        assert!(parse_reset_at("resets in soon", at(10, 0)).is_none());
        assert!(parse_reset_at("", at(10, 0)).is_none());
    }

    #[test]
    fn resume_at_falls_back_and_never_points_to_the_past() {
        let now = Utc::now();
        let fallback = resume_at("usage limit reached", now);
        assert_eq!(fallback, now + DEFAULT_RESET_WAIT);
        let soon = resume_at("resets in 0h 0m", now);
        assert!(soon >= now + Duration::minutes(1));
    }
}
