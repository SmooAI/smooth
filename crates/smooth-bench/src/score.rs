//! `Score` — aggregated result of a curated aider-polyglot sweep.
//!
//! This is the JSON artifact "The Line" publishes with every Smooth
//! release. Single number across 6 languages × 20 tasks, plus
//! per-language breakdown, cost, duration, and the budget-cap flag.
//!
//! A `Score` is *also* what gets emitted when a `--pr` CI-gate run
//! cuts short — the fewer-tasks sample has the same shape as the
//! authoritative `--release` sample, so downstream tooling (README
//! badge, release notes, PR bot) doesn't care which gate produced it.
//!
//! Serde round-trips losslessly; see `score_serde_roundtrip` in
//! tests for the invariant.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Per-language pass breakdown inside a `Score`.
///
/// `pass_rate` is `tasks_green / tasks_attempted`, with 0/0 returning
/// 0.0 (never NaN — downstream consumers serialise and compare these
/// numbers, and NaN breaks both).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LanguageScore {
    pub pass_rate: f64,
    pub tasks_attempted: u32,
    pub tasks_green: u32,
}

impl LanguageScore {
    /// Build a `LanguageScore` from raw counts. Handles the 0/0 case
    /// by returning `pass_rate = 0.0` rather than producing NaN.
    #[must_use]
    pub fn from_counts(tasks_attempted: u32, tasks_green: u32) -> Self {
        let pass_rate = if tasks_attempted == 0 {
            0.0
        } else {
            f64::from(tasks_green) / f64::from(tasks_attempted)
        };
        Self {
            pass_rate,
            tasks_attempted,
            tasks_green,
        }
    }
}

impl Score {
    /// Render a human-readable summary of the Score. Shared between
    /// `smooth-bench score` (no `--output` → stdout) and `th bench
    /// score` (the baked-in Line the shipped binary carries) so both
    /// surfaces print identically.
    #[must_use]
    pub fn render_table(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(out, "The Line — smooth-bench score");
        let _ = writeln!(out, "  smooth version:    {}", self.smooth_version);
        let _ = writeln!(out, "  commit:            {}", self.commit_sha);
        let _ = writeln!(out, "  ran at:            {}", self.ran_at.to_rfc3339());
        let _ = writeln!(
            out,
            "  overall pass rate: {:.1}%  ({}/{} tasks green)",
            self.overall_pass_rate * 100.0,
            self.tasks_green,
            self.tasks_attempted
        );
        let _ = writeln!(out, "  cost:              ${:.4} (cap ${:.2})", self.cost_usd, self.budget_usd_cap);
        if self.budget_usd_hit {
            let _ = writeln!(out, "  BUDGET CAP HIT — score is partial");
        }
        let _ = writeln!(out, "  median task time:  {} ms", self.median_task_ms);
        let _ = writeln!(out);
        let _ = writeln!(out, "  by language:");
        for (lang, ls) in &self.by_language {
            let _ = writeln!(out, "    {lang:<12} {:.1}%  ({}/{})", ls.pass_rate * 100.0, ls.tasks_green, ls.tasks_attempted);
        }
        out
    }
}

/// Aggregate score emitted by `smooth-bench score`.
///
/// Written to stdout (or `--output <path>`) as pretty-printed JSON
/// when the output path ends in `.json`. Otherwise a human table is
/// rendered; the JSON can still be recovered from
/// `~/.smooth/bench-runs/<run-id>/score.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Score {
    pub smooth_version: String,
    pub commit_sha: String,
    pub ran_at: chrono::DateTime<chrono::Utc>,
    pub overall_pass_rate: f64,
    pub by_language: BTreeMap<String, LanguageScore>,
    pub tasks_attempted: u32,
    pub tasks_green: u32,
    pub cost_usd: f64,
    pub median_task_ms: u64,
    pub budget_usd_cap: f64,
    pub budget_usd_hit: bool,
}

/// Compute the median of `values` in milliseconds. Empty input
/// returns 0 (there's no meaningful "median of nothing", and 0 is
/// the value that makes the downstream display harmless).
///
/// Even-length inputs average the two middle values and truncate
/// toward zero — we're milliseconds, a half-millisecond delta is noise.
#[must_use]
pub fn median_ms(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted: Vec<u64> = values.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    if n.is_multiple_of(2) {
        // average of the two middle elements
        let lo = sorted[n / 2 - 1];
        let hi = sorted[n / 2];
        // `u64 + u64 / 2` — neither value will come close to u64::MAX
        // in any realistic run, but be safe and use wrapping-averaging
        // via `((a ^ b) >> 1) + (a & b)` so we don't overflow.
        ((lo ^ hi) >> 1).wrapping_add(lo & hi)
    } else {
        sorted[n / 2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn language_score_pass_rate_handles_zero_zero() {
        let s = LanguageScore::from_counts(0, 0);
        assert_eq!(s.pass_rate, 0.0);
        assert!(!s.pass_rate.is_nan());
    }

    #[test]
    fn language_score_pass_rate_basic() {
        let s = LanguageScore::from_counts(20, 17);
        assert!((s.pass_rate - 0.85).abs() < 1e-9);
    }

    #[test]
    fn language_score_pass_rate_all_green() {
        let s = LanguageScore::from_counts(20, 20);
        assert!((s.pass_rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn language_score_pass_rate_all_red() {
        let s = LanguageScore::from_counts(20, 0);
        assert_eq!(s.pass_rate, 0.0);
    }

    #[test]
    fn median_empty_returns_zero() {
        assert_eq!(median_ms(&[]), 0);
    }

    #[test]
    fn median_single_entry() {
        assert_eq!(median_ms(&[42]), 42);
    }

    #[test]
    fn median_odd_count_takes_middle() {
        assert_eq!(median_ms(&[3, 1, 2]), 2);
        assert_eq!(median_ms(&[10, 50, 20, 30, 40]), 30);
    }

    #[test]
    fn median_even_count_averages_middle_two() {
        assert_eq!(median_ms(&[1, 2, 3, 4]), 2); // (2+3)/2 = 2 (trunc)
        assert_eq!(median_ms(&[10, 20]), 15);
        assert_eq!(median_ms(&[100, 200, 300, 400]), 250);
    }

    #[test]
    fn median_unsorted_input_still_correct() {
        // Should sort before computing — don't trust input order.
        assert_eq!(median_ms(&[300, 100, 200]), 200);
    }

    #[test]
    fn score_serde_roundtrip() {
        let mut by_language = BTreeMap::new();
        by_language.insert("python".to_string(), LanguageScore::from_counts(20, 17));
        by_language.insert("rust".to_string(), LanguageScore::from_counts(20, 15));

        let original = Score {
            smooth_version: "0.42.1".to_string(),
            commit_sha: "abc123def456".to_string(),
            ran_at: chrono::Utc.with_ymd_and_hms(2026, 4, 23, 12, 34, 56).unwrap(),
            overall_pass_rate: 0.8,
            by_language,
            tasks_attempted: 40,
            tasks_green: 32,
            cost_usd: 4.23,
            median_task_ms: 15_000,
            budget_usd_cap: 10.0,
            budget_usd_hit: false,
        };

        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: Score = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, original);
    }

    #[test]
    fn score_serde_roundtrip_with_budget_hit() {
        // The partial-result case — budget hit mid-run.
        let mut by_language = BTreeMap::new();
        by_language.insert("python".to_string(), LanguageScore::from_counts(5, 3));

        let original = Score {
            smooth_version: "0.42.1".to_string(),
            commit_sha: "abc123".to_string(),
            ran_at: chrono::Utc.with_ymd_and_hms(2026, 4, 23, 0, 0, 0).unwrap(),
            overall_pass_rate: 0.6,
            by_language,
            tasks_attempted: 5,
            tasks_green: 3,
            cost_usd: 10.07,
            median_task_ms: 8_000,
            budget_usd_cap: 10.0,
            budget_usd_hit: true,
        };

        let json = serde_json::to_string_pretty(&original).expect("serialize");
        let decoded: Score = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, original);
        assert!(decoded.budget_usd_hit);
    }
}
