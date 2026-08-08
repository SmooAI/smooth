//! Cross-model comparison (pearl th-898ec6).
//!
//! The suites answer "how did this model do?". This answers the two
//! questions that actually change decisions:
//!
//! 1. **Is the premium model worth it?** A rate-per-dollar column next to
//!    the pass rate, so "deepseek-v4-flash is incredible" is a number
//!    rather than a vibe.
//! 2. **Is this a model problem or a HARNESS problem?** The grid. A row
//!    that every model fails is not six bad models — it is our scenario,
//!    our tools, or our prompt. Those rows are the improvement backlog,
//!    and they are invisible in any single-model run.
//!
//! Deliberately dumb: the callers flatten their own run type into
//! [`ModelRow`], so this module knows nothing about agentic vs convo and
//! neither suite grows a trait to satisfy it.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

/// One model's results, flattened by the caller.
#[derive(Debug, Clone)]
pub struct ModelRow {
    pub model: String,
    /// Fraction in `0.0..=1.0` over conclusive scenarios.
    pub pass_rate: f64,
    pub passed: usize,
    pub conclusive: usize,
    pub inconclusive: usize,
    pub cost_usd: f64,
    /// Total wall clock across every trial.
    pub duration_ms: u64,
    /// `(scenario id, verdict)` — one entry per scenario, in suite order.
    pub cells: Vec<(String, Cell)>,
    /// False when the measured cost is not distinguishable from the
    /// shared key's background traffic. The figure is then rendered as
    /// `<noise` rather than a precise-looking number, because a cost
    /// column that ranks a premium model below a budget one is worse
    /// than an empty one — someone will act on it.
    pub cost_resolvable: bool,
}

/// A scenario's outcome for one model, reduced to what the grid shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Pass,
    Fail,
    /// No usable data — a boot failure or a broken judge. Never counted
    /// as either a pass or a fail.
    Inconclusive,
    /// Failed, but the gap is already documented (`expect_fail`). Counts
    /// as a fail in the rate — it genuinely isn't solved — but stays out
    /// of the "suspect the harness" callout, which exists to surface
    /// gaps nobody has written down yet.
    KnownGap,
}

impl Cell {
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Pass => "✓",
            Self::Fail => "✗",
            Self::Inconclusive => "·",
            Self::KnownGap => "⊘",
        }
    }
}

/// Cost of one additional passing scenario, in dollars.
///
/// `None` when nothing passed — a free run that solves nothing has no
/// meaningful price per pass, and reporting `$0.0000` would rank it
/// first — or when the cost itself is below the noise floor, since
/// dividing an unresolvable number does not make it resolvable.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "scenario counts are single digits")]
pub fn cost_per_pass(row: &ModelRow) -> Option<f64> {
    (row.passed > 0 && row.cost_resolvable).then(|| row.cost_usd / row.passed as f64)
}

/// Scenarios no model passed, excluding already-documented gaps. These
/// are the harness backlog: when every model fails the same row and
/// nobody has written down why, the row is the bug.
#[must_use]
pub fn universally_failed(rows: &[ModelRow]) -> Vec<String> {
    let mut ever_passed: BTreeMap<&str, bool> = BTreeMap::new();
    let mut order: Vec<&str> = Vec::new();
    for r in rows {
        for (id, cell) in &r.cells {
            if !ever_passed.contains_key(id.as_str()) {
                order.push(id.as_str());
            }
            let seen = ever_passed.entry(id.as_str()).or_insert(false);
            // A documented gap counts as "accounted for" — it is already
            // tracked, so re-reporting it as an unknown is noise.
            *seen = *seen || matches!(cell, Cell::Pass | Cell::KnownGap);
        }
    }
    order
        .into_iter()
        .filter(|id| ever_passed.get(id).copied() == Some(false))
        .map(ToString::to_string)
        .collect()
}

/// Rows sorted best-first: pass rate descending, then cheaper wins ties.
#[must_use]
pub fn ranked(rows: &[ModelRow]) -> Vec<&ModelRow> {
    let mut out: Vec<&ModelRow> = rows.iter().collect();
    out.sort_by(|a, b| {
        b.pass_rate
            .partial_cmp(&a.pass_rate)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Only break a rate tie on cost when BOTH figures actually
            // mean something; otherwise noise decides the ranking.
            .then_with(|| {
                if a.cost_resolvable && b.cost_resolvable {
                    a.cost_usd.partial_cmp(&b.cost_usd).unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
    });
    out
}

/// One model's row in the published scoreboard.
///
/// Deliberately a separate, flatter type from [`ModelRow`]: this one is
/// a published artefact other things parse (the docs table, the badge,
/// anything tracking a model over time), so it must not churn every
/// time the in-process row gains a field.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PublishedModel {
    pub model: String,
    /// Pass rate as a PERCENT, one decimal — the number that gets
    /// published, pre-rounded so the badge, the table and the JSON can
    /// never disagree.
    pub pass_rate_pct: f64,
    pub passed: usize,
    pub conclusive: usize,
    pub inconclusive: usize,
    /// `None` when the cost was not resolvable above the key's noise —
    /// publishing an unresolvable number as fact is how a premium model
    /// ends up looking cheap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_pass_usd: Option<f64>,
    pub duration_s: f64,
}

/// The whole scoreboard, ready to serialise into `docs/model-scores.json`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Scoreboard {
    /// Which suite produced this — `convo` and `agentic` measure
    /// different things and their percentages are not comparable.
    pub suite: String,
    /// Trials per scenario. A 1-trial run is an anecdote; published
    /// numbers must carry this so nobody over-reads a 1-scenario gap.
    pub trials: usize,
    pub scenario_count: usize,
    pub models: Vec<PublishedModel>,
}

impl Scoreboard {
    /// Build from leaderboard rows, best-first.
    #[must_use]
    #[allow(clippy::cast_precision_loss, reason = "durations are ms; f64 is exact well past that")]
    pub fn from_rows(suite: &str, trials: usize, rows: &[ModelRow]) -> Self {
        let models: Vec<PublishedModel> = ranked(rows)
            .into_iter()
            .map(|r| PublishedModel {
                model: r.model.clone(),
                pass_rate_pct: (r.pass_rate * 1000.0).round() / 10.0,
                passed: r.passed,
                conclusive: r.conclusive,
                inconclusive: r.inconclusive,
                cost_usd: r.cost_resolvable.then_some(r.cost_usd),
                cost_per_pass_usd: cost_per_pass(r),
                duration_s: (r.duration_ms as f64 / 100.0).round() / 10.0,
            })
            .collect();
        let scenario_count = rows.first().map_or(0, |r| r.cells.len());
        Self {
            suite: suite.to_string(),
            trials,
            scenario_count,
            models,
        }
    }

    /// The headline: the best model and its percentage, for the badge.
    /// `None` for an empty board.
    #[must_use]
    pub fn best(&self) -> Option<&PublishedModel> {
        self.models.first()
    }
}

/// The leaderboard plus the scenario × model grid.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "wall-clock ms to one decimal")]
pub fn render(suite: &str, rows: &[ModelRow]) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        let _ = writeln!(out, "smooth-bench {suite} — no models ran");
        return out;
    }

    let _ = writeln!(out, "\nsmooth-bench {suite} — model leaderboard");
    let _ = writeln!(out);
    let _ = writeln!(out, "  model                       pass    rate    $cost     $/pass    time");
    for r in ranked(rows) {
        let _ = writeln!(
            out,
            "  {model:<25} {passed:>3}/{conclusive:<3} {rate:>5.1}%  {cost:>8}  {per:>8}  {secs:>5.1}s{inc}",
            model = r.model,
            passed = r.passed,
            conclusive = r.conclusive,
            rate = r.pass_rate * 100.0,
            cost = if r.cost_resolvable {
                format!("{:.4}", r.cost_usd)
            } else {
                "<noise".to_string()
            },
            per = cost_per_pass(r).map_or_else(|| "-".to_string(), |c| format!("{c:.4}")),
            secs = r.duration_ms as f64 / 1000.0,
            inc = if r.inconclusive > 0 {
                format!("  ({} INCONCLUSIVE)", r.inconclusive)
            } else {
                String::new()
            },
        );
    }

    // Grid: scenario rows, model columns. Columns follow the ranking so
    // the best model is leftmost.
    let ordered = ranked(rows);
    let scenarios: Vec<String> = {
        let mut seen = Vec::new();
        for r in rows {
            for (id, _) in &r.cells {
                if !seen.contains(id) {
                    seen.push(id.clone());
                }
            }
        }
        seen
    };
    if !scenarios.is_empty() {
        let _ = writeln!(out, "\n  scenario × model   (✓ pass  ✗ fail  ⊘ known gap  · inconclusive)");
        let _ = write!(out, "\n  {:<28}", "");
        for i in 1..=ordered.len() {
            let _ = write!(out, " {i:>3}");
        }
        let _ = writeln!(out);
        for id in &scenarios {
            let _ = write!(out, "  {id:<28}");
            for r in &ordered {
                let cell = r.cells.iter().find(|(sid, _)| sid == id).map_or(Cell::Inconclusive, |(_, c)| *c);
                let _ = write!(out, " {:>3}", cell.glyph());
            }
            let _ = writeln!(out);
        }
        let _ = writeln!(out);
        for (i, r) in ordered.iter().enumerate() {
            let _ = writeln!(out, "  {:>3} = {}", i + 1, r.model);
        }
    }

    let stuck = universally_failed(rows);
    if !stuck.is_empty() {
        let _ = writeln!(out, "\n  ⚠ no model passed these — suspect the harness, not the model:");
        for id in &stuck {
            let _ = writeln!(out, "      {id}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(model: &str, rate: f64, passed: usize, cost: f64, cells: &[(&str, Cell)]) -> ModelRow {
        ModelRow {
            model: model.to_string(),
            pass_rate: rate,
            passed,
            conclusive: cells.iter().filter(|(_, c)| *c != Cell::Inconclusive).count(),
            inconclusive: cells.iter().filter(|(_, c)| *c == Cell::Inconclusive).count(),
            cost_usd: cost,
            duration_ms: 1000,
            cost_resolvable: true,
            cells: cells.iter().map(|(id, c)| ((*id).to_string(), *c)).collect(),
        }
    }

    #[test]
    fn ranks_by_rate_then_by_price() {
        let rows = vec![
            row("pricey", 0.5, 1, 2.00, &[("a", Cell::Pass), ("b", Cell::Fail)]),
            row("cheap", 0.5, 1, 0.01, &[("a", Cell::Pass), ("b", Cell::Fail)]),
            row("best", 1.0, 2, 5.00, &[("a", Cell::Pass), ("b", Cell::Pass)]),
        ];
        let order: Vec<&str> = ranked(&rows).iter().map(|r| r.model.as_str()).collect();
        assert_eq!(order, ["best", "cheap", "pricey"], "rate first, then the cheaper of a tie");
    }

    #[test]
    fn universally_failed_is_the_harness_backlog() {
        let rows = vec![
            row("a", 0.5, 1, 0.1, &[("solved", Cell::Pass), ("stuck", Cell::Fail)]),
            row("b", 0.5, 1, 0.1, &[("solved", Cell::Pass), ("stuck", Cell::Fail)]),
        ];
        assert_eq!(universally_failed(&rows), ["stuck"]);
    }

    #[test]
    fn an_inconclusive_scenario_is_not_a_universal_failure() {
        // Missing data must not masquerade as a harness bug.
        let rows = vec![row("a", 0.0, 0, 0.1, &[("x", Cell::Inconclusive)]), row("b", 1.0, 1, 0.1, &[("x", Cell::Pass)])];
        assert!(universally_failed(&rows).is_empty());

        let rows = vec![row("a", 0.0, 0, 0.1, &[("x", Cell::Inconclusive)]), row("b", 0.0, 0, 0.1, &[("x", Cell::Fail)])];
        assert_eq!(universally_failed(&rows), ["x"], "inconclusive everywhere-but-one still counts the fail");
    }

    #[test]
    fn a_documented_gap_is_not_reported_as_an_unknown() {
        // rapid-correction is expect_fail: every model fails it, and that
        // is already tracked. Reporting it as a harness mystery is noise.
        let rows = vec![
            row("a", 0.0, 0, 0.1, &[("rapid-correction", Cell::KnownGap), ("real", Cell::Fail)]),
            row("b", 0.0, 0, 0.1, &[("rapid-correction", Cell::KnownGap), ("real", Cell::Fail)]),
        ];
        assert_eq!(universally_failed(&rows), ["real"]);
    }

    #[test]
    fn cost_per_pass_is_none_when_nothing_passed() {
        // A $0 run that solves nothing must not rank as infinitely cheap.
        let none = row("dud", 0.0, 0, 0.0, &[("a", Cell::Fail)]);
        assert!(cost_per_pass(&none).is_none());
        let some = row("ok", 1.0, 2, 1.0, &[("a", Cell::Pass), ("b", Cell::Pass)]);
        assert!((cost_per_pass(&some).expect("passed") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn render_names_every_model_and_scenario() {
        let rows = vec![
            row("deepseek-v4-flash", 1.0, 1, 0.002, &[("discovery", Cell::Pass)]),
            row("gpt-5.5-pro", 0.0, 0, 0.900, &[("discovery", Cell::Fail)]),
        ];
        let t = render("agentic", &rows);
        assert!(t.contains("deepseek-v4-flash"));
        assert!(t.contains("gpt-5.5-pro"));
        assert!(t.contains("discovery"));
        // Cheap-and-correct must be ranked above expensive-and-wrong.
        let cheap = t.find("deepseek-v4-flash").expect("present");
        let pricey = t.find("gpt-5.5-pro").expect("present");
        assert!(cheap < pricey);
    }

    #[test]
    fn an_unresolvable_cost_renders_as_noise_and_suppresses_per_pass() {
        let mut r = row("noisy", 1.0, 1, 0.002, &[("a", Cell::Pass)]);
        r.cost_resolvable = false;
        assert!(cost_per_pass(&r).is_none(), "dividing noise does not make it a measurement");
        let t = render("convo", std::slice::from_ref(&r));
        assert!(t.contains("<noise"), "an unresolvable cost must not print as a precise figure: {t}");
        assert!(!t.contains("0.0020"));
    }

    #[test]
    fn noise_never_decides_the_ranking() {
        // Same rate; one cost is real, one is noise. Ordering must not
        // claim the noisy one is cheaper.
        let mut noisy = row("noisy", 1.0, 1, 0.001, &[("a", Cell::Pass)]);
        noisy.cost_resolvable = false;
        let real = row("real", 1.0, 1, 0.900, &[("a", Cell::Pass)]);
        let rows = vec![real, noisy];
        let order: Vec<&str> = ranked(&rows).iter().map(|r| r.model.as_str()).collect();
        assert_eq!(order, ["real", "noisy"], "input order preserved; noise must not sort ahead");
    }

    #[test]
    fn scoreboard_is_ranked_and_pre_rounded() {
        let rows = vec![
            row("cheap", 8.0 / 9.0, 8, 0.0188, &[("a", Cell::Pass)]),
            row("pricey", 7.0 / 9.0, 7, 0.7948, &[("a", Cell::Fail)]),
        ];
        let b = Scoreboard::from_rows("convo", 1, &rows);
        assert_eq!(b.suite, "convo");
        assert_eq!(b.trials, 1);
        assert_eq!(b.models[0].model, "cheap", "best model first");
        // Pre-rounded so badge/table/JSON can never disagree.
        assert!((b.models[0].pass_rate_pct - 88.9).abs() < 1e-9, "got {}", b.models[0].pass_rate_pct);
        assert_eq!(b.best().map(|m| m.model.as_str()), Some("cheap"));
    }

    #[test]
    fn an_unresolvable_cost_is_omitted_not_published_as_fact() {
        let mut r = row("noisy", 1.0, 1, 0.002, &[("a", Cell::Pass)]);
        r.cost_resolvable = false;
        let b = Scoreboard::from_rows("convo", 1, &[r]);
        assert_eq!(b.models[0].cost_usd, None);
        assert_eq!(b.models[0].cost_per_pass_usd, None);
        let json = serde_json::to_string(&b).expect("serialises");
        assert!(!json.contains("cost_usd"), "an unmeasured cost must be absent, not 0: {json}");
    }

    #[test]
    fn render_survives_an_empty_run() {
        assert!(render("agentic", &[]).contains("no models ran"));
    }
}
