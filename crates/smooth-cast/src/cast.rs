//! # Smooth cast roles — the coding-harness roles the generic engine dropped
//!
//! The published `smooai-smooth-operator-core` engine (0.14.0) ships a
//! *generic* [`Cast`](smooth_operator::cast::Cast) populated with the
//! generic roles (`tagger`, `presser`, `recapper`, `mapper`, `heckler`,
//! `scout`, `runner`). It deliberately dropped the four coding-harness
//! roles that only the `th code` workflow used:
//!
//! - **`fixer`** — the default `th` coding experience: full tool access,
//!   `Coding`-slot routing. [`crate::coding_workflow`] looks up its prompt
//!   + slot by name.
//! - **`oracle`** — pure read-only reasoning (no bash, no mutation).
//! - **`chief`** — the Chief-of-Staff router that emits `DISPATCH: <role>`.
//! - **`intent_classifier`** — the chat TUI's `WORK`/`QUESTION` router.
//!
//! This module rebuilds those four roles on the engine's public
//! [`OperatorRole`]/[`Clearance`]/[`RoleKind`] API and exposes
//! [`builtin()`], a drop-in replacement for `Cast::builtin()` that returns
//! the generic engine roles PLUS these four. Smooth call sites that used to
//! call `smooth_operator::Cast::builtin()` and then `.get("fixer")` (etc.)
//! now call [`smooth_cast::cast::builtin()`](builtin) instead.

use smooth_operator::cast::{Cast, Clearance, OperatorRole, RoleKind};
use smooth_operator::providers::Activity;

/// System prompt for the `fixer` role. Public because
/// [`crate::coding_workflow`] documents that it resolves the coding system
/// prompt from this role by name (mirrors the old engine's
/// `cast::FIXER_PROMPT`).
pub const FIXER_PROMPT: &str = include_str!("prompts/fixer.txt");
const ORACLE_PROMPT: &str = include_str!("prompts/oracle.txt");
const CHIEF_PROMPT: &str = include_str!("prompts/chief.txt");
const INTENT_CLASSIFIER_PROMPT: &str = include_str!("prompts/intent_classifier.txt");

/// Read-only tool set used by reasoning roles (`oracle`). Anything not in
/// this list is denied. Mirrors the engine's private `read_only_tools()`
/// helper — kept here because the harness `oracle` role needs the same
/// allowlist and the engine no longer exposes it.
fn read_only_tools() -> Vec<String> {
    vec![
        "read_file".into(),
        "list_files".into(),
        "grep".into(),
        "glob".into(),
        "lsp".into(),
        "project_inspect".into(),
        // Memory is metadata, not source code — even read-only
        // reasoning roles can persist what they learn about the
        // workspace to .smooth/MEMORY.md so a later session
        // doesn't have to re-discover everything.
        "read_memory".into(),
        "write_memory".into(),
    ]
}

/// The four coding-harness [`OperatorRole`]s the generic engine dropped.
fn smooth_roles() -> Vec<OperatorRole> {
    vec![
        // `intent_classifier` is the chat TUI's auto-router: given a
        // single user message, emit literal "WORK" or "QUESTION" so
        // the dispatcher knows whether to run under fixer (coding
        // workflow) or oracle (read-only Q&A). Routes through the
        // Fast slot so it adds milliseconds, not seconds.
        OperatorRole {
            name: "intent_classifier".into(),
            kind: RoleKind::Shadow,
            slot: Activity::Fast,
            model_override: None,
            prompt: INTENT_CLASSIFIER_PROMPT.trim().to_string(),
            permissions: Clearance::deny_all(),
            steps: None,
            hidden: true,
        },
        // `chief` is the Chief of Staff router. Reads the user message
        // and emits `DISPATCH: <role>` naming one of the lead/sidekick
        // roles. Routes through the Fast slot so adding it costs
        // milliseconds, not seconds. Falls back to the heuristic
        // ladder when chief is unavailable (no providers, gateway
        // down) so dispatch never hangs.
        OperatorRole {
            name: "chief".into(),
            kind: RoleKind::Shadow,
            slot: Activity::Fast,
            model_override: None,
            prompt: CHIEF_PROMPT.trim().to_string(),
            permissions: Clearance::deny_all(),
            steps: None,
            hidden: true,
        },
        // `fixer` is the default `th` experience: full tool access,
        // Coding-slot routing. Its prompt is the coding system prompt
        // that `coding_workflow` resolves by name.
        OperatorRole {
            name: "fixer".into(),
            kind: RoleKind::Lead,
            slot: Activity::Coding,
            model_override: None,
            prompt: FIXER_PROMPT.trim().to_string(),
            permissions: Clearance::default(),
            steps: None,
            hidden: false,
        },
        // `oracle` is pure reasoning — no bash, no mutation.
        OperatorRole {
            name: "oracle".into(),
            kind: RoleKind::Lead,
            slot: Activity::Reasoning,
            model_override: None,
            prompt: ORACLE_PROMPT.trim().to_string(),
            permissions: Clearance {
                allow_tools: read_only_tools(),
                deny_tools: vec![
                    "edit_file".into(),
                    "write_file".into(),
                    "apply_patch".into(),
                    "bash".into(),
                    "bg_run".into(),
                    "http_fetch".into(),
                ],
            },
            steps: None,
            hidden: false,
        },
    ]
}

/// Build a [`Cast`] populated with the engine's generic built-in roles
/// (`tagger`, `presser`, `recapper`, `mapper`, `heckler`, `scout`,
/// `runner`) PLUS the four smooth coding-harness roles (`fixer`, `oracle`,
/// `chief`, `intent_classifier`).
///
/// Drop-in replacement for `smooth_operator::Cast::builtin()` for smooth
/// call sites that depend on the coding-harness roles being present.
pub fn builtin() -> Cast {
    let mut cast = Cast::builtin();
    for role in smooth_roles() {
        cast.register(role);
    }
    cast
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registers_the_four_harness_roles() {
        let cast = builtin();
        for name in ["fixer", "oracle", "chief", "intent_classifier"] {
            assert!(cast.get(name).is_some(), "role '{name}' must be registered");
        }
    }

    #[test]
    fn builtin_keeps_the_generic_engine_roles() {
        let cast = builtin();
        for name in ["tagger", "presser", "recapper", "mapper", "heckler", "scout", "runner"] {
            assert!(cast.get(name).is_some(), "generic engine role '{name}' must survive");
        }
    }

    #[test]
    fn fixer_is_a_coding_lead_with_bash() {
        let cast = builtin();
        let fixer = cast.get("fixer").expect("fixer registered");
        assert_eq!(fixer.kind, RoleKind::Lead);
        assert!(matches!(fixer.slot, Activity::Coding));
        assert!(fixer.permissions.allows("bash"), "fixer must allow bash");
    }

    #[test]
    fn oracle_is_read_only() {
        let cast = builtin();
        let oracle = cast.get("oracle").expect("oracle registered");
        assert!(!oracle.permissions.allows("bash"), "oracle must deny bash");
        assert!(!oracle.permissions.allows("edit_file"), "oracle must deny edit_file");
        assert!(oracle.permissions.allows("read_file"), "oracle must allow read_file");
    }
}
