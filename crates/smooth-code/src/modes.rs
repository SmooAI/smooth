//! Smooth Modes — the model lineup the TUI's `/smooth-mode` switcher drives.
//!
//! Each mode pins a turn to a specific model; budget modes are the
//! daily-driver defaults, premium modes the "spend real money" tier. Cost is
//! surfaced live on the status bar so switching into something pricey is a
//! deliberate, visible act (th-f512b1, th-2a6330).
//!
//! This is the Rust mirror of `crates/smooth-web/web/src/modes.ts` — the
//! preset table, default mode, and `cost_badge` buckets are kept byte-for-byte
//! identical so the web console and `th code` agree on what every mode costs.

use std::collections::HashMap;

use serde::Deserialize;

/// Pricing tier of a mode preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// The everyday tier — cheap models, run freely.
    Budget,
    /// The "spend real money" tier — premium frontier models.
    Premium,
}

impl Tier {
    /// Lowercase string form, matching `ModeTier` in the web mirror.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::Premium => "premium",
        }
    }
}

/// A single mode preset: a named shortcut to a concrete model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// Stable id used by `/smooth-mode <id>` (e.g. `flash`, `code+`, `max`).
    pub id: &'static str,
    /// Short human label shown in the status bar + picker.
    pub label: &'static str,
    /// A glyph that reads the mode at a glance.
    pub emoji: &'static str,
    /// The model id sent on `send_message`.
    pub model: &'static str,
    /// Budget vs. premium.
    pub tier: Tier,
}

/// The full lineup, budget first then premium — also the picker order.
/// Mirrors `MODES` in `modes.ts` exactly.
pub const MODES: &[Mode] = &[
    // Budget — the everyday tier.
    Mode {
        id: "flash",
        label: "Flash",
        emoji: "⚡",
        model: "deepseek-v4-flash",
        tier: Tier::Budget,
    },
    Mode {
        id: "code",
        label: "Code",
        emoji: "💻",
        model: "minimax-m2.7",
        tier: Tier::Budget,
    },
    Mode {
        id: "ui",
        label: "UI",
        emoji: "🎨",
        model: "glm-5.1",
        tier: Tier::Budget,
    },
    Mode {
        id: "plan",
        label: "Plan",
        emoji: "🧠",
        model: "deepseek-v4-pro",
        tier: Tier::Budget,
    },
    Mode {
        id: "fast",
        label: "Fast",
        emoji: "🏎️",
        model: "groq-gpt-oss-20b",
        tier: Tier::Budget,
    },
    // Premium — the "spend real money" tier.
    Mode {
        id: "flash+",
        label: "Flash+",
        emoji: "⚡",
        model: "gemini-3.5-flash",
        tier: Tier::Premium,
    },
    Mode {
        id: "code+",
        label: "Code+",
        emoji: "💻",
        model: "claude-opus-4-8",
        tier: Tier::Premium,
    },
    Mode {
        id: "ui+",
        label: "UI+",
        emoji: "🎨",
        model: "gpt-5.5",
        tier: Tier::Premium,
    },
    Mode {
        id: "plan+",
        label: "Plan+",
        emoji: "🧠",
        model: "gpt-5.4",
        tier: Tier::Premium,
    },
    Mode {
        id: "max",
        label: "Max",
        emoji: "💎",
        model: "gpt-5.5-pro",
        tier: Tier::Premium,
    },
];

/// The mode a fresh session lands on.
pub const DEFAULT_MODE_ID: &str = "flash";

/// The default [`Mode`] — `flash`, the first entry in [`MODES`]. Infallible:
/// `MODES` is a non-empty const table whose first entry is `DEFAULT_MODE_ID`
/// (asserted by a unit test).
#[must_use]
pub fn default_mode() -> &'static Mode {
    &MODES[0]
}

/// Look a mode up by id, falling back to the default ([`default_mode`]) when
/// the id is unknown. Total — never panics.
#[must_use]
pub fn mode_by_id(id: &str) -> &'static Mode {
    MODES.iter().find(|m| m.id == id).unwrap_or_else(default_mode)
}

/// Per-token costs from `GET /admin/model-costs`, keyed by model id.
/// Mirrors `ModelCost` in the web `modes.ts`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    /// USD charged per input (prompt) token.
    pub input_cost_per_token: f64,
    /// USD charged per output (completion) token.
    pub output_cost_per_token: f64,
}

/// The full `/admin/model-costs` table keyed by model id.
pub type ModelCosts = HashMap<String, ModelCost>;

/// Blended $/1M-token rate — the number behind the badge.
#[must_use]
pub fn blended_per_million(input_cost_per_token: f64, output_cost_per_token: f64) -> f64 {
    f64::midpoint(input_cost_per_token, output_cost_per_token) * 1e6
}

/// A traffic-light glyph for a model's blended $/1M-token rate.
/// 💚 <$1, 💛 $1–5, 🧡 $5–30, ❤️ >$30. Buckets mirror `costBadge` in
/// `modes.ts` exactly (`< 1`, `< 5`, `<= 30`, else).
#[must_use]
pub fn cost_badge(input_cost_per_token: f64, output_cost_per_token: f64) -> &'static str {
    let blended = blended_per_million(input_cost_per_token, output_cost_per_token);
    if blended < 1.0 {
        "💚"
    } else if blended < 5.0 {
        "💛"
    } else if blended <= 30.0 {
        "🧡"
    } else {
        "❤️"
    }
}

/// A mode is "expensive" when its badge is 🧡 or ❤️ (≥ $5/1M blended).
#[must_use]
pub fn is_expensive_badge(badge: &str) -> bool {
    badge == "🧡" || badge == "❤️"
}

/// The traffic-light badge for the active `mode`, or `None` when its cost is
/// unknown (the model-costs table is empty or lacks this model). Mirrors
/// `modeBadge` in `App.tsx`.
#[must_use]
pub fn mode_badge(mode: &Mode, costs: &ModelCosts) -> Option<&'static str> {
    costs.get(mode.model).map(|c| cost_badge(c.input_cost_per_token, c.output_cost_per_token))
}

/// Whether switching to `mode` should warn the user. Expensive when the badge
/// is 🧡/❤️; falls back to the mode's tier when cost is unknown. Mirrors
/// `modeExpensive` in `App.tsx`.
#[must_use]
pub fn mode_expensive(mode: &Mode, costs: &ModelCosts) -> bool {
    mode_badge(mode, costs).map_or(mode.tier == Tier::Premium, is_expensive_badge)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "expect/unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn table_matches_web_lineup_exactly() {
        // Order, ids, models, and tiers must match modes.ts byte-for-byte so
        // the web console and the TUI never disagree about a preset.
        let expected: &[(&str, &str, &str, Tier)] = &[
            ("flash", "Flash", "deepseek-v4-flash", Tier::Budget),
            ("code", "Code", "minimax-m2.7", Tier::Budget),
            ("ui", "UI", "glm-5.1", Tier::Budget),
            ("plan", "Plan", "deepseek-v4-pro", Tier::Budget),
            ("fast", "Fast", "groq-gpt-oss-20b", Tier::Budget),
            ("flash+", "Flash+", "gemini-3.5-flash", Tier::Premium),
            ("code+", "Code+", "claude-opus-4-8", Tier::Premium),
            ("ui+", "UI+", "gpt-5.5", Tier::Premium),
            ("plan+", "Plan+", "gpt-5.4", Tier::Premium),
            ("max", "Max", "gpt-5.5-pro", Tier::Premium),
        ];
        assert_eq!(MODES.len(), expected.len());
        for (mode, (id, label, model, tier)) in MODES.iter().zip(expected) {
            assert_eq!(mode.id, *id);
            assert_eq!(mode.label, *label);
            assert_eq!(mode.model, *model);
            assert_eq!(mode.tier, *tier);
            assert!(!mode.emoji.is_empty(), "every mode needs a glyph: {}", mode.id);
        }
    }

    #[test]
    fn mode_ids_are_unique() {
        let mut ids: Vec<&str> = MODES.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "duplicate mode id in MODES");
    }

    #[test]
    fn default_mode_is_flash_and_present() {
        assert_eq!(DEFAULT_MODE_ID, "flash");
        // The first table entry must be the default — `default_mode` returns
        // `MODES[0]` directly, so this backs that contract.
        assert_eq!(MODES[0].id, DEFAULT_MODE_ID);
        assert_eq!(default_mode().id, "flash");
        assert_eq!(default_mode().model, "deepseek-v4-flash");
    }

    #[test]
    fn mode_by_id_falls_back_to_default_for_unknown() {
        assert_eq!(mode_by_id("code+").id, "code+");
        assert_eq!(mode_by_id("nope").id, DEFAULT_MODE_ID);
        assert_eq!(mode_by_id("").id, DEFAULT_MODE_ID);
    }

    #[test]
    fn cost_badge_buckets() {
        // blended = (in + out) / 2 * 1e6. Drive the blended value directly by
        // setting in == out so blended == in * 1e6.
        let badge_for = |blended_per_m: f64| {
            let per_token = blended_per_m / 1e6;
            cost_badge(per_token, per_token)
        };
        assert_eq!(badge_for(0.0), "💚");
        assert_eq!(badge_for(0.99), "💚");
        // Boundary at $1 — exactly 1.0 is no longer 💚.
        assert_eq!(badge_for(1.0), "💛");
        assert_eq!(badge_for(4.99), "💛");
        // Boundary at $5 — `< 5` for 💛, so exactly 5.0 is 🧡.
        assert_eq!(badge_for(5.0), "🧡");
        assert_eq!(badge_for(30.0), "🧡");
        // Boundary at $30 — `<= 30` for 🧡, so just past 30 is ❤️.
        assert_eq!(badge_for(30.01), "❤️");
        assert_eq!(badge_for(200.0), "❤️");
    }

    #[test]
    fn cost_badge_blends_asymmetric_in_out() {
        // input cheap, output pricey → blended is the average. (1 + 9)/2 = 5 →
        // exactly the 🧡 boundary.
        assert_eq!(cost_badge(1.0 / 1e6, 9.0 / 1e6), "🧡");
        // (0.5 + 1.0)/2 = 0.75 → 💚.
        assert_eq!(cost_badge(0.5 / 1e6, 1.0 / 1e6), "💚");
    }

    #[test]
    fn is_expensive_badge_only_orange_and_red() {
        assert!(!is_expensive_badge("💚"));
        assert!(!is_expensive_badge("💛"));
        assert!(is_expensive_badge("🧡"));
        assert!(is_expensive_badge("❤️"));
    }

    #[test]
    fn mode_expensive_falls_back_to_tier_without_costs() {
        let costs = ModelCosts::new();
        // Budget mode, no cost data → not expensive (tier fallback).
        assert!(!mode_expensive(mode_by_id("flash"), &costs));
        // Premium mode, no cost data → expensive (tier fallback).
        assert!(mode_expensive(mode_by_id("max"), &costs));
    }

    #[test]
    fn mode_expensive_prefers_cost_over_tier() {
        let mut costs = ModelCosts::new();
        // Give the premium `max` model a dirt-cheap rate (blended $0.2/1M) →
        // 💚 → not expensive, overriding its premium tier.
        costs.insert(
            "gpt-5.5-pro".to_string(),
            ModelCost {
                input_cost_per_token: 0.1 / 1e6,
                output_cost_per_token: 0.3 / 1e6,
            },
        );
        assert!(!mode_expensive(mode_by_id("max"), &costs));

        // Give the budget `flash` model a pricey rate (blended $50/1M) → ❤️ →
        // expensive, overriding its budget tier.
        costs.insert(
            "deepseek-v4-flash".to_string(),
            ModelCost {
                input_cost_per_token: 40.0 / 1e6,
                output_cost_per_token: 60.0 / 1e6,
            },
        );
        assert!(mode_expensive(mode_by_id("flash"), &costs));
    }

    #[test]
    fn model_cost_deserializes_camel_case() {
        let json = r#"{"deepseek-v4-flash":{"inputCostPerToken":2e-7,"outputCostPerToken":8e-7,"tier":"budget","useCases":["chat"]}}"#;
        let costs: ModelCosts = serde_json::from_str(json).expect("deserialize model costs");
        let c = costs.get("deepseek-v4-flash").expect("model present");
        assert!((c.input_cost_per_token - 2e-7).abs() < f64::EPSILON);
        assert!((c.output_cost_per_token - 8e-7).abs() < f64::EPSILON);
        // Extra fields (tier, useCases) are ignored without erroring.
    }

    #[test]
    fn tier_as_str() {
        assert_eq!(Tier::Budget.as_str(), "budget");
        assert_eq!(Tier::Premium.as_str(), "premium");
    }
}
