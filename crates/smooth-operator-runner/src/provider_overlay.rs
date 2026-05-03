//! Per-provider system-prompt overlays.
//!
//! Each major model family that Smoo routes to has known failure modes —
//! GPT-class trends to early-yield, Gemini-class trends to text-imitation
//! tool calls, Kimi-class wants action over narration, etc. The base
//! `system.md` carries the shared discipline; this module prepends a
//! short overlay tuned to whichever family the dispatched runner is on.
//!
//! Lifted from opencode's per-provider prompt directory (anthropic.txt,
//! beast.txt, gemini.txt, kimi.txt, ...) but trimmed and re-tuned for the
//! Smoo cast vocabulary.

const ANTHROPIC: &str = include_str!("../prompts/providers/anthropic.md");
const GPT: &str = include_str!("../prompts/providers/gpt.md");
const GEMINI: &str = include_str!("../prompts/providers/gemini.md");
const KIMI: &str = include_str!("../prompts/providers/kimi.md");
const DEEPSEEK: &str = include_str!("../prompts/providers/deepseek.md");
const GLM: &str = include_str!("../prompts/providers/glm.md");
const QWEN: &str = include_str!("../prompts/providers/qwen.md");

/// Return the provider overlay text for a model identifier, or `None` if
/// the model doesn't match any known family.
///
/// The match is substring-based and case-insensitive. Smoo semantic
/// aliases (`smooth-coding`, `smooth-reasoning`, `smooth-fast-gemini`,
/// `smooth-judge`, etc.) resolve here too — they map to the family that
/// backs them in the gateway today. If the gateway swaps a backing
/// provider, update the alias-mapping arm rather than the family arms.
pub fn for_model(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();

    // Smoo semantic aliases first — they're more specific than family
    // names and let us pin a particular overlay regardless of which
    // upstream the gateway happens to route to.
    if m.starts_with("smooth-fast-gemini") {
        return Some(GEMINI);
    }
    if m.starts_with("smooth-coding") {
        return Some(KIMI);
    }
    if m.starts_with("smooth-reasoning") {
        return Some(DEEPSEEK);
    }
    if m.starts_with("smooth-judge") || m.starts_with("smooth-reviewing") {
        return Some(ANTHROPIC);
    }
    if m.starts_with("smooth-fast") || m.starts_with("smooth-summarize") {
        return Some(GPT);
    }

    // Family substring fallthroughs — for explicit non-aliased model
    // strings like `claude-haiku-4-5` or `gpt-5.4-mini`. Order matters:
    // longer / more-specific names first.
    if m.contains("claude") || m.contains("anthropic") {
        return Some(ANTHROPIC);
    }
    if m.contains("kimi") || m.contains("minimax") || m.contains("moonshot") {
        return Some(KIMI);
    }
    if m.contains("deepseek") {
        return Some(DEEPSEEK);
    }
    if m.contains("gemini") {
        return Some(GEMINI);
    }
    if m.contains("glm") || m.contains("zai") || m.contains("z.ai") {
        return Some(GLM);
    }
    if m.contains("qwen") {
        return Some(QWEN);
    }
    if m.contains("gpt") || m.contains("o1") || m.contains("o3") || m.contains("codex") {
        return Some(GPT);
    }

    None
}

/// True when the model identifier indicates a Claude / Anthropic backing.
///
/// Used by the operator-runner to set `LlmConfig::api_format =
/// Anthropic` so the LLM client targets `<api_url>/messages` (LiteLLM's
/// native Anthropic-shape route, which resolves smooth-* aliases AND
/// preserves multi-turn tool_use / tool_result pairing). The OpenAI-compat
/// translation path silently mangles Claude tool calls on the second turn
/// per customer-service-bot research (memory:
/// `reference_litellm_native_passthrough.md`).
///
/// Matches both the smooth-* alias namespace (`smooth-judge`,
/// `smooth-fast-haiku`) and direct Anthropic model strings (`claude-...`,
/// `anthropic/...`, anything containing `haiku` / `sonnet` / `opus`).
#[must_use]
pub fn is_anthropic_family(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    if m.contains("claude") || m.contains("anthropic") || m.contains("haiku") || m.contains("sonnet") || m.contains("opus") {
        return true;
    }
    // Smooth aliases that pin to Claude in the LiteLLM gateway.
    matches!(
        m.as_str(),
        "smooth-judge" | "smooth-fast-haiku" | "smooth-reviewing-haiku" | "smooth-judge-haiku"
    )
}

/// True when the model identifier indicates a Google Gemini backing.
///
/// Used by the operator-runner to set `LlmConfig::api_format = Gemini`
/// so the LLM client targets `<api_url>/models/<model>:generateContent`
/// (LiteLLM's native pass-through route at `/gemini/v1beta`, which
/// preserves Gemini's `functionCall` / `functionResponse` content blocks).
/// The OpenAI-compat translation path silently drops Gemini tool calls
/// after the first turn per customer-service-bot research.
///
/// Matches both the smooth-* alias namespace (`smooth-fast-gemini`,
/// `smooth-judge-gemini`) and direct Gemini model strings (`gemini-...`,
/// `models/gemini-...`).
#[must_use]
pub fn is_gemini_family(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    if m.contains("gemini") {
        return true;
    }
    matches!(m.as_str(), "smooth-fast-gemini" | "smooth-judge-gemini" | "smooth-summarize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_aliases_resolve() {
        assert_eq!(for_model("smooth-coding"), Some(KIMI));
        assert_eq!(for_model("smooth-reasoning"), Some(DEEPSEEK));
        assert_eq!(for_model("smooth-fast-gemini"), Some(GEMINI));
        assert_eq!(for_model("smooth-judge"), Some(ANTHROPIC));
        assert_eq!(for_model("smooth-fast"), Some(GPT));
    }

    #[test]
    fn family_names_resolve_case_insensitive() {
        assert_eq!(for_model("claude-haiku-4-5-20251001"), Some(ANTHROPIC));
        assert_eq!(for_model("Claude-Sonnet-4.6"), Some(ANTHROPIC));
        assert_eq!(for_model("kimi-k2-thinking"), Some(KIMI));
        assert_eq!(for_model("gpt-5.4-mini"), Some(GPT));
        assert_eq!(for_model("gemini-3-flash"), Some(GEMINI));
        assert_eq!(for_model("deepseek-v3.2-speciale"), Some(DEEPSEEK));
        assert_eq!(for_model("glm-5.1"), Some(GLM));
        assert_eq!(for_model("qwen3-coder-plus"), Some(QWEN));
    }

    #[test]
    fn is_anthropic_family_matches_aliases_and_models() {
        // Smooth alias namespace
        assert!(is_anthropic_family("smooth-judge"));
        assert!(is_anthropic_family("smooth-fast-haiku"));
        assert!(is_anthropic_family("smooth-reviewing-haiku"));

        // Direct Anthropic model strings
        assert!(is_anthropic_family("claude-haiku-4-5"));
        assert!(is_anthropic_family("claude-haiku-4-5-20251001"));
        assert!(is_anthropic_family("claude-sonnet-4-6"));
        assert!(is_anthropic_family("Claude-Opus-4-7"));
        assert!(is_anthropic_family("anthropic/claude-haiku-4-5"));

        // Substring matches
        assert!(is_anthropic_family("haiku-3-5"));
        assert!(is_anthropic_family("opus-4"));
        assert!(is_anthropic_family("sonnet-4-5"));

        // Negative cases — must NOT match non-Anthropic models
        assert!(!is_anthropic_family("gpt-5-mini"));
        assert!(!is_anthropic_family("smooth-coding"));
        assert!(!is_anthropic_family("smooth-reasoning"));
        assert!(!is_anthropic_family("smooth-fast-gemini"));
        assert!(!is_anthropic_family("kimi-k2-thinking"));
        assert!(!is_anthropic_family("gemini-2.5-flash"));
        assert!(!is_anthropic_family("deepseek-chat"));
    }

    #[test]
    fn is_gemini_family_matches_aliases_and_models() {
        // Smooth alias namespace
        assert!(is_gemini_family("smooth-fast-gemini"));
        assert!(is_gemini_family("smooth-judge-gemini"));
        assert!(is_gemini_family("smooth-summarize"));

        // Direct Gemini model strings
        assert!(is_gemini_family("gemini-2.5-flash"));
        assert!(is_gemini_family("gemini-3-flash-preview"));
        assert!(is_gemini_family("gemini-3.1-flash-lite-preview"));
        assert!(is_gemini_family("gemini-3-pro-preview"));
        assert!(is_gemini_family("Gemini-3.1-Pro-Preview"));
        assert!(is_gemini_family("models/gemini-2.5-flash"));

        // Negative cases — must NOT match non-Gemini models
        assert!(!is_gemini_family("gpt-5-mini"));
        assert!(!is_gemini_family("claude-haiku-4-5"));
        assert!(!is_gemini_family("smooth-coding"));
        assert!(!is_gemini_family("smooth-reasoning"));
        assert!(!is_gemini_family("smooth-judge"));
        assert!(!is_gemini_family("kimi-k2-thinking"));
        assert!(!is_gemini_family("deepseek-chat"));
    }

    #[test]
    fn unknown_model_returns_none() {
        assert_eq!(for_model(""), None);
        assert_eq!(for_model("something-completely-unknown"), None);
    }

    #[test]
    fn smooth_fast_gemini_does_not_match_smooth_fast_first() {
        // smooth-fast-gemini must hit the gemini arm, not the smooth-fast
        // arm — order of the prefix checks matters. Guards against a
        // refactor that re-orders the alias arms alphabetically.
        assert_eq!(for_model("smooth-fast-gemini"), Some(GEMINI));
        assert_ne!(for_model("smooth-fast-gemini"), Some(GPT));
    }

    #[test]
    fn overlays_are_nonempty_and_have_provider_heading() {
        // Each overlay must be a real document, not an empty file. The
        // include_str! at compile time would catch a missing file but
        // not a zero-byte one. Provider heading is the single visible
        // marker that the overlay landed in the model's context.
        for &overlay in &[ANTHROPIC, GPT, GEMINI, KIMI, DEEPSEEK, GLM, QWEN] {
            assert!(overlay.len() > 100, "overlay too short: {} chars", overlay.len());
            assert!(overlay.starts_with("# Provider notes"), "overlay missing heading: {overlay:.40}");
        }
    }
}
