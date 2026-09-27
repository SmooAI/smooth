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
//! [`load_providers_with_migration`] also retires, ONCE per config file,
//! the *concrete* names earlier migrations pinned as slot defaults
//! (`gpt-5.6-luna`, `gemini-3.5-flash`, …) that are outside the SMOODEV-3342
//! model policy — only on slots routed to the Smoo gateway, where those names
//! were ours; see [`retire_gateway_defaults`]. Once, because after it runs
//! nothing distinguishes a leftover default from a model the user then picked
//! on purpose, and a re-pick must stick.
//!
//! Both functions are conservative on failure: a save error is logged
//! but does not block the returned registry — the in-memory migration
//! still applies, so the running process keeps working. The next
//! successful save flushes the rewrite to disk.

use std::path::Path;

use smooth_operator::providers::{ModelSlot, ProviderRegistry};
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

/// True when a provider is the Smoo AI gateway: the id `th model login`
/// stamps, or any provider pointed at `llm.smoo.ai` (installs from before the
/// id was standardised call it `smooth`).
fn is_gateway_provider(id: &str, api_url: &str) -> bool {
    id == "smooai-gateway" || api_url.contains("llm.smoo.ai")
}

/// SMOODEV-3342: rewrite retired gateway defaults on every slot (and its
/// fallback chain) that routes to the Smoo gateway. A slot on any other
/// provider keeps its model — `gemini-2.5-flash` on a Google key is the
/// user's choice, not a default we wrote.
///
/// Callers run this once per config (see [`load_providers_with_migration`]);
/// it cannot tell a leftover default from a deliberate later pick.
pub fn retire_gateway_defaults(registry: &mut ProviderRegistry) -> Vec<AliasRewrite> {
    let mut out = Vec::new();
    retire_gateway_defaults_into(registry, &mut out);
    out
}

fn retire_gateway_defaults_into(registry: &mut ProviderRegistry, out: &mut Vec<AliasRewrite>) {
    let gateway: std::collections::HashSet<String> = {
        let r = &registry.routing;
        let mut ids = vec![
            &r.coding.provider,
            &r.reviewing.provider,
            &r.judge.provider,
            &r.summarize.provider,
            &r.default.provider,
        ];
        ids.extend([&r.reasoning, &r.fast, &r.planning].into_iter().flatten().map(|s| &s.provider));
        ids.into_iter()
            .filter(|id| registry.get_provider(id).is_some_and(|p| is_gateway_provider(&p.id, &p.api_url)))
            .cloned()
            .collect()
    };
    if gateway.is_empty() {
        return;
    }
    let routing = &mut registry.routing;
    let mut slots: Vec<(&'static str, &mut ModelSlot)> = vec![
        ("coding", &mut routing.coding),
        ("reviewing", &mut routing.reviewing),
        ("judge", &mut routing.judge),
        ("summarize", &mut routing.summarize),
        ("default", &mut routing.default),
    ];
    for (name, slot) in [
        ("reasoning", &mut routing.reasoning),
        ("fast", &mut routing.fast),
        ("planning", &mut routing.planning),
    ] {
        if let Some(s) = slot.as_mut() {
            slots.push((name, s));
        }
    }
    for (name, slot) in slots {
        let mut cur = Some(slot);
        while let Some(s) = cur {
            if gateway.contains(&s.provider) {
                if let Some(new) = smooth_alias::retired_gateway_default(&s.model) {
                    let old = std::mem::replace(&mut s.model, new.to_string());
                    out.push(AliasRewrite {
                        slot: name,
                        old,
                        new: new.to_string(),
                    });
                }
            }
            cur = s.fallback.as_deref_mut();
        }
    }
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
    let mut rewrites = migrate_provider_registry(&mut registry);
    let marker = retirement_marker(path);
    let retire = !marker.exists();
    if retire {
        retire_gateway_defaults_into(&mut registry, &mut rewrites);
    }
    if !rewrites.is_empty() {
        for r in &rewrites {
            tracing::info!(
                slot = r.slot,
                old = %r.old,
                new = %r.new,
                "migrated providers.json: {} {} → {}",
                r.slot,
                r.old,
                r.new,
            );
        }
        // Best-effort flush so the user only sees the migration once.
        //
        // Save via raw JSON, NOT `registry.save_to_file`: the typed
        // serializer drops any field the published `ProviderConfig` lacks
        // — including per-provider `max_tokens`. Rewriting the file as a
        // `Value` preserves those. If the raw path fails to load/parse we
        // fall back to the typed save (correct model names, no worse than
        // before on the unknown-field front).
        match crate::providers::load_value(path) {
            Ok(mut root) => {
                migrate_value_in_place(&mut root);
                if retire {
                    retire_gateway_defaults_value(&mut root);
                }
                if let Err(e) = crate::providers::save_value(path, &root) {
                    tracing::warn!(error = %e, "failed to save migrated providers.json — in-memory migration still applied");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "raw providers.json reload failed; falling back to typed save (drops unknown fields)");
                if let Err(e) = registry.save_to_file(path) {
                    tracing::warn!(error = %e, "failed to save migrated providers.json — in-memory migration still applied");
                }
            }
        }
    }
    if retire {
        // Record that this file's retired defaults were handled, so a model the
        // user picks from now on is never rewritten. Best-effort: a missing
        // marker only means the (idempotent) retirement runs again next load.
        if let Err(e) = std::fs::write(&marker, "SMOODEV-3342: retired gateway slot defaults migrated\n") {
            tracing::debug!(error = %e, path = %marker.display(), "could not write the model-policy marker");
        }
    }
    Ok(registry)
}

/// The sidecar that records [`retire_gateway_defaults`] already ran for a
/// `providers.json`: `<dir>/.<file name>.model-policy-3342`. A sidecar, not a
/// key in the file, because the typed `save_to_file` drops unknown keys.
fn retirement_marker(path: &Path) -> std::path::PathBuf {
    let name = path.file_name().map_or_else(|| "providers.json".into(), |n| n.to_string_lossy().into_owned());
    path.with_file_name(format!(".{name}.model-policy-3342"))
}

/// Apply the `smooth-*` alias migration to a raw providers.json `Value`,
/// preserving unknown fields (per-provider `max_tokens`, etc.). Rewrites
/// every string under a `model` or `default_model` key — the only places
/// model names live in providers.json (routing slots + provider
/// `default_model`), including nested `fallback` chains. Returns `true` if
/// anything changed.
pub fn migrate_value_in_place(root: &mut serde_json::Value) -> bool {
    use serde_json::Value;
    fn walk(v: &mut Value, changed: &mut bool) {
        match v {
            Value::Object(map) => {
                for (k, val) in map.iter_mut() {
                    if (k == "model" || k == "default_model") && val.is_string() {
                        if let Value::String(s) = val {
                            if smooth_alias::migrate_in_place(s) {
                                *changed = true;
                            }
                        }
                    } else {
                        walk(val, changed);
                    }
                }
            }
            Value::Array(arr) => {
                for e in arr.iter_mut() {
                    walk(e, changed);
                }
            }
            _ => {}
        }
    }
    let mut changed = false;
    walk(root, &mut changed);
    changed
}

/// Raw-JSON twin of [`retire_gateway_defaults`]: rewrites retired defaults in
/// every `routing.<slot>` (and nested `fallback`) whose `provider` is the Smoo
/// gateway, plus the gateway provider's own `default_model`. Returns `true` if
/// anything changed.
fn retire_gateway_defaults_value(root: &mut serde_json::Value) -> bool {
    use serde_json::Value;
    fn retire(model: &mut Value) -> bool {
        let Some(new) = model.as_str().and_then(smooth_alias::retired_gateway_default) else {
            return false;
        };
        *model = Value::String(new.to_string());
        true
    }
    let mut changed = false;
    let mut gateway = std::collections::HashSet::new();
    if let Some(providers) = root.get_mut("providers").and_then(Value::as_array_mut) {
        for p in providers {
            let id = p.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            let url = p.get("api_url").and_then(Value::as_str).unwrap_or_default();
            if is_gateway_provider(&id, url) {
                if let Some(m) = p.get_mut("default_model") {
                    changed |= retire(m);
                }
                gateway.insert(id);
            }
        }
    }
    if let Some(routing) = root.get_mut("routing").and_then(Value::as_object_mut) {
        for slot in routing.values_mut() {
            let mut cur = Some(slot);
            while let Some(s) = cur {
                let on_gateway = s.get("provider").and_then(Value::as_str).is_some_and(|p| gateway.contains(p));
                if on_gateway {
                    if let Some(m) = s.get_mut("model") {
                        changed |= retire(m);
                    }
                }
                cur = s.get_mut("fallback").filter(|f| f.is_object());
            }
        }
    }
    changed
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
        assert_eq!(r.routing.coding.model, "gpt-6-luna");
        assert_eq!(r.routing.reasoning.as_ref().unwrap().model, "gpt-6-luna-high");
        assert_eq!(r.routing.reviewing.model, "gpt-6-luna");
        assert_eq!(r.routing.judge.model, "groq-gpt-oss-120b");
        assert_eq!(r.routing.summarize.model, "gpt-6-luna");
        assert_eq!(r.routing.default.model, "gpt-6-luna");
        assert_eq!(r.routing.fast.as_ref().unwrap().model, "gpt-6-luna-fast");
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
        assert_eq!(r.routing.planning.as_ref().unwrap().model, "gpt-6-luna-high", "planning folded into reasoning");
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
        assert_eq!(r.routing.coding.model, "gpt-6-luna");
        assert_eq!(r.routing.coding.fallback.as_ref().unwrap().model, "gpt-6-luna-high");
    }

    #[test]
    fn load_with_migration_round_trips_to_disk() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("providers.json");
        legacy_registry().save_to_file(&path).expect("seed file");

        // Load via the wrapper: should rewrite + save back.
        let loaded = load_providers_with_migration(&path).expect("load");
        assert_eq!(loaded.routing.coding.model, "gpt-6-luna");
        assert_eq!(loaded.routing.fast.as_ref().unwrap().model, "gpt-6-luna-fast");

        // Read again with raw load_from_file — the file on disk must
        // now hold the concrete names too.
        let raw_reloaded = ProviderRegistry::load_from_file(&path).expect("reload");
        assert_eq!(raw_reloaded.routing.coding.model, "gpt-6-luna");
        assert_eq!(raw_reloaded.routing.reasoning.as_ref().unwrap().model, "gpt-6-luna-high");
        assert_eq!(raw_reloaded.routing.judge.model, "groq-gpt-oss-120b");
    }

    #[test]
    fn load_with_migration_preserves_per_provider_max_tokens() {
        // The whole reason the migration save goes through raw JSON: a
        // config that still holds a stale `smooth-*` alias AND a
        // per-provider `max_tokens` must keep the max_tokens after the
        // save-back. The typed `save_to_file` would silently drop it.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("providers.json");
        let raw = serde_json::json!({
            "providers": [
                { "id": "ollama", "api_url": "http://localhost:11434/v1", "api_key": "", "api_format": "OpenAiCompat", "default_model": "llama3.3", "max_tokens": 8192 }
            ],
            "routing": {
                "coding": { "provider": "ollama", "model": "smooth-coding" },
                "reasoning": { "provider": "ollama", "model": "llama3.3" },
                "reviewing": { "provider": "ollama", "model": "llama3.3" },
                "judge": { "provider": "ollama", "model": "llama3.3" },
                "summarize": { "provider": "ollama", "model": "llama3.3" },
                "fast": { "provider": "ollama", "model": "llama3.3" },
                "default": { "provider": "ollama", "model": "llama3.3" }
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        // Triggers a rewrite (smooth-coding → concrete) hence a save-back.
        let _ = load_providers_with_migration(&path).expect("load");

        let on_disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // Alias got rewritten...
        assert_eq!(on_disk["routing"]["coding"]["model"], "gpt-6-luna");
        // ...and max_tokens survived.
        assert_eq!(on_disk["providers"][0]["max_tokens"], 8192);
    }

    #[test]
    fn migrate_value_in_place_rewrites_nested_model_keys() {
        let mut root = serde_json::json!({
            "providers": [{ "id": "g", "default_model": "smooth-default", "max_tokens": 4096 }],
            "routing": { "coding": { "provider": "g", "model": "smooth-coding", "fallback": { "provider": "g", "model": "smooth-reasoning" } } }
        });
        assert!(migrate_value_in_place(&mut root));
        assert_eq!(root["providers"][0]["default_model"], "gpt-6-luna");
        assert_eq!(root["routing"]["coding"]["model"], "gpt-6-luna");
        assert_eq!(root["routing"]["coding"]["fallback"]["model"], "gpt-6-luna-high");
        // Non-model field untouched.
        assert_eq!(root["providers"][0]["max_tokens"], 4096);
        // Idempotent.
        assert!(!migrate_value_in_place(&mut root));
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
        assert_eq!(coding.new, "gpt-6-luna");
        let fast = rewrites.iter().find(|r| r.slot == "fast").expect("fast rewrite");
        assert_eq!(fast.old, "smooth-fast");
        assert_eq!(fast.new, "gpt-6-luna-fast");
    }

    /// SMOODEV-2097: a config that already ran the smooth-* migration is
    /// pinned to the *concrete* Groq Llama names. The gateway then
    /// removed those models, so the second migration step must bump them
    /// to gpt-oss — even though they carry no `smooth-` prefix.
    #[test]
    fn migrate_bumps_already_migrated_groq_llama_to_current_defaults() {
        let mut r = ProviderRegistry::new().with_routing(ModelRouting {
            coding: ModelSlot::new("smooai-gateway", "gpt-6-luna"),
            reasoning: Some(ModelSlot::new("smooai-gateway", "deepseek-v4-pro")),
            reviewing: ModelSlot::new("smooai-gateway", "minimax-m2.7-direct"),
            judge: ModelSlot::new("smooai-gateway", "groq-llama-3.3-70b"),
            summarize: ModelSlot::new("smooai-gateway", "gemini-2.5-flash"),
            default: ModelSlot::new("smooai-gateway", "gpt-6-luna"),
            fast: Some(ModelSlot::new("smooai-gateway", "groq-llama-3.1-8b")),
            planning: None,
        });
        let rewrites = migrate_provider_registry(&mut r);
        // Only judge + fast change; the rest were already live concrete
        // names.
        assert_eq!(rewrites.len(), 2, "rewrites = {rewrites:?}");
        assert_eq!(r.routing.judge.model, "groq-gpt-oss-120b");
        assert_eq!(r.routing.fast.as_ref().unwrap().model, "gpt-6-luna-fast");
        let judge = rewrites.iter().find(|r| r.slot == "judge").expect("judge rewrite");
        assert_eq!(judge.old, "groq-llama-3.3-70b");
        assert_eq!(judge.new, "groq-gpt-oss-120b");
        // Idempotent: a second pass makes no further changes.
        assert!(migrate_provider_registry(&mut r).is_empty());
    }

    /// SMOODEV-3342: the file a user actually has — `th model login` plus the
    /// smooth-* rewrite of its day pinned the concrete defaults of that day.
    /// Every one of them that is outside the model policy moves to the current
    /// slot default, on the gateway only; a slot on another provider is the
    /// user's own choice and stays.
    fn pinned_gateway_file() -> serde_json::Value {
        serde_json::json!({
            "providers": [
                { "id": "smooth", "api_url": "https://llm.smoo.ai/v1", "api_key": "k", "api_format": "OpenAiCompat", "default_model": "deepseek-v4-flash", "max_tokens": 4096 },
                { "id": "google", "api_url": "https://generativelanguage.googleapis.com/v1beta/openai", "api_key": "g", "api_format": "OpenAiCompat", "default_model": "gemini-2.5-flash" }
            ],
            "routing": {
                "coding": { "provider": "smooth", "model": "gpt-5.6-luna", "fallback": { "provider": "smooth", "model": "deepseek-v4-flash" } },
                "reasoning": { "provider": "smooth", "model": "deepseek-v4-pro" },
                "reviewing": { "provider": "smooth", "model": "minimax-m2.7-direct" },
                "judge": { "provider": "google", "model": "gemini-2.5-flash" },
                "summarize": { "provider": "smooth", "model": "gemini-2.5-flash" },
                "fast": { "provider": "smooth", "model": "gemini-3.5-flash" },
                "default": { "provider": "smooth", "model": "gpt-6-luna" }
            }
        })
    }

    #[test]
    fn retires_pinned_gateway_defaults_but_not_other_providers() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("providers.json");
        std::fs::write(&path, serde_json::to_string(&pinned_gateway_file()).unwrap()).unwrap();
        let mut r = ProviderRegistry::load_from_file(&path).expect("registry");
        assert!(migrate_provider_registry(&mut r).is_empty(), "no smooth-* alias in this file");
        let rewrites = retire_gateway_defaults(&mut r);
        assert_eq!(r.routing.coding.model, "gpt-6-luna");
        assert_eq!(r.routing.coding.fallback.as_ref().unwrap().model, "gpt-6-luna");
        assert_eq!(r.routing.reasoning.as_ref().unwrap().model, "gpt-6-luna-high");
        assert_eq!(r.routing.reviewing.model, "gpt-6-luna");
        assert_eq!(r.routing.summarize.model, "gpt-6-luna");
        assert_eq!(r.routing.fast.as_ref().unwrap().model, "gpt-6-luna-fast");
        // Routed to Google, not the gateway — the user's pick.
        assert_eq!(r.routing.judge.model, "gemini-2.5-flash");
        let fast = rewrites.iter().find(|w| w.slot == "fast").expect("fast rewrite");
        assert_eq!((fast.old.as_str(), fast.new.as_str()), ("gemini-3.5-flash", "gpt-6-luna-fast"));
        assert!(retire_gateway_defaults(&mut r).is_empty(), "idempotent");
    }

    #[test]
    fn load_with_migration_retires_pinned_defaults_on_disk() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("providers.json");
        std::fs::write(&path, serde_json::to_string_pretty(&pinned_gateway_file()).unwrap()).unwrap();

        let _ = load_providers_with_migration(&path).expect("load");

        let on_disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk["routing"]["coding"]["model"], "gpt-6-luna");
        assert_eq!(on_disk["routing"]["coding"]["fallback"]["model"], "gpt-6-luna");
        assert_eq!(on_disk["routing"]["reasoning"]["model"], "gpt-6-luna-high");
        assert_eq!(on_disk["routing"]["fast"]["model"], "gpt-6-luna-fast");
        assert_eq!(on_disk["routing"]["summarize"]["model"], "gpt-6-luna");
        assert_eq!(on_disk["providers"][0]["default_model"], "gpt-6-luna");
        assert_eq!(on_disk["providers"][0]["max_tokens"], 4096, "unknown fields survive");
        // The Google provider and the slot routed to it are untouched.
        assert_eq!(on_disk["routing"]["judge"]["model"], "gemini-2.5-flash");
        assert_eq!(on_disk["providers"][1]["default_model"], "gemini-2.5-flash");
    }

    #[test]
    fn retirement_runs_once_so_a_later_pick_sticks() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("providers.json");
        std::fs::write(&path, serde_json::to_string_pretty(&pinned_gateway_file()).unwrap()).unwrap();
        let _ = load_providers_with_migration(&path).expect("first load retires");
        assert!(retirement_marker(&path).exists(), "the marker records the one-time pass");

        // The user deliberately picks a retired name again (th code picker).
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["routing"]["reasoning"]["model"] = "deepseek-v4-pro".into();
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();

        let reloaded = load_providers_with_migration(&path).expect("second load");
        assert_eq!(
            reloaded.routing.reasoning.as_ref().unwrap().model,
            "deepseek-v4-pro",
            "a pick made after the migration sticks"
        );
        let on_disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk["routing"]["reasoning"]["model"], "deepseek-v4-pro");
    }
}
