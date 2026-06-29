//! Gate 1 enforcement at the tool boundary (EPIC th-c89c2a, th-515a13).
//!
//! Loads the user's deny/ask/allow rules from `~/.smooth/permissions.toml`
//! (override with `SMOOTH_PERMISSIONS_FILE`) and exposes the **deny** verdict to
//! the `bash` tool — a configurable complement to the hardcoded
//! [`crate::guard`] circuit-breaker. This enforces only `Deny` here: `Ask`
//! (per-call human confirmation) needs the operator's `write_confirmation_required`
//! HITL routed per-call, which requires a host ToolHook seam in the operator
//! (tracked separately); until then `Ask`/`Allow` both proceed, and the
//! name-based operator HITL (`SMOOTH_AGENT_CONFIRM_TOOLS`) covers confirmation.
//!
//! Rules load **once** (process-global). The default when no file is present is
//! the empty rule set, whose bash verdict is never `Deny` — so an unconfigured
//! daemon behaves exactly as before.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use smooth_policy::auto_mode::{Decision, PermissionRules};

/// The path the permission rules load from: `SMOOTH_PERMISSIONS_FILE` if set,
/// else `~/.smooth/permissions.toml`.
fn permissions_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SMOOTH_PERMISSIONS_FILE") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    dirs_next::home_dir().map(|h| h.join(".smooth").join("permissions.toml"))
}

/// Load rules from `path` (best-effort): a missing file or parse error yields the
/// empty rule set (logged), so a bad config never bricks the agent.
fn load_rules_from(path: &Path) -> PermissionRules {
    match std::fs::read_to_string(path) {
        Ok(s) => PermissionRules::from_toml(&s).unwrap_or_else(|e| {
            tracing::warn!(path = %path.display(), error = %e, "ignoring malformed permissions.toml");
            PermissionRules::default()
        }),
        Err(_) => PermissionRules::default(),
    }
}

fn rules() -> &'static PermissionRules {
    static RULES: OnceLock<PermissionRules> = OnceLock::new();
    RULES.get_or_init(|| permissions_path().map_or_else(PermissionRules::default, |p| load_rules_from(&p)))
}

/// Whether the configured Gate-1 rules **deny** this bash command (accounting for
/// compound commands + wrappers via [`PermissionRules::decide_bash`]).
#[must_use]
pub fn bash_denied(command: &str) -> bool {
    rules().decide_bash(command) == Decision::Deny
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn loads_deny_rules_and_denies_matching_bash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.toml");
        std::fs::write(&path, "deny = [\"Bash(rm:*)\"]\nallow = [\"Bash(ls:*)\"]\n").unwrap();
        let rules = load_rules_from(&path);
        assert_eq!(rules.decide_bash("rm -rf /"), Decision::Deny);
        assert_eq!(rules.decide_bash("ls && rm x"), Decision::Deny, "compound: rm subcommand denied");
        assert_ne!(rules.decide_bash("ls -la"), Decision::Deny);
    }

    #[test]
    fn missing_file_is_empty_and_never_denies() {
        let dir = tempfile::tempdir().unwrap();
        let rules = load_rules_from(&dir.path().join("nope.toml"));
        assert_ne!(rules.decide_bash("rm -rf /"), Decision::Deny, "no config → no policy deny");
    }

    #[test]
    fn malformed_file_falls_back_to_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "deny = [\"Bash(\"]\n").unwrap();
        let rules = load_rules_from(&path);
        assert_ne!(rules.decide_bash("rm -rf /"), Decision::Deny, "bad config → safe empty fallback");
    }
}
