//! Field-preserving `providers.json` editing.
//!
//! The published `ProviderRegistry` (`smooai-smooth-operator-core`) has a
//! fixed [`ProviderConfig`](smooth_operator::providers::ProviderConfig)
//! shape and its serializer **drops any field it doesn't know** — there is
//! no `#[serde(flatten)]` catch-all and no `deny_unknown_fields`. So a
//! typed load → save round-trip silently erases anything the struct lacks,
//! including the per-provider `max_tokens` this feature adds.
//!
//! Every write that must keep those extra fields therefore goes through
//! `serde_json::Value` here instead of the typed registry:
//!
//! - `th providers add/remove/list` edit the providers array in place.
//! - The runner reads a provider's `max_tokens` via [`max_tokens_for_api_url`].
//! - Routing-slot changes (model picker, alias migration) use
//!   [`set_routing_slot`] so persisting a routing change doesn't clobber a
//!   sibling provider's `max_tokens`.
//!
//! All of these read the file to a `Value`, mutate only the keys they own,
//! and write it back — unknown keys ride along untouched.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

/// A provider to add or update via [`upsert_provider`]. Only `id` and
/// `api_url` are required; the rest fall back to sane defaults when `None`.
#[derive(Debug, Clone)]
pub struct NewProvider {
    pub id: String,
    pub api_url: String,
    /// `None` → empty string (local servers like Ollama need no key).
    pub api_key: Option<String>,
    /// `None` → `"OpenAiCompat"`. Accepts loose input; see [`normalize_format`].
    pub api_format: Option<String>,
    /// `None` → empty string. Set from a live `/v1/models` probe when adding
    /// a detected local server.
    pub default_model: Option<String>,
    /// Optional per-provider token cap. Plumbed to the runner so small
    /// local-model context windows aren't blown by the default 32768.
    pub max_tokens: Option<u32>,
}

/// A one-line summary of a configured provider, for `th providers list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSummary {
    pub id: String,
    pub api_url: String,
    pub default_model: String,
    pub max_tokens: Option<u32>,
    pub local: bool,
}

/// True for URLs that point at a local inference server (Ollama, LM
/// Studio, llama.cpp, …). Used to decide which providers to query live in
/// the picker / `th cast models`, and to tag `th providers list` output.
#[must_use]
pub fn is_local_url(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.contains("localhost") || u.contains("127.0.0.1") || u.contains("0.0.0.0") || u.contains("[::1]")
}

/// Normalize loose `--format` input to the exact `ApiFormat` string the
/// typed loader deserializes (`"OpenAiCompat"` or `"Anthropic"`). Anything
/// unrecognized falls back to `"OpenAiCompat"` — the shape every local
/// server (Ollama, LM Studio) speaks.
#[must_use]
pub fn normalize_format(raw: Option<&str>) -> String {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("anthropic") => "Anthropic".to_string(),
        _ => "OpenAiCompat".to_string(),
    }
}

/// Load providers.json as a raw `Value`. A missing file yields an empty
/// skeleton (`{"providers": []}`) so the upsert path can create it.
///
/// # Errors
/// Propagates read / JSON-parse errors for an existing but malformed file.
pub fn load_value(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({ "providers": [] }));
    }
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
}

/// Write `root` back to `path` as pretty JSON, creating the parent dir if
/// needed.
///
/// # Errors
/// Propagates directory-create / serialize / write errors.
pub fn save_value(path: &Path, root: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let s = serde_json::to_string_pretty(root).context("serializing providers.json")?;
    std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))
}

fn providers_array_mut(root: &mut Value) -> &mut Vec<Value> {
    if !root.is_object() {
        *root = json!({});
    }
    let obj = root.as_object_mut().expect("root is object");
    obj.entry("providers").or_insert_with(|| Value::Array(Vec::new()));
    // Coerce a non-array `providers` (corrupt file) into an empty array.
    if !obj["providers"].is_array() {
        obj["providers"] = Value::Array(Vec::new());
    }
    obj["providers"].as_array_mut().expect("providers is array")
}

fn provider_to_value(p: &NewProvider) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(p.id));
    obj.insert("api_url".into(), json!(p.api_url));
    obj.insert("api_key".into(), json!(p.api_key.clone().unwrap_or_default()));
    obj.insert("api_format".into(), json!(normalize_format(p.api_format.as_deref())));
    obj.insert("default_model".into(), json!(p.default_model.clone().unwrap_or_default()));
    // Only emit max_tokens when set, so an unset field never overwrites /
    // clears an existing one on update.
    if let Some(mt) = p.max_tokens {
        obj.insert("max_tokens".into(), json!(mt));
    }
    Value::Object(obj)
}

/// Insert a new provider, or merge into the existing entry with the same
/// `id`, preserving every other field in the file (and any unknown keys on
/// the existing entry). Returns `true` when an existing entry was updated.
///
/// When adding the *first* provider to a file that has no `routing` block,
/// wires all routing slots to the new provider so the typed loader accepts
/// the result.
pub fn upsert_provider(root: &mut Value, p: &NewProvider) -> bool {
    let is_first = providers_array_mut(root).is_empty();
    let arr = providers_array_mut(root);

    let updated = if let Some(slot) = arr.iter_mut().find(|e| e.get("id").and_then(Value::as_str) == Some(p.id.as_str())) {
        // Merge: overwrite ONLY the fields explicitly provided, leaving
        // every other key intact (a prior `default_model`, unknown keys,
        // etc.). A `None` optional must not clobber an existing value —
        // `provider_to_value` fills `None` with empty-string defaults,
        // which is right for a brand-new entry but would wipe an
        // existing one on re-`add`, so the merge path is sparse.
        if let Some(dst) = slot.as_object_mut() {
            dst.insert("api_url".into(), json!(p.api_url));
            if let Some(ref k) = p.api_key {
                dst.insert("api_key".into(), json!(k));
            }
            if p.api_format.is_some() {
                dst.insert("api_format".into(), json!(normalize_format(p.api_format.as_deref())));
            }
            if let Some(ref m) = p.default_model {
                dst.insert("default_model".into(), json!(m));
            }
            if let Some(mt) = p.max_tokens {
                dst.insert("max_tokens".into(), json!(mt));
            }
        }
        true
    } else {
        arr.push(provider_to_value(p));
        false
    };

    if is_first && root.get("routing").is_none() {
        let model = p.default_model.clone().unwrap_or_default();
        wire_routing_to(root, &p.id, &model);
    }
    updated
}

/// Remove the provider with the given `id`. Returns `true` if one was
/// removed. Leaves routing untouched (a dangling routing slot still loads
/// via the typed loader's fallback).
pub fn remove_provider(root: &mut Value, id: &str) -> bool {
    let arr = providers_array_mut(root);
    let before = arr.len();
    arr.retain(|e| e.get("id").and_then(Value::as_str) != Some(id));
    before != arr.len()
}

/// Summarize every configured provider for `th providers list`.
#[must_use]
pub fn list_providers(root: &Value) -> Vec<ProviderSummary> {
    root.get("providers")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|e| {
                    let api_url = e.get("api_url").and_then(Value::as_str).unwrap_or_default().to_string();
                    ProviderSummary {
                        id: e.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                        local: is_local_url(&api_url),
                        default_model: e.get("default_model").and_then(Value::as_str).unwrap_or_default().to_string(),
                        max_tokens: e.get("max_tokens").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()),
                        api_url,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `max_tokens` for the provider entry whose `api_url` matches
/// `api_url`, if set. Used by Big Smooth to plumb `SMOOTH_MAX_TOKENS` to the
/// runner so small local-model context windows aren't blown by the default.
#[must_use]
pub fn max_tokens_for_api_url(root: &Value, api_url: &str) -> Option<u32> {
    root.get("providers")
        .and_then(Value::as_array)?
        .iter()
        .find(|e| e.get("api_url").and_then(Value::as_str) == Some(api_url))
        .and_then(|e| e.get("max_tokens"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

/// [`max_tokens_for_api_url`] straight off disk. Returns `None` on any
/// read / parse error (caller treats absence as "use the default cap").
#[must_use]
pub fn max_tokens_for_api_url_from_file(path: &Path, api_url: &str) -> Option<u32> {
    load_value(path).ok().and_then(|root| max_tokens_for_api_url(&root, api_url))
}

/// Set a routing slot's `provider` + `model` in place, preserving every
/// other field in the file (sibling providers' `max_tokens`, unknown keys).
///
/// `slot_key` is the on-disk routing key: `coding`, `reasoning`,
/// `reviewing`, `judge`, `summarize`, `fast`, or `default`. Any existing
/// per-slot fields (e.g. `fallback`) on that slot are preserved; only
/// `provider` and `model` are overwritten.
pub fn set_routing_slot(root: &mut Value, slot_key: &str, provider: &str, model: &str) {
    if !root.is_object() {
        *root = json!({});
    }
    let obj = root.as_object_mut().expect("root object");
    let routing = obj.entry("routing").or_insert_with(|| json!({}));
    if !routing.is_object() {
        *routing = json!({});
    }
    let routing = routing.as_object_mut().expect("routing object");
    let slot = routing.entry(slot_key.to_string()).or_insert_with(|| json!({}));
    if !slot.is_object() {
        *slot = json!({});
    }
    let slot = slot.as_object_mut().expect("slot object");
    slot.insert("provider".into(), json!(provider));
    slot.insert("model".into(), json!(model));
}

/// Point every routing slot at `provider`/`model`. Used to bootstrap a
/// fresh file's routing when the first provider is added, and by onboarding
/// (`th up`'s picker, the daemon's `configure_provider` tool) when the user
/// explicitly switches Big Smooth to a provider.
pub fn wire_routing_to(root: &mut Value, provider: &str, model: &str) {
    for slot in ["coding", "reasoning", "reviewing", "judge", "summarize", "fast", "default"] {
        set_routing_slot(root, slot, provider, model);
    }
}

fn provider_by_id<'a>(providers: &'a [Value], id: Option<&str>) -> Option<&'a Value> {
    let id = id?;
    providers.iter().find(|p| p.get("id").and_then(Value::as_str) == Some(id))
}

/// Resolve a routing slot of a parsed providers.json to `(api_url, api_key,
/// model)` — the rule Big Smooth uses to pick its gateway at boot.
///
/// The provider is found BY the slot's own `provider` id (every writer stamps a
/// different id), falling back to the `coding` slot's provider, then the sole
/// provider. A provider with an empty key resolves `None` (the daemon cannot
/// run a turn without one). The model falls back from the slot to `coding`, to
/// the provider default, to the gateway's default slot model.
#[must_use]
pub fn route_from_value(v: &Value, route: &str) -> Option<(String, String, String)> {
    let providers = v.get("providers")?.as_array()?;
    let slot_provider = |r: &str| v.pointer(&format!("/routing/{r}/provider")).and_then(Value::as_str).map(str::to_owned);
    let route_id = slot_provider(route);
    let coding_id = slot_provider("coding");
    let provider = provider_by_id(providers, route_id.as_deref())
        .or_else(|| provider_by_id(providers, coding_id.as_deref()))
        .or_else(|| if providers.len() == 1 { providers.first() } else { None })?;
    let url = provider.get("api_url")?.as_str()?.to_owned();
    let key = provider.get("api_key")?.as_str().filter(|k| !k.trim().is_empty())?.to_owned();
    let model = v
        .pointer(&format!("/routing/{route}/model"))
        .and_then(Value::as_str)
        .or_else(|| v.pointer("/routing/coding/model").and_then(Value::as_str))
        .or_else(|| provider.get("default_model").and_then(Value::as_str))
        .unwrap_or(smooth_policy::smooth_alias::SmoothSlot::Default.concrete_default())
        .to_owned();
    Some((url, key, model))
}

/// [`route_from_value`] against a file. `None` when it is missing or unparseable.
#[must_use]
pub fn route_at(path: &Path, route: &str) -> Option<(String, String, String)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    route_from_value(&v, route)
}

/// The configured provider a direct-route model
/// ([`smooth_policy::direct_models`]) runs on: the provider whose id is the
/// model's `provider_id`, else the first whose `api_format` matches — in both
/// cases only one with a url and a non-empty key. `None` means the model cannot
/// run here and is not offered.
#[must_use]
pub fn direct_provider<'a>(v: &'a Value, model: &smooth_policy::direct_models::DirectModel) -> Option<&'a Value> {
    let providers = v.get("providers")?.as_array()?;
    let usable =
        |p: &&Value| p.get("api_url").and_then(Value::as_str).is_some() && p.get("api_key").and_then(Value::as_str).is_some_and(|k| !k.trim().is_empty());
    providers
        .iter()
        .filter(usable)
        .find(|p| p.get("id").and_then(Value::as_str) == Some(model.provider_id))
        .or_else(|| {
            providers
                .iter()
                .filter(usable)
                .find(|p| normalize_format(p.get("api_format").and_then(Value::as_str)) == model.api_format)
        })
}

/// [`direct_provider`]'s `(api_url, api_key)`.
#[must_use]
pub fn direct_provider_from_value(v: &Value, model: &smooth_policy::direct_models::DirectModel) -> Option<(String, String)> {
    let p = direct_provider(v, model)?;
    Some((p.get("api_url")?.as_str()?.to_owned(), p.get("api_key")?.as_str()?.to_owned()))
}

/// [`route_from_value`] for the daemon's upstream GATEWAY. When the slot is
/// pinned to a direct-route model on its own provider (the `th code` picker
/// writes `{provider: "anthropic", model: "claude-haiku-5-5"}`), the model is
/// kept but the url/key come from the gateway provider — a non-Anthropic
/// provider, preferring the ids `smooth` and `smooai-gateway` — because every
/// other model the user switches to still runs there; the daemon's model router
/// sends the direct model to its provider. With no such gateway, this is
/// exactly [`route_from_value`].
#[must_use]
pub fn upstream_route_from_value(v: &Value, route: &str) -> Option<(String, String, String)> {
    let (url, key, model) = route_from_value(v, route)?;
    if smooth_policy::direct_models::direct_model(&model).is_none() {
        return Some((url, key, model));
    }
    let providers = v.get("providers").and_then(Value::as_array);
    let gateway = providers.and_then(|ps| {
        let usable = |p: &&Value| {
            normalize_format(p.get("api_format").and_then(Value::as_str)) != "Anthropic"
                && p.get("api_key").and_then(Value::as_str).is_some_and(|k| !k.trim().is_empty())
                && p.get("api_url").and_then(Value::as_str).is_some()
        };
        ["smooth", "smooai-gateway"]
            .iter()
            .find_map(|id| ps.iter().filter(usable).find(|p| p.get("id").and_then(Value::as_str) == Some(id)))
            .or_else(|| ps.iter().find(usable))
    });
    match gateway {
        Some(g) => Some((
            g.get("api_url").and_then(Value::as_str)?.to_owned(),
            g.get("api_key").and_then(Value::as_str)?.to_owned(),
            model,
        )),
        None => Some((url, key, model)),
    }
}

/// [`upstream_route_from_value`] against a file. `None` when it is missing or
/// unparseable.
#[must_use]
pub fn upstream_route_at(path: &Path, route: &str) -> Option<(String, String, String)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    upstream_route_from_value(&v, route)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_anthropic() -> Value {
        json!({
            "providers": [
                { "id": "smooth", "api_url": "https://llm.smoo.ai/v1", "api_key": "sk-gw", "api_format": "OpenAiCompat" },
                { "id": "anthropic", "api_url": "https://api.anthropic.com/v1", "api_key": "sk-ant", "api_format": "Anthropic" }
            ],
            "routing": { "coding": { "provider": "smooth", "model": "gpt-6-luna" } }
        })
    }

    #[test]
    fn direct_provider_prefers_the_named_id() {
        let haiku = smooth_policy::direct_models::direct_model("claude-haiku-5-5").unwrap();
        assert_eq!(
            direct_provider_from_value(&with_anthropic(), haiku),
            Some(("https://api.anthropic.com/v1".into(), "sk-ant".into()))
        );
    }

    #[test]
    fn direct_provider_falls_back_to_the_api_format() {
        let haiku = smooth_policy::direct_models::direct_model("claude-haiku-5-5").unwrap();
        let v = json!({ "providers": [
            { "id": "smooth", "api_url": "https://llm.smoo.ai/v1", "api_key": "sk-gw" },
            { "id": "my-claude", "api_url": "https://api.anthropic.com/v1", "api_key": "sk-mine", "api_format": "anthropic" }
        ]});
        assert_eq!(
            direct_provider_from_value(&v, haiku),
            Some(("https://api.anthropic.com/v1".into(), "sk-mine".into()))
        );
    }

    #[test]
    fn direct_provider_needs_a_key() {
        let haiku = smooth_policy::direct_models::direct_model("claude-haiku-5-5").unwrap();
        let v = json!({ "providers": [
            { "id": "anthropic", "api_url": "https://api.anthropic.com/v1", "api_key": "  ", "api_format": "Anthropic" }
        ]});
        assert_eq!(direct_provider_from_value(&v, haiku), None);
        assert_eq!(direct_provider_from_value(&sample(), haiku), None, "no Anthropic provider at all");
    }

    #[test]
    fn upstream_route_is_route_from_value_for_gateway_models() {
        let v = with_anthropic();
        assert_eq!(upstream_route_from_value(&v, "coding"), route_from_value(&v, "coding"));
    }

    #[test]
    fn upstream_route_keeps_the_gateway_for_a_direct_slot() {
        let mut v = with_anthropic();
        set_routing_slot(&mut v, "coding", "anthropic", "claude-haiku-5-5");
        assert_eq!(
            upstream_route_from_value(&v, "coding"),
            Some(("https://llm.smoo.ai/v1".into(), "sk-gw".into(), "claude-haiku-5-5".into()))
        );
    }

    #[test]
    fn upstream_route_without_a_gateway_uses_the_slot_provider() {
        let v = json!({
            "providers": [{ "id": "anthropic", "api_url": "https://api.anthropic.com/v1", "api_key": "sk-ant", "api_format": "Anthropic" }],
            "routing": { "coding": { "provider": "anthropic", "model": "claude-haiku-5-5" } }
        });
        assert_eq!(
            upstream_route_from_value(&v, "coding"),
            Some(("https://api.anthropic.com/v1".into(), "sk-ant".into(), "claude-haiku-5-5".into()))
        );
    }

    fn sample() -> Value {
        json!({
            "providers": [
                { "id": "smooth", "api_url": "https://llm.smoo.ai/v1", "api_key": "k", "api_format": "OpenAiCompat", "default_model": "deepseek-v4-flash", "custom_field": 7 }
            ],
            "routing": { "coding": { "provider": "smooth", "model": "deepseek-v4-flash" } },
            "top_level_unknown": "keep me"
        })
    }

    #[test]
    fn is_local_url_detects_local_hosts() {
        assert!(is_local_url("http://localhost:11434/v1"));
        assert!(is_local_url("http://127.0.0.1:1234/v1"));
        assert!(is_local_url("http://0.0.0.0:8080/v1"));
        assert!(!is_local_url("https://llm.smoo.ai/v1"));
        assert!(!is_local_url("https://api.openai.com/v1"));
    }

    #[test]
    fn normalize_format_maps_loose_input() {
        assert_eq!(normalize_format(Some("anthropic")), "Anthropic");
        assert_eq!(normalize_format(Some("Anthropic")), "Anthropic");
        assert_eq!(normalize_format(Some("openai")), "OpenAiCompat");
        assert_eq!(normalize_format(Some("garbage")), "OpenAiCompat");
        assert_eq!(normalize_format(None), "OpenAiCompat");
    }

    #[test]
    fn upsert_adds_and_preserves_unknown_fields() {
        let mut root = sample();
        let updated = upsert_provider(
            &mut root,
            &NewProvider {
                id: "ollama".into(),
                api_url: "http://localhost:11434/v1".into(),
                api_key: None,
                api_format: None,
                default_model: Some("llama3.3".into()),
                max_tokens: Some(8192),
            },
        );
        assert!(!updated, "ollama is a new entry");
        // Existing provider + its unknown field survive untouched.
        let arr = root["providers"].as_array().unwrap();
        let smooth = arr.iter().find(|e| e["id"] == "smooth").unwrap();
        assert_eq!(smooth["custom_field"], 7);
        // Top-level unknown key survives.
        assert_eq!(root["top_level_unknown"], "keep me");
        // New entry has the right shape.
        let ollama = arr.iter().find(|e| e["id"] == "ollama").unwrap();
        assert_eq!(ollama["api_key"], "");
        assert_eq!(ollama["api_format"], "OpenAiCompat");
        assert_eq!(ollama["default_model"], "llama3.3");
        assert_eq!(ollama["max_tokens"], 8192);
    }

    #[test]
    fn upsert_updates_existing_preserving_its_extra_keys() {
        let mut root = sample();
        let updated = upsert_provider(
            &mut root,
            &NewProvider {
                id: "smooth".into(),
                api_url: "https://llm.smoo.ai/v1".into(),
                api_key: Some("newkey".into()),
                api_format: None,
                default_model: None,
                max_tokens: Some(64000),
            },
        );
        assert!(updated, "smooth already exists");
        let smooth = root["providers"].as_array().unwrap().iter().find(|e| e["id"] == "smooth").unwrap();
        assert_eq!(smooth["api_key"], "newkey");
        assert_eq!(smooth["max_tokens"], 64000);
        // Merge, not replace: the pre-existing unknown field is retained.
        assert_eq!(smooth["custom_field"], 7);
        // default_model was None → left as the existing value.
        assert_eq!(smooth["default_model"], "deepseek-v4-flash");
    }

    #[test]
    fn upsert_without_max_tokens_does_not_write_the_key() {
        let mut root = json!({ "providers": [] });
        upsert_provider(
            &mut root,
            &NewProvider {
                id: "x".into(),
                api_url: "https://x/v1".into(),
                api_key: Some("k".into()),
                api_format: None,
                default_model: Some("m".into()),
                max_tokens: None,
            },
        );
        let x = &root["providers"][0];
        assert!(x.get("max_tokens").is_none(), "unset max_tokens must not appear");
    }

    #[test]
    fn upsert_first_provider_wires_routing() {
        let mut root = json!({ "providers": [] });
        upsert_provider(
            &mut root,
            &NewProvider {
                id: "ollama".into(),
                api_url: "http://localhost:11434/v1".into(),
                api_key: None,
                api_format: None,
                default_model: Some("llama3.3".into()),
                max_tokens: None,
            },
        );
        assert_eq!(root["routing"]["coding"]["provider"], "ollama");
        assert_eq!(root["routing"]["fast"]["model"], "llama3.3");
        assert_eq!(root["routing"]["default"]["provider"], "ollama");
    }

    #[test]
    fn remove_provider_drops_only_the_named_entry() {
        let mut root = sample();
        assert!(remove_provider(&mut root, "smooth"));
        assert!(root["providers"].as_array().unwrap().is_empty());
        assert!(!remove_provider(&mut root, "smooth"), "already gone");
    }

    #[test]
    fn list_providers_summarizes_with_local_tag() {
        let mut root = sample();
        upsert_provider(
            &mut root,
            &NewProvider {
                id: "ollama".into(),
                api_url: "http://localhost:11434/v1".into(),
                api_key: None,
                api_format: None,
                default_model: Some("llama3.3".into()),
                max_tokens: Some(8192),
            },
        );
        let summaries = list_providers(&root);
        let ollama = summaries.iter().find(|s| s.id == "ollama").unwrap();
        assert!(ollama.local);
        assert_eq!(ollama.max_tokens, Some(8192));
        let smooth = summaries.iter().find(|s| s.id == "smooth").unwrap();
        assert!(!smooth.local);
        assert_eq!(smooth.max_tokens, None);
    }

    #[test]
    fn max_tokens_lookup_by_api_url() {
        let mut root = sample();
        upsert_provider(
            &mut root,
            &NewProvider {
                id: "ollama".into(),
                api_url: "http://localhost:11434/v1".into(),
                api_key: None,
                api_format: None,
                default_model: Some("llama3.3".into()),
                max_tokens: Some(8192),
            },
        );
        assert_eq!(max_tokens_for_api_url(&root, "http://localhost:11434/v1"), Some(8192));
        // No max_tokens on the smooth entry.
        assert_eq!(max_tokens_for_api_url(&root, "https://llm.smoo.ai/v1"), None);
        // Unknown url.
        assert_eq!(max_tokens_for_api_url(&root, "https://nope/v1"), None);
    }

    #[test]
    fn set_routing_slot_preserves_sibling_max_tokens() {
        // Regression for the exact failure this feature guards against:
        // routing a local model into a slot must NOT wipe a provider's
        // max_tokens (which the typed save_to_file would).
        let mut root = json!({
            "providers": [
                { "id": "ollama", "api_url": "http://localhost:11434/v1", "api_key": "", "api_format": "OpenAiCompat", "default_model": "llama3.3", "max_tokens": 8192 }
            ],
            "routing": { "fast": { "provider": "smooth", "model": "groq-gpt-oss-20b" } }
        });
        set_routing_slot(&mut root, "fast", "ollama", "llama3.3");
        assert_eq!(root["routing"]["fast"]["provider"], "ollama");
        assert_eq!(root["routing"]["fast"]["model"], "llama3.3");
        // The provider entry's max_tokens is untouched.
        assert_eq!(root["providers"][0]["max_tokens"], 8192);
    }

    #[test]
    fn set_routing_slot_keeps_existing_slot_fields() {
        let mut root = json!({
            "routing": { "coding": { "provider": "smooth", "model": "deepseek-v4-flash", "fallback": { "provider": "smooth", "model": "deepseek-v4-pro" } } }
        });
        set_routing_slot(&mut root, "coding", "ollama", "qwen");
        assert_eq!(root["routing"]["coding"]["provider"], "ollama");
        assert_eq!(root["routing"]["coding"]["model"], "qwen");
        // The fallback sub-object is preserved.
        assert_eq!(root["routing"]["coding"]["fallback"]["model"], "deepseek-v4-pro");
    }

    #[test]
    fn load_and_save_round_trip_preserves_everything() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("providers.json");
        save_value(&path, &sample()).unwrap();
        let mut root = load_value(&path).unwrap();
        upsert_provider(
            &mut root,
            &NewProvider {
                id: "ollama".into(),
                api_url: "http://localhost:11434/v1".into(),
                api_key: None,
                api_format: None,
                default_model: Some("llama3.3".into()),
                max_tokens: Some(4096),
            },
        );
        save_value(&path, &root).unwrap();
        let reloaded = load_value(&path).unwrap();
        assert_eq!(reloaded["top_level_unknown"], "keep me");
        assert_eq!(reloaded["providers"].as_array().unwrap().len(), 2);
        assert_eq!(max_tokens_for_api_url(&reloaded, "http://localhost:11434/v1"), Some(4096));
    }

    #[test]
    fn load_value_missing_file_is_empty_skeleton() {
        let root = load_value(Path::new("/nonexistent/does/not/exist.json")).unwrap();
        assert!(root["providers"].as_array().unwrap().is_empty());
    }
}
