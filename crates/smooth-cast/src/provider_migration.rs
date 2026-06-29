//! `providers.json` migration shim for SMOODEV-1793.
//!
//! The Smoo AI LLM gateway is removing the `smooth-*` semantic-slot
//! aliases. Any user whose `~/.smooth/providers.json` references
//! `smooth-coding`, `smooth-reasoning`, … will get HTTP 400 from the
//! gateway after cutover.
//!
//! This module is the load-time rewrite layer:
//!
//! 1. [`migrate_provider_registry`] walks every routing slot on a
//!    `ProviderRegistry` and substitutes the concrete model name for
//!    any legacy `smooth-*` alias (see [`smooth_policy::smooth_alias`]).
//!    Returns the list of `(slot_name, old, new)` rewrites it made so
//!    the caller can log them and decide whether to save the file
//!    back.
//!
//! 2. [`load_providers_with_migration`] is the drop-in replacement for
//!    `ProviderRegistry::load_from_file`. It loads the file, runs the
//!    migration, **saves the file back to disk if anything changed**,
//!    and emits a `tracing::info!` line per rewrite. Callers that hit
//!    every chat-agent / coding-agent / Narc invocation should funnel
//!    through this entry point.
//!
//! Both functions are conservative on failure: a save error is logged
//! but does not block the returned registry — the in-memory migration
//! still applies, so the running process keeps working. The next
//! successful save flushes the rewrite to disk.

use std::path::Path;

use smooth_operator::providers::ProviderRegistry;
use smooth_policy::smooth_alias;

/// One alias rewrite produced by [`migrate_provider_registry`]: the
/// slot name (`"coding"`, `"reasoning"`, …), the legacy alias we
/// replaced, and the concrete model name we substituted in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasRewrite {
    pub slot: &'static str,
    pub old: String,
    pub new: String,
}

/// Walk the registry's routing slots and rewrite any legacy
/// `smooth-*` aliases to their concrete model names. Returns one
/// [`AliasRewrite`] per rewrite for caller-side logging.
///
/// Also rewrites the `default_model` field on every registered
/// provider config — older `providers.json` files have
/// `default_model: "smooth-default"` baked in.
pub fn migrate_provider_registry(registry: &mut ProviderRegistry) -> Vec<AliasRewrite> {
    let mut out = Vec::new();

    // Routing slots. We walk every slot present on disk including the
    // deprecated `planning` field so older configs are flushed clean.
    rewrite_slot("coding", &mut registry.routing.coding.model, &mut out);
    if let Some(ref mut s) = registry.routing.reasoning {
        rewrite_slot("reasoning", &mut s.model, &mut out);
    }
    rewrite_slot("reviewing", &mut registry.routing.reviewing.model, &mut out);
    rewrite_slot("judge", &mut registry.routing.judge.model, &mut out);
    rewrite_slot("summarize", &mut registry.routing.summarize.model, &mut out);
    rewrite_slot("default", &mut registry.routing.default.model, &mut out);
    if let Some(ref mut s) = registry.routing.fast {
        rewrite_slot("fast", &mut s.model, &mut out);
    }
    if let Some(ref mut s) = registry.routing.planning {
        rewrite_slot("planning", &mut s.model, &mut out);
    }

    // Fallback chains on each slot — same `model` field one level down.
    rewrite_fallback("coding.fallback", registry.routing.coding.fallback.as_deref_mut(), &mut out);
    if let Some(ref mut s) = registry.routing.reasoning {
        rewrite_fallback("reasoning.fallback", s.fallback.as_deref_mut(), &mut out);
    }
    rewrite_fallback("reviewing.fallback", registry.routing.reviewing.fallback.as_deref_mut(), &mut out);
    rewrite_fallback("judge.fallback", registry.routing.judge.fallback.as_deref_mut(), &mut out);
    rewrite_fallback("summarize.fallback", registry.routing.summarize.fallback.as_deref_mut(), &mut out);
    rewrite_fallback("default.fallback", registry.routing.default.fallback.as_deref_mut(), &mut out);
    if let Some(ref mut s) = registry.routing.fast {
        rewrite_fallback("fast.fallback", s.fallback.as_deref_mut(), &mut out);
    }

    out
}

fn rewrite_slot(slot_name: &'static str, model: &mut String, out: &mut Vec<AliasRewrite>) {
    if let Some(concrete) = smooth_alias::migrate_alias(model) {
        if model.as_str() != concrete {
            let old = std::mem::replace(model, concrete.to_string());
            out.push(AliasRewrite {
                slot: slot_name,
                old,
                new: concrete.to_string(),
            });
        }
    }
}

fn rewrite_fallback(slot_name: &'static str, slot: Option<&mut smooth_operator::providers::ModelSlot>, out: &mut Vec<AliasRewrite>) {
    let Some(fallback) = slot else { return };
    rewrite_slot(slot_name, &mut fallback.model, out);
    // Recurse one more level so a fallback-of-fallback also migrates.
    // Two levels is plenty in practice — the registry doesn't grow
    // deeper than that.
    if let Some(deeper) = fallback.fallback.as_deref_mut() {
        rewrite_slot(slot_name, &mut deeper.model, out);
    }
}

/// Drop-in replacement for `ProviderRegistry::load_from_file`.
///
/// Loads the registry, runs the [`migrate_provider_registry`] shim,
/// and saves the file back if anything changed. Logs each rewrite at
/// `info` level so users see the migration once and can audit it in
/// their session log.
///
/// A save failure is logged but does not propagate: the returned
/// registry still reflects the migration, so the running process
/// keeps working against the gateway's new model names.
///
/// # Errors
///
/// Propagates any error from
/// [`smooth_operator::providers::ProviderRegistry::load_from_file`] —
/// typically a missing file, malformed JSON, or an unreadable path. A
/// save failure during the on-disk rewrite is logged but not returned.
pub fn load_providers_with_migration(path: &Path) -> anyhow::Result<ProviderRegistry> {
    let mut registry = ProviderRegistry::load_from_file(path)?;
    let rewrites = migrate_provider_registry(&mut registry);
    if !rewrites.is_empty() {
        for r in &rewrites {
            tracing::info!(
                slot = r.slot,
                old = %r.old,
                new = %r.new,
                "migrated providers.json: {} smooth-* → {}",
                r.slot,
                r.new,
            );
        }
        // Best-effort flush so the user only sees the migration once.
        if let Err(e) = registry.save_to_file(path) {
            tracing::warn!(error = %e, "failed to save migrated providers.json — in-memory migration still applied");
        }
    }
    Ok(registry)
}

/// In-memory variant for callers that have a registry from JSON or
/// from a `from_preset` builder — no file I/O. Returns the rewrite
/// list so the caller can decide whether to log.
pub fn migrate_in_memory(registry: &mut ProviderRegistry) -> Vec<AliasRewrite> {
    migrate_provider_registry(registry)
}

#[cfg(test)]
mod tests {
    use smooth_operator::providers::{ModelRouting, ModelSlot, Preset, ProviderConfig};

    use super::*;

    fn legacy_registry() -> ProviderRegistry {
        let mut r = ProviderRegistry::new();
        r.register_provider(ProviderConfig {
            id: "smooai-gateway".into(),
            api_url: "https://llm.smoo.ai/v1".into(),
            api_key: "test".into(),
            api_format: smooth_operator::llm::ApiFormat::OpenAiCompat,
            default_model: "smooth-default".into(),
        });
        r.with_routing(ModelRouting {
            coding: ModelSlot::new("smooai-gateway", "smooth-coding"),
            reasoning: Some(ModelSlot::new("smooai-gateway", "smooth-reasoning")),
            reviewing: ModelSlot::new("smooai-gateway", "smooth-reviewing"),
            judge: ModelSlot::new("smooai-gateway", "smooth-judge"),
            summarize: ModelSlot::new("smooai-gateway", "smooth-summarize"),
            default: ModelSlot::new("smooai-gateway", "smooth-default"),
            fast: Some(ModelSlot::new("smooai-gateway", "smooth-fast")),
            planning: None,
        })
    }

    #[test]
    fn migrate_rewrites_every_slot() {
        let mut r = legacy_registry();
        let rewrites = migrate_provider_registry(&mut r);
        // 7 slots (coding, reasoning, reviewing, judge, summarize,
        // default, fast) — all start out as legacy aliases.
        assert_eq!(rewrites.len(), 7, "rewrites = {rewrites:?}");
        assert_eq!(r.routing.coding.model, "deepseek-v4-flash");
        assert_eq!(r.routing.reasoning.as_ref().unwrap().model, "deepseek-v4-pro");
        assert_eq!(r.routing.reviewing.model, "minimax-m2.7-direct");
        assert_eq!(r.routing.judge.model, "groq-gpt-oss-120b");
        assert_eq!(r.routing.summarize.model, "gemini-2.5-flash");
        assert_eq!(r.routing.default.model, "deepseek-v4-flash");
        assert_eq!(r.routing.fast.as_ref().unwrap().model, "groq-gpt-oss-20b");
    }

    #[test]
    fn migrate_idempotent() {
        let mut r = legacy_registry();
        let first = migrate_provider_registry(&mut r);
        assert!(!first.is_empty());
        let second = migrate_provider_registry(&mut r);
        assert!(second.is_empty(), "second pass made changes: {second:?}");
    }

    #[test]
    fn migrate_leaves_concrete_models_alone() {
        let mut r = ProviderRegistry::new();
        let routing = ModelRouting {
            coding: ModelSlot::new("openrouter", "deepseek/deepseek-chat"),
            reasoning: Some(ModelSlot::new("openrouter", "deepseek/deepseek-r1")),
            reviewing: ModelSlot::new("anthropic", "claude-sonnet-4"),
            judge: ModelSlot::new("google", "gemini-2.5-flash"),
            summarize: ModelSlot::new("google", "gemini-2.5-flash"),
            default: ModelSlot::new("openrouter", "deepseek/deepseek-chat"),
            fast: Some(ModelSlot::new("google", "gemini-2.5-flash-lite")),
            planning: None,
        };
        r = r.with_routing(routing);
        let rewrites = migrate_provider_registry(&mut r);
        assert!(rewrites.is_empty(), "concrete models triggered rewrites: {rewrites:?}");
    }

    #[test]
    fn migrate_handles_deprecated_planning_slot() {
        let mut r = ProviderRegistry::new();
        r = r.with_routing(ModelRouting {
            coding: ModelSlot::new("p", "smooth-coding"),
            reasoning: Some(ModelSlot::new("p", "smooth-reasoning")),
            reviewing: ModelSlot::new("p", "smooth-reviewing"),
            judge: ModelSlot::new("p", "smooth-judge"),
            summarize: ModelSlot::new("p", "smooth-summarize"),
            default: ModelSlot::new("p", "smooth-default"),
            fast: Some(ModelSlot::new("p", "smooth-fast")),
            planning: Some(ModelSlot::new("p", "smooth-planning")),
        });
        let rewrites = migrate_provider_registry(&mut r);
        assert_eq!(r.routing.planning.as_ref().unwrap().model, "deepseek-v4-pro", "planning folded into reasoning");
        assert!(rewrites.iter().any(|r| r.slot == "planning"));
    }

    #[test]
    fn migrate_rewrites_fallback_models() {
        let primary = ModelSlot::new("smooai-gateway", "smooth-coding").with_fallback(ModelSlot::new("smooai-gateway", "smooth-reasoning"));
        let mut r = ProviderRegistry::new().with_routing(ModelRouting {
            coding: primary,
            reasoning: Some(ModelSlot::new("p", "smooth-reasoning")),
            reviewing: ModelSlot::new("p", "smooth-reviewing"),
            judge: ModelSlot::new("p", "smooth-judge"),
            summarize: ModelSlot::new("p", "smooth-summarize"),
            default: ModelSlot::new("p", "smooth-default"),
            fast: Some(ModelSlot::new("p", "smooth-fast")),
            planning: None,
        });
        migrate_provider_registry(&mut r);
        assert_eq!(r.routing.coding.model, "deepseek-v4-flash");
        assert_eq!(r.routing.coding.fallback.as_ref().unwrap().model, "deepseek-v4-pro");
    }

    #[test]
    fn load_with_migration_round_trips_to_disk() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("providers.json");
        legacy_registry().save_to_file(&path).expect("seed file");

        // Load via the wrapper: should rewrite + save back.
        let loaded = load_providers_with_migration(&path).expect("load");
        assert_eq!(loaded.routing.coding.model, "deepseek-v4-flash");
        assert_eq!(loaded.routing.fast.as_ref().unwrap().model, "groq-gpt-oss-20b");

        // Read again with raw load_from_file — the file on disk must
        // now hold the concrete names too.
        let raw_reloaded = ProviderRegistry::load_from_file(&path).expect("reload");
        assert_eq!(raw_reloaded.routing.coding.model, "deepseek-v4-flash");
        assert_eq!(raw_reloaded.routing.reasoning.as_ref().unwrap().model, "deepseek-v4-pro");
        assert_eq!(raw_reloaded.routing.judge.model, "groq-gpt-oss-120b");
    }

    #[test]
    fn load_with_migration_is_noop_for_clean_file() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("providers.json");
        // Seed with concrete models — no migration needed.
        let mut r = ProviderRegistry::from_preset(Preset::OpenRouterLowCost, "k");
        // Snap an explicit non-smooth model into one slot for clarity.
        r.routing.coding.model = "deepseek/deepseek-chat".into();
        r.save_to_file(&path).expect("seed");
        let before = std::fs::read_to_string(&path).expect("read");
        let _loaded = load_providers_with_migration(&path).expect("load");
        let after = std::fs::read_to_string(&path).expect("re-read");
        assert_eq!(before, after, "load with no migration must not rewrite the file");
    }

    #[test]
    fn alias_rewrite_records_old_and_new() {
        let mut r = legacy_registry();
        let rewrites = migrate_provider_registry(&mut r);
        let coding = rewrites.iter().find(|r| r.slot == "coding").expect("coding rewrite");
        assert_eq!(coding.old, "smooth-coding");
        assert_eq!(coding.new, "deepseek-v4-flash");
        let fast = rewrites.iter().find(|r| r.slot == "fast").expect("fast rewrite");
        assert_eq!(fast.old, "smooth-fast");
        assert_eq!(fast.new, "groq-gpt-oss-20b");
    }

    /// SMOODEV-2097: a config that already ran the smooth-* migration is
    /// pinned to the *concrete* Groq Llama names. The gateway then
    /// removed those models, so the second migration step must bump them
    /// to gpt-oss — even though they carry no `smooth-` prefix.
    #[test]
    fn migrate_bumps_already_migrated_groq_llama_to_gpt_oss() {
        let mut r = ProviderRegistry::new().with_routing(ModelRouting {
            coding: ModelSlot::new("smooai-gateway", "deepseek-v4-flash"),
            reasoning: Some(ModelSlot::new("smooai-gateway", "deepseek-v4-pro")),
            reviewing: ModelSlot::new("smooai-gateway", "minimax-m2.7-direct"),
            judge: ModelSlot::new("smooai-gateway", "groq-llama-3.3-70b"),
            summarize: ModelSlot::new("smooai-gateway", "gemini-2.5-flash"),
            default: ModelSlot::new("smooai-gateway", "deepseek-v4-flash"),
            fast: Some(ModelSlot::new("smooai-gateway", "groq-llama-3.1-8b")),
            planning: None,
        });
        let rewrites = migrate_provider_registry(&mut r);
        // Only judge + fast change; the rest were already live concrete
        // names.
        assert_eq!(rewrites.len(), 2, "rewrites = {rewrites:?}");
        assert_eq!(r.routing.judge.model, "groq-gpt-oss-120b");
        assert_eq!(r.routing.fast.as_ref().unwrap().model, "groq-gpt-oss-20b");
        let judge = rewrites.iter().find(|r| r.slot == "judge").expect("judge rewrite");
        assert_eq!(judge.old, "groq-llama-3.3-70b");
        assert_eq!(judge.new, "groq-gpt-oss-120b");
        // Idempotent: a second pass makes no further changes.
        assert!(migrate_provider_registry(&mut r).is_empty());
    }
}
