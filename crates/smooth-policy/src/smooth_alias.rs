//! Legacy `smooth-*` semantic-slot alias migration.
//!
//! The Smoo AI LLM gateway (`https://llm.smoo.ai/v1`) used to publish a
//! family of routing aliases — `smooth-coding`, `smooth-reasoning`,
//! `smooth-judge`, `smooth-fast`, `smooth-default`, plus per-family
//! sub-aliases like `smooth-fast-gemini`, `smooth-coding-kimi`, etc.
//! Those aliases are being removed at the gateway (SMOODEV-1793). After
//! cutover any request for a `smooth-*` model name returns HTTP 400
//! `Invalid model name`.
//!
//! This module is the single source of truth for "what concrete model
//! does each legacy slot alias map to today?". It is **pure strings, no
//! external deps** — every consumer crate that loads a provider registry
//! can wire it in without pulling new transitive deps.
//!
//! ## Mapping table (June 2026)
//!
//! | Old slot                      | Concrete model_name        |
//! |-------------------------------|----------------------------|
//! | `smooth-coding`               | `deepseek-v4-flash`        |
//! | `smooth-reasoning`            | `deepseek-v4-pro`          |
//! | `smooth-reviewing`            | `minimax-m2.7-direct`      |
//! | `smooth-judge`                | `gemini-2.5-flash`         |
//! | `smooth-summarize`            | `gemini-2.5-flash`         |
//! | `smooth-fast`                 | `gemini-2.5-flash-lite`    |
//! | `smooth-default`              | (alias of coding)          |
//! | `smooth-planning` (deprecated)| (alias of reasoning)       |
//! | `smooth-thinking` (deprecated)| (alias of reasoning)       |
//!
//! Plus all known per-family sub-aliases collapse to their slot default
//! (e.g. `smooth-coding-qwen`, `smooth-judge-haiku`, …). The picker
//! still lets the user pin a specific concrete model — these mappings
//! only kick in for users whose `providers.json` references a stale
//! alias.

/// The seven canonical routing-slot names this migration knows about.
///
/// Lower-cased to match the picker's display labels and the
/// `Activity` enum from `smooth-operator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothSlot {
    Coding,
    Reasoning,
    Reviewing,
    Judge,
    Summarize,
    Fast,
    Default,
}

impl SmoothSlot {
    /// Concrete model the gateway routes this slot to by default after
    /// cutover. This is the value we substitute in for `smooth-<slot>`.
    #[must_use]
    pub const fn concrete_default(self) -> &'static str {
        match self {
            // `default` is the wire-compat fallback served by the coding
            // route — keep it pinned to the same concrete model so the
            // two stay in sync.
            Self::Coding | Self::Default => "deepseek-v4-flash",
            Self::Reasoning => "deepseek-v4-pro",
            Self::Reviewing => "minimax-m2.7-direct",
            Self::Judge | Self::Summarize => "gemini-2.5-flash",
            Self::Fast => "gemini-2.5-flash-lite",
        }
    }

    /// Slot name as used in `providers.json` routing keys.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Reasoning => "reasoning",
            Self::Reviewing => "reviewing",
            Self::Judge => "judge",
            Self::Summarize => "summarize",
            Self::Fast => "fast",
            Self::Default => "default",
        }
    }
}

/// All seven slots in fixed display order. Picker UIs render in this
/// order; the migration walks them in this order too.
pub const ALL_SLOTS: &[SmoothSlot] = &[
    SmoothSlot::Coding,
    SmoothSlot::Reasoning,
    SmoothSlot::Reviewing,
    SmoothSlot::Judge,
    SmoothSlot::Summarize,
    SmoothSlot::Fast,
    SmoothSlot::Default,
];

/// Map a legacy `smooth-*` model name to the concrete gateway model it
/// should be rewritten to. Returns `None` for anything that isn't a
/// known legacy alias (concrete model names pass through untouched).
///
/// The lookup is case-insensitive on the prefix but exact on the rest,
/// matching the gateway's old route names.
#[must_use]
pub fn migrate_alias(model: &str) -> Option<&'static str> {
    let lower = model.to_ascii_lowercase();
    let stripped = lower.strip_prefix("smooth-")?;

    // Exact slot aliases.
    if let Some(slot) = match_slot_exact(stripped) {
        return Some(slot.concrete_default());
    }

    // `smooth-<slot>-<vendor>` sub-aliases (e.g. `smooth-coding-kimi`,
    // `smooth-fast-gemini`, `smooth-judge-haiku`). The longest matching
    // slot prefix wins so `smooth-fast-gemini` resolves via Fast, not
    // via a stray substring of another slot.
    for slot in ALL_SLOTS {
        let prefix = slot.name();
        if let Some(rest) = stripped.strip_prefix(prefix) {
            // Must be followed by `-` so `summary` doesn't accidentally
            // match `summarize`. Also accept exact slot name with no
            // suffix, which match_slot_exact handles above.
            if rest.starts_with('-') {
                return Some(slot.concrete_default());
            }
        }
    }

    // Deprecated slots that fold into reasoning.
    if stripped == "planning" || stripped == "thinking" || stripped.starts_with("planning-") || stripped.starts_with("thinking-") {
        return Some(SmoothSlot::Reasoning.concrete_default());
    }

    None
}

fn match_slot_exact(stripped: &str) -> Option<SmoothSlot> {
    Some(match stripped {
        "coding" => SmoothSlot::Coding,
        "reasoning" => SmoothSlot::Reasoning,
        "reviewing" => SmoothSlot::Reviewing,
        "judge" => SmoothSlot::Judge,
        "summarize" => SmoothSlot::Summarize,
        "fast" => SmoothSlot::Fast,
        "default" => SmoothSlot::Default,
        _ => return None,
    })
}

/// True iff `model` is one of the legacy `smooth-*` aliases (i.e.
/// [`migrate_alias`] would return `Some`).
#[must_use]
pub fn is_smooth_alias(model: &str) -> bool {
    migrate_alias(model).is_some()
}

/// Rewrite a model name in place if it's a legacy `smooth-*` alias.
/// Returns `true` when a substitution happened. Used by the in-memory
/// migration that runs on every `providers.json` load.
pub fn migrate_in_place(model: &mut String) -> bool {
    if let Some(concrete) = migrate_alias(model) {
        if model.as_str() != concrete {
            *model = concrete.to_string();
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_slot_aliases_map_to_concrete_defaults() {
        assert_eq!(migrate_alias("smooth-coding"), Some("deepseek-v4-flash"));
        assert_eq!(migrate_alias("smooth-reasoning"), Some("deepseek-v4-pro"));
        assert_eq!(migrate_alias("smooth-reviewing"), Some("minimax-m2.7-direct"));
        assert_eq!(migrate_alias("smooth-judge"), Some("gemini-2.5-flash"));
        assert_eq!(migrate_alias("smooth-summarize"), Some("gemini-2.5-flash"));
        assert_eq!(migrate_alias("smooth-fast"), Some("gemini-2.5-flash-lite"));
        assert_eq!(migrate_alias("smooth-default"), Some("deepseek-v4-flash"));
    }

    #[test]
    fn deprecated_planning_and_thinking_fold_to_reasoning() {
        assert_eq!(migrate_alias("smooth-planning"), Some("deepseek-v4-pro"));
        assert_eq!(migrate_alias("smooth-thinking"), Some("deepseek-v4-pro"));
        assert_eq!(migrate_alias("smooth-thinking-kimi"), Some("deepseek-v4-pro"));
    }

    #[test]
    fn sub_aliases_map_to_slot_concrete_default() {
        assert_eq!(migrate_alias("smooth-fast-gemini"), Some("gemini-2.5-flash-lite"));
        assert_eq!(migrate_alias("smooth-fast-haiku"), Some("gemini-2.5-flash-lite"));
        assert_eq!(migrate_alias("smooth-fast-gpt"), Some("gemini-2.5-flash-lite"));
        assert_eq!(migrate_alias("smooth-judge-gemini"), Some("gemini-2.5-flash"));
        assert_eq!(migrate_alias("smooth-judge-haiku"), Some("gemini-2.5-flash"));
        assert_eq!(migrate_alias("smooth-judge-gpt"), Some("gemini-2.5-flash"));
        assert_eq!(migrate_alias("smooth-summarize-gemini"), Some("gemini-2.5-flash"));
        assert_eq!(migrate_alias("smooth-summarize-gpt"), Some("gemini-2.5-flash"));
        assert_eq!(migrate_alias("smooth-summarize-qwen"), Some("gemini-2.5-flash"));
        assert_eq!(migrate_alias("smooth-coding-qwen"), Some("deepseek-v4-flash"));
        assert_eq!(migrate_alias("smooth-coding-glm"), Some("deepseek-v4-flash"));
        assert_eq!(migrate_alias("smooth-coding-kimi"), Some("deepseek-v4-flash"));
        assert_eq!(migrate_alias("smooth-coding-minimax"), Some("deepseek-v4-flash"));
        assert_eq!(migrate_alias("smooth-reasoning-kimi"), Some("deepseek-v4-pro"));
        assert_eq!(migrate_alias("smooth-reasoning-deepseek"), Some("deepseek-v4-pro"));
        assert_eq!(migrate_alias("smooth-reasoning-qwen"), Some("deepseek-v4-pro"));
        assert_eq!(migrate_alias("smooth-reviewing-minimax"), Some("minimax-m2.7-direct"));
        assert_eq!(migrate_alias("smooth-reviewing-qwen-coder"), Some("minimax-m2.7-direct"));
    }

    #[test]
    fn unknown_aliases_return_none() {
        // `smooth-` prefix but unknown slot name.
        assert_eq!(migrate_alias("smooth-bogus"), None);
        assert_eq!(migrate_alias("smooth-bogus-gemini"), None);
        // No `smooth-` prefix at all.
        assert_eq!(migrate_alias("gemini-2.5-flash"), None);
        assert_eq!(migrate_alias("deepseek-v4-flash"), None);
        assert_eq!(migrate_alias("claude-haiku-4-5"), None);
        assert_eq!(migrate_alias(""), None);
    }

    #[test]
    fn is_smooth_alias_matches_migrate_alias() {
        assert!(is_smooth_alias("smooth-coding"));
        assert!(is_smooth_alias("smooth-fast-gemini"));
        assert!(is_smooth_alias("smooth-thinking"));
        assert!(!is_smooth_alias("deepseek-v4-flash"));
        assert!(!is_smooth_alias("smooth-bogus"));
    }

    #[test]
    fn migrate_in_place_rewrites_only_legacy_aliases() {
        let mut s = "smooth-coding".to_string();
        assert!(migrate_in_place(&mut s));
        assert_eq!(s, "deepseek-v4-flash");

        let mut s = "deepseek-v4-flash".to_string();
        assert!(!migrate_in_place(&mut s));
        assert_eq!(s, "deepseek-v4-flash");

        let mut s = "gpt-4o".to_string();
        assert!(!migrate_in_place(&mut s));
        assert_eq!(s, "gpt-4o");
    }

    #[test]
    fn migrate_in_place_returns_false_when_already_concrete() {
        // Sanity: if someone calls us with a non-alias we don't bother
        // doing the work. The bool guards the "save back to disk" path
        // in the load wrapper, so a false negative here would cause
        // spurious disk writes.
        let mut s = "deepseek-v4-pro".to_string();
        assert!(!migrate_in_place(&mut s));
    }

    #[test]
    fn case_insensitive_prefix_match() {
        assert_eq!(migrate_alias("SMOOTH-CODING"), Some("deepseek-v4-flash"));
        assert_eq!(migrate_alias("Smooth-Reasoning"), Some("deepseek-v4-pro"));
    }

    #[test]
    fn slot_name_round_trips() {
        for slot in ALL_SLOTS {
            assert!(!slot.name().is_empty());
            assert!(!slot.concrete_default().is_empty());
        }
    }

    /// Regression: `smooth-default` must map to the same concrete
    /// model as `smooth-coding`, since the default slot is served by
    /// the coding route at the gateway. If these diverge, the
    /// fallback path breaks for callers that hit the default slot
    /// directly.
    #[test]
    fn default_and_coding_map_to_same_model() {
        assert_eq!(migrate_alias("smooth-default"), migrate_alias("smooth-coding"));
    }
}
