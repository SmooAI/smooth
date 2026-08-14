//! User-configurable controls for Narc's LLM safety judge (pearls th-eec7a5,
//! th-7aa2af).
//!
//! Narc ([`crate::hooks::NarcHook`]) runs regex detectors on every tool call and
//! escalates ambiguous hits to a fail-closed LLM judge. That judge used to be
//! always-on with a fixed fast model and no user control. This module is the
//! runtime knob store the daemon shares between [`NarcHook`](crate::hooks::NarcHook)
//! and the `/api/judge` route (same cheap-`Arc`-clone shape as
//! [`SessionModes`](crate::session_mode::SessionModes)):
//!
//! - **enabled** — turn the LLM-judge escalation on/off. **Off does NOT disable
//!   safety**: the permission gate's `DenyPolicy` circuit-breakers, Narc's
//!   hard-signal detectors (dangerous-CLI hard block, unambiguous
//!   `Block`-severity destruction/exfiltration), the effect-based shell restore,
//!   and secret redaction all still run. Off only removes the LLM *escalation*
//!   tier, so ambiguous `Alert`-severity hits are alert-only instead of being
//!   adjudicated by the judge.
//! - **strictness** — which detector severities escalate vs. alert-only (see
//!   [`Strictness`]).
//! - **model** — which model the judge runs as, selectable independently of the
//!   chat model (th-7aa2af, the first "role slot"). Defaults to the daemon's
//!   fast model.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::hooks::narc::Severity;

/// How aggressively Narc treats a detector finding.
///
/// The two orthogonal knobs a level sets are [`Strictness::escalates`] (does a
/// finding of a given severity go to the judge?) and
/// [`Strictness::blocks_without_judge`] (with no judge available — disabled, or
/// no gateway — does it hard-block?). Each adjacent pair of levels differs on
/// exactly one axis, so every level is behaviourally distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strictness {
    /// Only hard-signal (`Block`) findings are acted on. Ambiguous `Alert`
    /// findings are alert-only even when a judge is available — fewer prompts,
    /// more trust in the model.
    Lenient,
    /// The default. `Alert` findings escalate to the judge; with no judge,
    /// `Alert` is alert-only and `Block` hard-blocks.
    #[default]
    Normal,
    /// Like `Normal` for escalation, but fails closed harder: with no judge,
    /// even an ambiguous `Alert` finding hard-blocks.
    Strict,
}

impl Strictness {
    /// Parse a wire value (`"lenient"`/`"normal"`/`"strict"`, case-insensitive).
    /// Anything else ⇒ `None`, so a bad value is rejected rather than silently
    /// coerced (which could weaken the safety posture).
    // No `#[must_use]`: the `Option` return is already `#[must_use]` (clippy
    // `double_must_use` — CI's clippy is stricter than the local toolchain).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lenient" => Some(Self::Lenient),
            "normal" => Some(Self::Normal),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }

    /// The wire string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lenient => "lenient",
            Self::Normal => "normal",
            Self::Strict => "strict",
        }
    }

    /// Does a finding of `sev` escalate to the LLM judge (when one is available)?
    /// `Lenient` ignores ambiguous `Alert` hits; `Normal`/`Strict` escalate them.
    #[must_use]
    pub fn escalates(self, sev: Severity) -> bool {
        match self {
            Self::Lenient => sev == Severity::Block,
            Self::Normal | Self::Strict => true,
        }
    }

    /// With no judge available (disabled, or no gateway key), does a finding of
    /// `sev` hard-block? `Lenient`/`Normal` block only unambiguous `Block`
    /// findings; `Strict` also blocks ambiguous `Alert` findings (fail-closed).
    #[must_use]
    pub fn blocks_without_judge(self, sev: Severity) -> bool {
        match self {
            Self::Lenient | Self::Normal => sev == Severity::Block,
            Self::Strict => true,
        }
    }
}

/// The judge's runtime configuration — the three knobs `/api/judge` exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeConfig {
    /// Whether the LLM-judge escalation tier is on. See the module docs: off
    /// keeps every non-LLM safety layer running.
    pub enabled: bool,
    /// Which detector severities escalate vs. alert-only.
    pub strictness: Strictness,
    /// The model the judge runs as (independent of the chat model, th-7aa2af).
    pub model: String,
}

impl JudgeConfig {
    /// The shipped defaults: judge on, `Normal` strictness, `model` (the daemon's
    /// fast model — the caller passes it so this module doesn't hardcode a model
    /// string that lives in `operator.rs`).
    #[must_use]
    pub fn defaults(model: String) -> Self {
        Self {
            enabled: true,
            strictness: Strictness::Normal,
            model,
        }
    }
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self::defaults(String::new())
    }
}

/// The shared judge-settings store. Cheap to clone (state is behind an `Arc`), so
/// the `NarcHook` and the `/api/judge` route hold the same one.
#[derive(Clone, Default)]
pub struct JudgeSettings {
    inner: Arc<Mutex<JudgeConfig>>,
}

impl JudgeSettings {
    /// A store seeded with `cfg`.
    #[must_use]
    pub fn new(cfg: JudgeConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(cfg)),
        }
    }

    /// A snapshot of the current config.
    #[must_use]
    pub fn get(&self) -> JudgeConfig {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Replace the config wholesale.
    pub fn set(&self, cfg: JudgeConfig) {
        *self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = cfg;
    }

    /// Apply a partial update — only the `Some` fields change. Returns the config
    /// after the update, so the route can echo it. This is what lets the Settings
    /// UI toggle `enabled` without having to resend `model`/`strictness`.
    pub fn patch(&self, enabled: Option<bool>, strictness: Option<Strictness>, model: Option<String>) -> JudgeConfig {
        let mut guard = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(e) = enabled {
            guard.enabled = e;
        }
        if let Some(s) = strictness {
            guard.strictness = s;
        }
        if let Some(m) = model {
            guard.model = m;
        }
        guard.clone()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn strictness_wire_roundtrip_and_rejects_garbage() {
        for s in [Strictness::Lenient, Strictness::Normal, Strictness::Strict] {
            assert_eq!(Strictness::parse(s.as_str()), Some(s));
        }
        assert_eq!(Strictness::parse(" STRICT "), Some(Strictness::Strict));
        assert_eq!(Strictness::parse("paranoid"), None, "unknown is rejected, not coerced");
        assert_eq!(Strictness::parse(""), None);
    }

    /// The security-relevant table: which severity does each level act on, with
    /// and without a judge. A regression here silently changes the safety posture.
    #[test]
    fn strictness_severity_matrix() {
        // Escalation (judge available).
        assert!(!Strictness::Lenient.escalates(Severity::Alert), "lenient ignores ambiguous hits");
        assert!(Strictness::Lenient.escalates(Severity::Block), "lenient still escalates hard signals");
        assert!(Strictness::Normal.escalates(Severity::Alert));
        assert!(Strictness::Strict.escalates(Severity::Alert));

        // No judge → block?
        assert!(!Strictness::Lenient.blocks_without_judge(Severity::Alert));
        assert!(Strictness::Lenient.blocks_without_judge(Severity::Block));
        assert!(!Strictness::Normal.blocks_without_judge(Severity::Alert));
        assert!(Strictness::Normal.blocks_without_judge(Severity::Block));
        assert!(Strictness::Strict.blocks_without_judge(Severity::Alert), "strict fails closed on ambiguity");
        assert!(Strictness::Strict.blocks_without_judge(Severity::Block));
    }

    #[test]
    fn store_patch_updates_only_named_fields() {
        let s = JudgeSettings::new(JudgeConfig::defaults("fast".into()));
        // Toggle enabled without touching model/strictness.
        let after = s.patch(Some(false), None, None);
        assert!(!after.enabled);
        assert_eq!(after.model, "fast");
        assert_eq!(after.strictness, Strictness::Normal);
        // Change model only.
        let after = s.patch(None, None, Some("judge-x".into()));
        assert!(!after.enabled, "enabled stayed off");
        assert_eq!(after.model, "judge-x");
        // Set strictness only.
        let after = s.patch(None, Some(Strictness::Strict), None);
        assert_eq!(after.strictness, Strictness::Strict);
        assert_eq!(s.get(), after, "get reflects the last patch");
    }

    #[test]
    fn config_json_roundtrips() {
        let cfg = JudgeConfig {
            enabled: false,
            strictness: Strictness::Strict,
            model: "m".into(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(serde_json::from_str::<JudgeConfig>(&json).unwrap(), cfg);
        // lowercase wire spelling for strictness.
        assert!(json.contains("\"strict\""), "{json}");
    }
}
