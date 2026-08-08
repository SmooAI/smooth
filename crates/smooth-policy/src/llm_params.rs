//! LLM request parameters that must be identical everywhere (pearl th-c127d1).
//!
//! This module exists because a single scattered literal — `temperature: 0.0`
//! — silently broke Big Smooth's entire model picker, and the fix had to be
//! applied in seven places across four crates before it held.

/// The temperature EVERY LLM request in this repo sends.
///
/// **1.0, not 0.0.** Agentic work wants determinism, which is why 0.0 was the
/// obvious choice — but a growing set of frontier models accept only their
/// default temperature and **400 the entire request**:
///
/// ```text
/// Unsupported value: 'temperature' does not support 0 with this model.
/// Only the default (1) value is supported.
/// ```
///
/// The symptom does not look like a config error. The daemon boots, accepts
/// the turn, every LLM call 400s, and the user sees an assistant that
/// silently says nothing. Every model in Big Smooth's picker except the
/// default was affected.
///
/// # Why not a per-model allowlist
///
/// Because the behaviour does not follow the names, and a table that looks
/// right would be wrong. Measured against `llm.smoo.ai` on 2026-08-07 by
/// actually calling each model:
///
/// | rejects `temperature: 0` | accepts it |
/// |---|---|
/// | `gpt-5.1`, `gpt-5.4-pro`, `gpt-5.5` | `gpt-5`, `gpt-5.2`, `gpt-5.4` |
/// | `claude-opus-4-7`, `claude-opus-4-8`, `claude-sonnet-5`, `claude-fable-5` | `claude-haiku-4-5`, `claude-sonnet-4-5`, `claude-sonnet-4-6`, `claude-opus-4-6` |
/// | | `gemini-3.5-flash`, `deepseek-v4-flash`, `deepseek-v4-pro`, `glm-5.1`, `minimax-m2.7`, `groq-gpt-oss-20b` |
///
/// `gpt-5.1` rejects while `gpt-5.2` accepts. `gpt-5.4` accepts while
/// `gpt-5.4-pro` rejects. There is no prefix rule to write, and the set moves
/// every time a provider ships a model.
///
/// `1.0` was accepted by **all 12 models tested across 6 families** — the one
/// value that works everywhere. The cost is losing temperature-0 determinism
/// on the models that would allow it, which is a fair trade against "only the
/// default model works at all".
///
/// Re-measure with `/update-models --probe`, which calls each model and
/// reports `ok (rejects temperature 0)` for the strict ones.
///
/// # The real fix, upstream
///
/// `LlmConfig::temperature` should be `Option<f32>` so we send **nothing** and
/// take each provider's own default. That lives in `smooth-operator-core`;
/// until it exists, this constant is how the repo stays consistent.
pub const AGENT_TEMPERATURE: f32 = 1.0;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn agent_temperature_is_the_universally_accepted_value() {
        assert!(
            (AGENT_TEMPERATURE - 1.0).abs() < f32::EPSILON,
            "temperature must be 1.0 — see this module's docs for the measured table (th-c127d1)"
        );
    }

    /// Workspace root, or `None` when the layout isn't what we expect (a
    /// vendored build, a packaged crate) — in which case the guard below
    /// skips rather than failing for the wrong reason.
    fn crates_dir() -> Option<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.to_path_buf();
        root.is_dir().then_some(root).filter(|p| p.join("smooth-daemon").is_dir())
    }

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                // Skip build output; it contains vendored third-party source.
                if !matches!(p.file_name().and_then(|n| n.to_str()), Some("target" | ".target-local")) {
                    rust_sources(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// The guard that makes this stick.
    ///
    /// This bug cost hours precisely because `temperature: 0.0` was written
    /// in seven places and fixing six of them looked like fixing it. A new
    /// call site that hardcodes 0 would silently break every strict model
    /// again, and the symptom (an assistant that says nothing) points
    /// nowhere near the cause. So: no source file may hardcode a zero
    /// temperature. Use [`AGENT_TEMPERATURE`].
    #[test]
    fn no_source_file_hardcodes_a_zero_temperature() {
        let Some(crates) = crates_dir() else {
            eprintln!("skipping: not running from a workspace checkout");
            return;
        };
        let mut files = Vec::new();
        rust_sources(&crates, &mut files);
        assert!(!files.is_empty(), "found no .rs files under {}", crates.display());

        let mut offenders = Vec::new();
        for f in &files {
            // This file is the detector; its own needles are not offences.
            if f.file_name().and_then(|n| n.to_str()) == Some("llm_params.rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(f) else { continue };
            for (i, line) in text.lines().enumerate() {
                let t = line.trim();
                // Only flag real assignments, not prose in a doc comment
                // (this module is full of the string "temperature: 0").
                if t.starts_with("//") {
                    continue;
                }
                let zero_temp =
                    t.contains("temperature: 0.0") || t.contains("temperature: 0,") || t.contains("\"temperature\": 0,") || t.contains("\"temperature\": 0.0");
                if zero_temp {
                    offenders.push(format!("{}:{}: {t}", f.display(), i + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "hardcoded zero temperature — frontier models 400 on it and the symptom is an agent \
             that silently says nothing (th-c127d1). Use smooth_policy::llm_params::AGENT_TEMPERATURE:\n  {}",
            offenders.join("\n  ")
        );
    }
}
