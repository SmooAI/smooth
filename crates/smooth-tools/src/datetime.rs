//! `current_datetime` — the agent's clock (pearl th-4c6271).
//!
//! Big Smooth had no clock, so it hallucinated "today" from whatever date its
//! training data suggested (observed: "Fri May 22", "December 19, 2024"). This
//! tool is the fix: one call returns the real local time, UTC, ISO-8601, the
//! weekday, and the IANA timezone name.
//!
//! Cross-platform by construction — `chrono::Local` and `iana_time_zone` both
//! handle macOS/Linux/Windows, so there is no `#[cfg]` here.

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, Local, Utc};
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

/// `current_datetime` — what time is it, right now, on this machine.
pub struct CurrentDatetimeTool;

#[async_trait]
impl Tool for CurrentDatetimeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "current_datetime".into(),
            description: "Get the CURRENT date and time (local, UTC, ISO-8601, weekday, IANA timezone). You have no internal clock, so call this before answering anything that depends on \"now\" — today's date, the day of the week, how long ago something was, scheduling, or dating a document. Takes no arguments."
                .into(),
            parameters: json!({ "type": "object", "properties": {}, "required": [] }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _arguments: Value) -> anyhow::Result<String> {
        let local = Local::now().fixed_offset();
        // Falls back to the numeric offset when the platform can't name the
        // zone (bare containers with no /etc/localtime symlink hit this).
        let tz = iana_time_zone::get_timezone().unwrap_or_else(|_| local.offset().to_string());
        Ok(render(local, &tz))
    }
}

/// Render the reply. Split out from `execute` so tests pin a known instant
/// instead of racing the real clock.
fn render(local: DateTime<FixedOffset>, tz_name: &str) -> String {
    let utc: DateTime<Utc> = local.into();
    format!(
        "Local:     {}\nWeekday:   {}\nTimezone:  {tz_name} (UTC{})\nISO-8601:  {}\nUTC:       {}\nUnix:      {}",
        local.format("%Y-%m-%d %H:%M:%S"),
        local.format("%A"),
        local.format("%:z"),
        local.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        utc.format("%Y-%m-%d %H:%M:%SZ"),
        utc.timestamp(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    /// 2026-08-01 14:32:05 in UTC-07:00 — a Saturday.
    fn fixed() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-08-01T14:32:05-07:00").unwrap()
    }

    #[test]
    fn render_emits_every_field_from_a_fixed_instant() {
        let out = render(fixed(), "America/Los_Angeles");
        assert!(out.contains("Local:     2026-08-01 14:32:05"), "{out}");
        assert!(out.contains("Weekday:   Saturday"), "{out}");
        assert!(out.contains("Timezone:  America/Los_Angeles (UTC-07:00)"), "{out}");
        assert!(out.contains("ISO-8601:  2026-08-01T14:32:05-07:00"), "{out}");
        // Same instant, shifted into UTC — catches an accidental offset drop.
        assert!(out.contains("UTC:       2026-08-01 21:32:05Z"), "{out}");
        assert!(out.contains(&format!("Unix:      {}", fixed().timestamp())), "{out}");
    }

    #[test]
    fn render_handles_utc_and_positive_offsets() {
        let utc = DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap();
        let out = render(utc, "UTC");
        assert!(out.contains("Weekday:   Thursday"), "{out}");
        assert!(out.contains("Timezone:  UTC (UTC+00:00)"), "{out}");

        // Tokyo is a day ahead of UTC at this instant.
        let jst = DateTime::parse_from_rfc3339("2026-01-01T08:30:00+09:00").unwrap();
        let out = render(jst, "Asia/Tokyo");
        assert!(out.contains("Local:     2026-01-01 08:30:00"), "{out}");
        assert!(out.contains("UTC:       2025-12-31 23:30:00Z"), "{out}");
        assert!(out.contains("Timezone:  Asia/Tokyo (UTC+09:00)"), "{out}");
    }

    #[test]
    fn schema_takes_no_arguments() {
        let s = CurrentDatetimeTool.schema();
        assert_eq!(s.name, "current_datetime");
        assert_eq!(s.parameters["required"].as_array().unwrap().len(), 0);
        assert!(CurrentDatetimeTool.is_concurrent_safe());
    }

    #[tokio::test]
    async fn execute_ignores_arguments_and_reports_a_plausible_now() {
        let out = CurrentDatetimeTool.execute(json!({"unexpected": 1})).await.unwrap();
        assert!(out.contains("Unix:      "), "{out}");
        let unix: i64 = out.rsplit_once("Unix:      ").unwrap().1.trim().parse().unwrap();
        // Sometime after this tool was written and before the year 2100.
        assert!((1_785_000_000..4_102_444_800).contains(&unix), "{out}");
    }
}
