//! Cost tracking and budget enforcement for LLM usage.
//!
//! Provides [`CostTracker`] for accumulating token usage and costs across
//! multiple LLM calls, [`CostBudget`] for setting spending limits, and
//! [`ModelPricing`] with built-in presets for common models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::llm::Usage;

/// Tracks cumulative LLM cost and token usage across an agent session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostTracker {
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_cost_usd: f64,
    pub calls: u32,
    entries: Vec<CostEntry>,
}

/// A single recorded LLM call with its cost breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEntry {
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cost_usd: f64,
    pub timestamp: DateTime<Utc>,
}

/// Budget limits for an agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostBudget {
    pub max_cost_usd: Option<f64>,
    pub max_tokens: Option<u64>,
}

/// Per-model pricing in USD per million tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    /// USD per million input/prompt tokens.
    pub prompt_per_mtok: f64,
    /// USD per million output/completion tokens.
    pub completion_per_mtok: f64,
}

/// Error returned when a budget limit has been exceeded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetExceeded {
    pub spent_usd: f64,
    pub limit_usd: Option<f64>,
    pub total_tokens: u64,
    pub limit_tokens: Option<u64>,
}

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "budget exceeded: spent ${:.4}", self.spent_usd)?;
        if let Some(limit) = self.limit_usd {
            write!(f, " (limit ${limit:.4})")?;
        }
        write!(f, ", {} tokens", self.total_tokens)?;
        if let Some(limit) = self.limit_tokens {
            write!(f, " (limit {limit})")?;
        }
        Ok(())
    }
}

impl std::error::Error for BudgetExceeded {}

impl CostTracker {
    /// Record a single LLM call's usage and cost.
    pub fn record(&mut self, model: &str, usage: &Usage, pricing: &ModelPricing) {
        let cost = pricing.calculate(usage.prompt_tokens, usage.completion_tokens);
        self.record_with_cost(model, usage, cost);
    }

    /// Record a single LLM call with an explicit cost (e.g. the
    /// gateway's authoritative number from the `x-litellm-response-cost`
    /// response header). Prefer this over [`record`] whenever the
    /// gateway reports a cost — local `ModelPricing` can't price
    /// aliased routes (`smooth-coding` → unknown upstream) accurately.
    pub fn record_with_cost(&mut self, model: &str, usage: &Usage, cost_usd: f64) {
        self.total_prompt_tokens += u64::from(usage.prompt_tokens);
        self.total_completion_tokens += u64::from(usage.completion_tokens);
        self.total_cost_usd += cost_usd;
        self.calls += 1;

        self.entries.push(CostEntry {
            model: model.to_string(),
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cost_usd,
            timestamp: Utc::now(),
        });
    }

    /// Check whether the current totals exceed the given budget.
    ///
    /// # Errors
    /// Returns [`BudgetExceeded`] if either the USD or token limit is breached.
    pub fn check_budget(&self, budget: &CostBudget) -> Result<(), BudgetExceeded> {
        let total_tokens = self.total_prompt_tokens + self.total_completion_tokens;

        let usd_exceeded = budget.max_cost_usd.is_some_and(|limit| self.total_cost_usd > limit);
        let tokens_exceeded = budget.max_tokens.is_some_and(|limit| total_tokens > limit);

        if usd_exceeded || tokens_exceeded {
            return Err(BudgetExceeded {
                spent_usd: self.total_cost_usd,
                limit_usd: budget.max_cost_usd,
                total_tokens,
                limit_tokens: budget.max_tokens,
            });
        }

        Ok(())
    }

    /// Return all recorded cost entries.
    pub fn entries(&self) -> &[CostEntry] {
        &self.entries
    }

    /// Reset the tracker to its initial state.
    pub fn reset(&mut self) {
        self.total_prompt_tokens = 0;
        self.total_completion_tokens = 0;
        self.total_cost_usd = 0.0;
        self.calls = 0;
        self.entries.clear();
    }
}

impl ModelPricing {
    /// Calculate cost in USD for a given number of prompt and completion tokens.
    fn calculate(&self, prompt_tokens: u32, completion_tokens: u32) -> f64 {
        let prompt_cost = f64::from(prompt_tokens) * self.prompt_per_mtok / 1_000_000.0;
        let completion_cost = f64::from(completion_tokens) * self.completion_per_mtok / 1_000_000.0;
        prompt_cost + completion_cost
    }

    /// GPT-4o pricing.
    #[must_use]
    pub fn gpt_4o() -> Self {
        Self {
            prompt_per_mtok: 2.50,
            completion_per_mtok: 10.00,
        }
    }

    /// GPT-4o Mini pricing.
    #[must_use]
    pub fn gpt_4o_mini() -> Self {
        Self {
            prompt_per_mtok: 0.15,
            completion_per_mtok: 0.60,
        }
    }

    /// DeepSeek V3 pricing.
    #[must_use]
    pub fn deepseek_v3() -> Self {
        Self {
            prompt_per_mtok: 0.27,
            completion_per_mtok: 1.10,
        }
    }

    /// DeepSeek R1 pricing.
    #[must_use]
    pub fn deepseek_r1() -> Self {
        Self {
            prompt_per_mtok: 0.55,
            completion_per_mtok: 2.19,
        }
    }

    /// Gemini Flash pricing.
    #[must_use]
    pub fn gemini_flash() -> Self {
        Self {
            prompt_per_mtok: 0.075,
            completion_per_mtok: 0.30,
        }
    }

    /// Look up pricing for a model name, falling back to free tier for unknown models.
    #[must_use]
    pub fn for_model(model: &str) -> Self {
        let m = model.to_lowercase();
        if m.contains("gpt-4o-mini") {
            Self::gpt_4o_mini()
        } else if m.contains("gpt-4o") {
            Self::gpt_4o()
        } else if m.contains("deepseek") && m.contains("r1") {
            Self::deepseek_r1()
        } else if m.contains("deepseek") {
            Self::deepseek_v3()
        } else if m.contains("gemini") && m.contains("flash") {
            Self::gemini_flash()
        } else {
            Self::free()
        }
    }

    /// Free tier / local model pricing.
    #[must_use]
    pub fn free() -> Self {
        Self {
            prompt_per_mtok: 0.0,
            completion_per_mtok: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Usage;

    #[test]
    fn record_accumulates_tokens() {
        let mut tracker = CostTracker::default();
        let pricing = ModelPricing::gpt_4o();

        tracker.record(
            "gpt-4o",
            &Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            },
            &pricing,
        );
        tracker.record(
            "gpt-4o",
            &Usage {
                prompt_tokens: 200,
                completion_tokens: 100,
                total_tokens: 300,
            },
            &pricing,
        );

        assert_eq!(tracker.total_prompt_tokens, 300);
        assert_eq!(tracker.total_completion_tokens, 150);
        assert_eq!(tracker.calls, 2);
        assert_eq!(tracker.entries().len(), 2);
    }

    #[test]
    fn cost_calculation_accuracy() {
        // 1000 prompt tokens at $3/Mtok = $0.003
        let pricing = ModelPricing {
            prompt_per_mtok: 3.0,
            completion_per_mtok: 0.0,
        };
        let mut tracker = CostTracker::default();
        tracker.record(
            "test-model",
            &Usage {
                prompt_tokens: 1000,
                completion_tokens: 0,
                total_tokens: 1000,
            },
            &pricing,
        );

        let expected = 0.003;
        assert!(
            (tracker.total_cost_usd - expected).abs() < 1e-10,
            "expected {expected}, got {}",
            tracker.total_cost_usd
        );
    }

    #[test]
    fn check_budget_passes_when_under() {
        let mut tracker = CostTracker::default();
        tracker.record(
            "gpt-4o-mini",
            &Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            },
            &ModelPricing::gpt_4o_mini(),
        );

        let budget = CostBudget {
            max_cost_usd: Some(1.0),
            max_tokens: Some(1_000_000),
        };
        assert!(tracker.check_budget(&budget).is_ok());
    }

    #[test]
    fn check_budget_fails_on_usd_limit() {
        let mut tracker = CostTracker::default();
        // Use a pricing that makes 1000 tokens very expensive
        let pricing = ModelPricing {
            prompt_per_mtok: 1_000_000.0, // $1 per token
            completion_per_mtok: 0.0,
        };
        tracker.record(
            "expensive-model",
            &Usage {
                prompt_tokens: 100,
                completion_tokens: 0,
                total_tokens: 100,
            },
            &pricing,
        );

        let budget = CostBudget {
            max_cost_usd: Some(1.0),
            max_tokens: None,
        };
        let err = tracker.check_budget(&budget).unwrap_err();
        assert!(err.spent_usd > 1.0);
        assert_eq!(err.limit_usd, Some(1.0));
    }

    #[test]
    fn check_budget_fails_on_token_limit() {
        let mut tracker = CostTracker::default();
        tracker.record(
            "gpt-4o",
            &Usage {
                prompt_tokens: 5000,
                completion_tokens: 5000,
                total_tokens: 10000,
            },
            &ModelPricing::gpt_4o(),
        );

        let budget = CostBudget {
            max_cost_usd: None,
            max_tokens: Some(100),
        };
        let err = tracker.check_budget(&budget).unwrap_err();
        assert_eq!(err.total_tokens, 10000);
        assert_eq!(err.limit_tokens, Some(100));
    }

    #[test]
    fn model_pricing_presets_reasonable() {
        let gpt4o = ModelPricing::gpt_4o();
        assert!(gpt4o.prompt_per_mtok > 0.0);
        assert!(gpt4o.completion_per_mtok > gpt4o.prompt_per_mtok);

        let mini = ModelPricing::gpt_4o_mini();
        assert!(mini.prompt_per_mtok < gpt4o.prompt_per_mtok);
        assert!(mini.completion_per_mtok < gpt4o.completion_per_mtok);

        let free = ModelPricing::free();
        assert_eq!(free.prompt_per_mtok, 0.0);
        assert_eq!(free.completion_per_mtok, 0.0);

        let ds_v3 = ModelPricing::deepseek_v3();
        let ds_r1 = ModelPricing::deepseek_r1();
        assert!(ds_r1.prompt_per_mtok > ds_v3.prompt_per_mtok);

        let gemini = ModelPricing::gemini_flash();
        assert!(gemini.prompt_per_mtok > 0.0);
        assert!(gemini.prompt_per_mtok < mini.prompt_per_mtok);
    }

    #[test]
    fn cost_entry_timestamps_work() {
        let mut tracker = CostTracker::default();
        let before = Utc::now();

        tracker.record(
            "gpt-4o",
            &Usage {
                prompt_tokens: 10,
                completion_tokens: 10,
                total_tokens: 20,
            },
            &ModelPricing::gpt_4o(),
        );

        let after = Utc::now();
        let entry = &tracker.entries()[0];
        assert!(entry.timestamp >= before);
        assert!(entry.timestamp <= after);
        assert_eq!(entry.model, "gpt-4o");
    }

    #[test]
    fn budget_exceeded_serialization() {
        let err = BudgetExceeded {
            spent_usd: 5.50,
            limit_usd: Some(5.0),
            total_tokens: 100_000,
            limit_tokens: Some(50_000),
        };

        let json = serde_json::to_string(&err).expect("serialize");
        assert!(json.contains("5.5"));
        assert!(json.contains("100000"));

        let deserialized: BudgetExceeded = serde_json::from_str(&json).expect("deserialize");
        assert!((deserialized.spent_usd - 5.50).abs() < f64::EPSILON);
        assert_eq!(deserialized.limit_usd, Some(5.0));
        assert_eq!(deserialized.total_tokens, 100_000);
        assert_eq!(deserialized.limit_tokens, Some(50_000));

        // Display impl
        let display = format!("{err}");
        assert!(display.contains("budget exceeded"));
        assert!(display.contains("5.5"));
    }
}
