//! The New Session kind picker (spec §6).
//!
//! The engine's harness list in the user's order, hidden harnesses omitted,
//! Shell last; a not-installed
//! harness disabled with its reason, a degraded one flagged "needs setup"
//! and still startable.

use serde::{Deserialize, Serialize};

/// The harness doctor's verdict (`health`), when the daemon has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// `works` | `degraded` | `not_installed`.
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The one command that fixes `reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

/// One row of `flow.hello.harnesses` / `flow.harnesses`, reduced to what the
/// picker reads. Unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Harness {
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub state_source: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub order_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<Health>,
}

impl Harness {
    /// A minimal installed harness, for tests and vectors.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            display_name: String::new(),
            installed: true,
            reason: None,
            state_source: "hooks".to_string(),
            hidden: false,
            order_index: 0,
            health: None,
        }
    }

    /// The display name, else the name.
    #[must_use]
    pub fn display(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.name
        } else {
            &self.display_name
        }
    }

    /// Launchable, but the doctor found something that breaks part of a session.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.installed && self.health.as_ref().is_some_and(|h| h.verdict == "degraded")
    }
}

/// The shell entry's kind.
pub const SHELL: &str = "shell";

/// One row of the kind picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerRow {
    /// The session `kind` `flow.new` sends.
    pub kind: String,
    /// The display name, with why it's disabled or flagged when it is.
    pub label: String,
    /// False for a harness that isn't installed. Degraded stays true.
    pub enabled: bool,
    pub needs_setup: bool,
    /// Why it is disabled or degraded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The command to copy that fixes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

/// The picker rows: `harnesses` in the order the engine sent (the user's),
/// hidden ones dropped, and Shell last (a `shell` row from the engine is
/// moved there rather than doubled).
#[must_use]
pub fn picker(harnesses: &[Harness], include_shell: bool) -> Vec<PickerRow> {
    let mut rows: Vec<PickerRow> = harnesses
        .iter()
        .filter(|h| !h.hidden && h.name != SHELL)
        .map(|h| {
            let health_reason = h.health.as_ref().and_then(|x| x.reason.clone());
            let fix = h.health.as_ref().and_then(|x| x.fix.clone());
            if !h.installed {
                let reason = h.reason.clone().or(health_reason).unwrap_or_else(|| "not installed".to_string());
                PickerRow {
                    kind: h.name.clone(),
                    label: format!("{} — {reason}", h.display()),
                    enabled: false,
                    needs_setup: false,
                    reason: Some(reason),
                    fix,
                }
            } else if h.is_degraded() {
                PickerRow {
                    kind: h.name.clone(),
                    label: format!("{} — needs setup", h.display()),
                    enabled: true,
                    needs_setup: true,
                    reason: Some(health_reason.unwrap_or_else(|| "needs setup".to_string())),
                    fix,
                }
            } else {
                PickerRow {
                    kind: h.name.clone(),
                    label: h.display().to_string(),
                    enabled: true,
                    needs_setup: false,
                    reason: None,
                    fix: None,
                }
            }
        })
        .collect();
    if include_shell {
        rows.push(PickerRow {
            kind: SHELL.to_string(),
            label: "Shell".to_string(),
            enabled: true,
            needs_setup: false,
            reason: None,
            fix: None,
        });
    }
    rows
}

/// What a fresh sheet selects: the first startable row.
#[must_use]
pub fn default_kind(rows: &[PickerRow]) -> Option<String> {
    rows.iter().find(|r| r.enabled).map(|r| r.kind.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_dropped_shell_last_disabled_and_degraded() {
        let mut missing = Harness::new("codex");
        missing.installed = false;
        missing.reason = Some("codex not on PATH".into());
        let mut degraded = Harness::new("opencode");
        degraded.display_name = "OpenCode".into();
        degraded.health = Some(Health {
            verdict: "degraded".into(),
            reason: Some("hooks untrusted".into()),
            fix: Some("th harness enable opencode".into()),
        });
        let mut hidden = Harness::new("aider");
        hidden.hidden = true;
        let engine_shell = Harness::new("shell");
        let rows = picker(&[missing, engine_shell, Harness::new("claude"), degraded, hidden], true);
        let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds, ["codex", "claude", "opencode", "shell"]);
        assert_eq!((rows[0].enabled, rows[0].label.as_str()), (false, "codex — codex not on PATH"));
        assert!(rows[2].enabled && rows[2].needs_setup);
        assert_eq!(rows[2].label, "OpenCode — needs setup");
        assert_eq!(rows[2].fix.as_deref(), Some("th harness enable opencode"));
        assert_eq!(default_kind(&rows).as_deref(), Some("claude"));
        assert_eq!(default_kind(&picker(&[], true)).as_deref(), Some("shell"));
        assert!(picker(&[], false).is_empty());
    }
}
