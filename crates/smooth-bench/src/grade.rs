//! `grade.toml` schema for real-world bench tasks.
//!
//! Each task under `crates/smooth-bench/tasks-real/<id>/` carries a
//! `grade.toml` describing how the post-hoc scorer should weight its
//! five axes (pass, edits, verify, tools, cost), the held-out test
//! command, and the human-baseline numbers used to normalize the
//! "edits" axis.
//!
//! Parsing is deliberately strict: weights MUST sum to 1.0 (within a
//! small epsilon) and all required fields must be present. A bad
//! `grade.toml` is a harness bug — silently defaulting would hide
//! real misconfigurations.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level shape of `grade.toml`. Matches the schema documented in
/// the `score-real` plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GradeToml {
    pub task: TaskMeta,
    pub verify: VerifyConfig,
    pub weights: AxisWeights,
    pub cost: CostConfig,
    pub edits: EditsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskMeta {
    /// Stable identifier — usually matches the directory name.
    pub id: String,
    /// `rust` | `python` | `typescript`. Used by the runner to pick
    /// the right test command and edit-detection rules.
    pub language: String,
    /// Files a human reviewer would expect a competent implementer
    /// to touch. Used as the denominator for the edits-axis score.
    pub human_baseline_edits: u32,
    /// Net lines a human would write. Used to penalize sprawl when
    /// the agent's diff is much larger than necessary.
    pub human_baseline_lines: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerifyConfig {
    /// Shell command run in the scratch dir AFTER the hidden-tests
    /// overlay is applied. Single string; the runner splits on
    /// whitespace and execs the first token.
    pub test_cmd: String,
    /// Substring patterns the scorer greps the AgentEvent stream
    /// for — tools the agent SHOULD have invoked at some point.
    /// Each hit contributes to the verify-axis score.
    #[serde(default)]
    pub expect_tool_invocations: Vec<String>,
    /// Minimum number of verify-tool invocations (matches across
    /// `expect_tool_invocations`) required for full credit on the
    /// verify axis. Below this, the axis decays linearly to zero.
    #[serde(default = "default_min_verify")]
    pub min_verify_invocations: u32,
}

fn default_min_verify() -> u32 {
    1
}

/// Per-axis weights. Validated to sum to 1.0 on load.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AxisWeights {
    pub pass: f64,
    pub edits: f64,
    pub verify: f64,
    pub tools: f64,
    pub cost: f64,
}

impl AxisWeights {
    pub fn sum(&self) -> f64 {
        self.pass + self.edits + self.verify + self.tools + self.cost
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CostConfig {
    /// USD budget. <= budget → 1.0 on cost axis; linear decay to 0
    /// at 2× budget; clamped to 0 beyond.
    pub budget_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditsConfig {
    /// Subtracted from the edits-axis score for each file edited
    /// beyond the human baseline.
    pub penalty_per_extra_file: f64,
    /// Subtracted from the edits-axis score for each 100 net lines
    /// over the human baseline (linearly interpolated for fractional
    /// hundreds — 50 extra lines = 0.5 × penalty).
    pub penalty_per_extra_100_lines: f64,
}

/// Tolerance for the "weights must sum to 1.0" check. TOML parses
/// floats with enough precision that the sum of five clean decimals
/// (0.50 + 0.15 + 0.15 + 0.10 + 0.10) rounds exactly — but we leave
/// epsilon so a config author can write 0.333 + 0.333 + 0.334 + 0 + 0
/// without it tripping the gate.
const WEIGHT_SUM_EPSILON: f64 = 1e-3;

impl GradeToml {
    /// Parse a `grade.toml` from a TOML string.
    ///
    /// # Errors
    /// - TOML parse failure.
    /// - Weights don't sum to 1.0 (within `WEIGHT_SUM_EPSILON`).
    /// - Any individual weight is negative.
    pub fn parse(s: &str) -> Result<Self> {
        let parsed: Self = toml::from_str(s).context("parse grade.toml")?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Load a `grade.toml` from disk.
    ///
    /// # Errors
    /// File-read errors and any error from [`Self::parse`].
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read_to_string(path).with_context(|| format!("read grade.toml at {}", path.display()))?;
        Self::parse(&bytes)
    }

    fn validate(&self) -> Result<()> {
        let w = &self.weights;
        for (name, v) in [("pass", w.pass), ("edits", w.edits), ("verify", w.verify), ("tools", w.tools), ("cost", w.cost)] {
            if v < 0.0 {
                return Err(anyhow!("grade.toml: weight `{name}` is negative ({v})"));
            }
        }
        let sum = w.sum();
        if (sum - 1.0).abs() > WEIGHT_SUM_EPSILON {
            return Err(anyhow!("grade.toml: axis weights must sum to 1.0 (got {sum}; tolerance {WEIGHT_SUM_EPSILON})"));
        }
        if self.cost.budget_usd <= 0.0 {
            return Err(anyhow!("grade.toml: cost.budget_usd must be positive (got {})", self.cost.budget_usd));
        }
        Ok(())
    }
}

/// Combine five axis scores in `[0.0, 1.0]` into a single weighted
/// score using `weights`. Each axis is clamped to `[0.0, 1.0]` before
/// weighting so a buggy axis computation can't blow the result past 1.0
/// or below 0.0.
#[must_use]
pub fn combine_axes(pass: f64, edits: f64, verify: f64, tools: f64, cost: f64, weights: &AxisWeights) -> f64 {
    let clamp = |x: f64| x.clamp(0.0, 1.0);
    weights.pass.mul_add(
        clamp(pass),
        weights.edits.mul_add(
            clamp(edits),
            weights
                .verify
                .mul_add(clamp(verify), weights.tools.mul_add(clamp(tools), weights.cost * clamp(cost))),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_EXAMPLE: &str = r#"
[task]
id = "rust-ttl-cache"
language = "rust"
human_baseline_edits = 3
human_baseline_lines = 80

[verify]
test_cmd = "cargo test --quiet"
expect_tool_invocations = ["cargo test", "cargo check"]
min_verify_invocations = 2

[weights]
pass = 0.50
edits = 0.15
verify = 0.15
tools = 0.10
cost = 0.10

[cost]
budget_usd = 0.50

[edits]
penalty_per_extra_file = 0.10
penalty_per_extra_100_lines = 0.05
"#;

    #[test]
    fn parse_full_example() {
        let g = GradeToml::parse(FULL_EXAMPLE).expect("parse + validate");
        assert_eq!(g.task.id, "rust-ttl-cache");
        assert_eq!(g.task.language, "rust");
        assert_eq!(g.task.human_baseline_edits, 3);
        assert_eq!(g.task.human_baseline_lines, 80);
        assert_eq!(g.verify.test_cmd, "cargo test --quiet");
        assert_eq!(g.verify.expect_tool_invocations.len(), 2);
        assert_eq!(g.verify.min_verify_invocations, 2);
        assert!((g.weights.sum() - 1.0).abs() < 1e-9);
        assert!((g.cost.budget_usd - 0.50).abs() < 1e-9);
        assert!((g.edits.penalty_per_extra_file - 0.10).abs() < 1e-9);
        assert!((g.edits.penalty_per_extra_100_lines - 0.05).abs() < 1e-9);
    }

    #[test]
    fn weights_must_sum_to_one() {
        // Same as full example but weights sum to 0.9 — should reject.
        let bad = r#"
[task]
id = "x"
language = "rust"
human_baseline_edits = 1
human_baseline_lines = 10

[verify]
test_cmd = "cargo test"

[weights]
pass = 0.40
edits = 0.15
verify = 0.15
tools = 0.10
cost = 0.10

[cost]
budget_usd = 0.5

[edits]
penalty_per_extra_file = 0.1
penalty_per_extra_100_lines = 0.05
"#;
        let err = GradeToml::parse(bad).expect_err("should reject 0.9 sum");
        let msg = format!("{err:#}");
        assert!(msg.contains("must sum to 1.0"), "unexpected error: {msg}");
    }

    #[test]
    fn missing_weight_rejected() {
        // `cost` weight missing entirely → serde fails before we
        // reach `validate`. Either error path is acceptable; just
        // ensure we don't silently default to 0.
        let bad = r#"
[task]
id = "x"
language = "rust"
human_baseline_edits = 1
human_baseline_lines = 10

[verify]
test_cmd = "cargo test"

[weights]
pass = 0.50
edits = 0.20
verify = 0.20
tools = 0.10

[cost]
budget_usd = 0.5

[edits]
penalty_per_extra_file = 0.1
penalty_per_extra_100_lines = 0.05
"#;
        let err = GradeToml::parse(bad).expect_err("missing weight must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("cost") || msg.to_lowercase().contains("missing"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn negative_weight_rejected() {
        let bad = r#"
[task]
id = "x"
language = "rust"
human_baseline_edits = 1
human_baseline_lines = 10

[verify]
test_cmd = "cargo test"

[weights]
pass = 0.70
edits = 0.20
verify = 0.20
tools = -0.10
cost = 0.00

[cost]
budget_usd = 0.5

[edits]
penalty_per_extra_file = 0.1
penalty_per_extra_100_lines = 0.05
"#;
        let err = GradeToml::parse(bad).expect_err("negative weight must be rejected");
        let msg = format!("{err:#}");
        assert!(msg.contains("negative"), "unexpected error: {msg}");
    }

    #[test]
    fn zero_budget_rejected() {
        let bad = r#"
[task]
id = "x"
language = "rust"
human_baseline_edits = 1
human_baseline_lines = 10

[verify]
test_cmd = "cargo test"

[weights]
pass = 0.50
edits = 0.15
verify = 0.15
tools = 0.10
cost = 0.10

[cost]
budget_usd = 0.0

[edits]
penalty_per_extra_file = 0.1
penalty_per_extra_100_lines = 0.05
"#;
        let err = GradeToml::parse(bad).expect_err("zero budget must be rejected");
        let msg = format!("{err:#}");
        assert!(msg.contains("budget_usd"), "unexpected error: {msg}");
    }

    #[test]
    fn combine_axes_weighted_correctly() {
        let w = AxisWeights {
            pass: 0.5,
            edits: 0.1,
            verify: 0.1,
            tools: 0.1,
            cost: 0.2,
        };
        // Every axis at 1.0 → result is 1.0.
        let full = combine_axes(1.0, 1.0, 1.0, 1.0, 1.0, &w);
        assert!((full - 1.0).abs() < 1e-9, "expected 1.0 got {full}");

        // Every axis at 0.0 → 0.0.
        let none = combine_axes(0.0, 0.0, 0.0, 0.0, 0.0, &w);
        assert!(none.abs() < 1e-9);

        // Mixed: pass=1, others=0 → 0.5.
        let pass_only = combine_axes(1.0, 0.0, 0.0, 0.0, 0.0, &w);
        assert!((pass_only - 0.5).abs() < 1e-9);
    }

    #[test]
    fn combine_axes_clamps_inputs() {
        let w = AxisWeights {
            pass: 0.5,
            edits: 0.1,
            verify: 0.1,
            tools: 0.1,
            cost: 0.2,
        };
        // Out-of-range inputs (negative + > 1.0) must be clamped, not
        // amplified.
        let clamped = combine_axes(2.0, -0.5, 1.5, 0.5, 1.0, &w);
        // Equivalent to combine(1, 0, 1, 0.5, 1) = 0.5 + 0.1 + 0.05 + 0.2 = 0.85
        assert!((clamped - 0.85).abs() < 1e-9, "got {clamped}");
    }
}
