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
