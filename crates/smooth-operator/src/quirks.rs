//! Per-model wire-format quirks.
//!
//! When we route through a LiteLLM-style gateway the concrete
//! upstream model only reveals itself in response headers
//! (`x-litellm-model-name`) — but by then we've already sent the
//! request. So quirks here are split into two surfaces:
//!
//! * **Always-safe defaults** — things that work across every model
//!   we've tested. For example, `canonical_tool_arguments_json` in
//!   `llm.rs` always emits a JSON-object string; that's the strictest
//!   shape and every provider accepts it. Prefer always-safe over
//!   per-model conditional logic when the strict form works.
//! * **Model-specific tweaks** — keyed off the concrete upstream
//!   name. Used for things where the strict form doesn't work
//!   everywhere (e.g. a model that rejects `parallel_tool_calls: true`
//!   that most others accept). Today this registry is empty; the
//!   module exists so future quirks have an obvious home.
//!
//! How to identify the upstream concrete model at request time:
//! 1. Configured model name in `LlmConfig.model` (e.g. `smooth-coding`).
//! 2. The last-seen `x-litellm-model-name` header from a prior
//!    response on the same client (future work: cache on the
//!    `LlmClient`).
//!
//! See the Aider / OpenCode equivalents for reference.

use std::collections::HashMap;

/// Per-model flags. Populate fields only when the quirk is worth the
/// branch — every conditional is a place for drift.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelQuirks {
    /// Some providers reject `parallel_tool_calls: true`. When this
    /// is `Some(false)` the caller should force the field off even
    /// if the agent config requests parallel tools.
    pub allow_parallel_tools: Option<bool>,
    /// A few providers (DashScope/qwen3, notably) give obscure
    /// errors on borderline-malformed tool_call echoes. Flagging a
    /// model here asks the client to be extra careful about the
    /// wire shape — today nothing reads this, but it's the anchor
    /// for future defensive tweaks.
    pub strict_tool_call_json: bool,
}

/// Look up per-model quirks by concrete upstream name.
///
/// Matching is substring-based so minor version drift (`qwen3-coder-plus-2025-04`)
/// still hits the entry for `qwen3-coder-plus`.
///
/// Returns an empty `ModelQuirks` when nothing matches — callers
/// pattern `quirks.foo.is_some_and(|v| !v)` so defaults stay safe.
pub fn for_model(upstream: &str) -> ModelQuirks {
    let lc = upstream.to_lowercase();
    for (needle, quirks) in table() {
        if lc.contains(needle) {
            return quirks;
        }
    }
    ModelQuirks::default()
}

fn table() -> Vec<(&'static str, ModelQuirks)> {
    vec![
        (
            "qwen3-coder",
            ModelQuirks {
                strict_tool_call_json: true,
                ..ModelQuirks::default()
            },
        ),
        (
            "qwen-coder",
            ModelQuirks {
                strict_tool_call_json: true,
                ..ModelQuirks::default()
            },
        ),
    ]
}

/// Index the table by its canonical keys. Exposed for tests and
/// `th routing quirks`-style diagnostics.
pub fn all_keys() -> Vec<String> {
    table().into_iter().map(|(k, _)| k.to_string()).collect()
}

/// Aggregate all quirk matches for an upstream name. Usually just
/// one entry wins; kept as a `HashMap` so tests can assert coverage.
pub fn debug_snapshot(upstream: &str) -> HashMap<String, ModelQuirks> {
    let mut out = HashMap::new();
    let lc = upstream.to_lowercase();
    for (needle, quirks) in table() {
        if lc.contains(needle) {
            out.insert(needle.to_string(), quirks);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_returns_default() {
        assert_eq!(for_model("gpt-4o"), ModelQuirks::default());
        assert_eq!(for_model("anthropic/claude-sonnet-4-6"), ModelQuirks::default());
    }

    #[test]
    fn qwen3_coder_matches_substring() {
        let q = for_model("qwen3-coder-plus");
        assert!(q.strict_tool_call_json);

        let q = for_model("dashscope/qwen3-coder-plus-2025");
        assert!(q.strict_tool_call_json);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let q = for_model("QWEN3-CODER-PLUS");
        assert!(q.strict_tool_call_json);
    }

    #[test]
    fn all_keys_are_non_empty_lowercase() {
        let keys = all_keys();
        assert!(!keys.is_empty(), "quirks table should not be empty");
        for k in &keys {
            assert_eq!(k, &k.to_lowercase(), "key should be lowercase: {k}");
            assert!(!k.is_empty());
        }
    }

    #[test]
    fn debug_snapshot_includes_all_matches() {
        let snap = debug_snapshot("qwen3-coder-plus");
        assert!(snap.contains_key("qwen3-coder"));
    }
}
