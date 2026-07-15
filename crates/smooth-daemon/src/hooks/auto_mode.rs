//! [`AutoModeHook`] — the permission gate (pearls th-3119e3 + th-515a13).
//!
//! A `ToolHook` whose `pre_call` runs the deterministic
//! [`smooth_policy::auto_mode`] rule engine on every tool call and turns its
//! `allow` / `deny` / `ask` verdict into proceed / block, subject to the
//! operator-selected [`AutoMode`].
//!
//! ## Fail-closed on `ask`
//!
//! The rule engine's third verdict is **ask** — "a human should confirm this".
//! Big Smooth's daemon does not yet have an interactive approval queue (that is
//! th-1f7fd7). Until it does, an `ask` verdict **fails closed to a deny** in the
//! default `ask` mode. `accept-edits` auto-accepts `ask` verdicts; `bypass`
//! skips the gate entirely (surveillance still runs); `deny` is the headless
//! posture where anything not explicitly allowed is denied.
//!
//! ## Tool-name bridge
//!
//! The rule matchers use Claude-Code capability names (`Bash`, `Read`,
//! `Write`), but the operator's tools have their own names (`bash`,
//! `read_file`, `write_file`, …). [`classify`] maps the operator tool to a
//! capability + its primary argument so a `permissions.toml` written in
//! Claude-Code syntax matches. Shell tools route through
//! [`PermissionRules::decide_bash`] (compound-command aware).

use std::path::PathBuf;

use async_trait::async_trait;
use smooth_operator::tool::{ToolCall, ToolHook};
use smooth_policy::auto_mode::{Decision, PermissionRules};

/// The operator-selected permission posture, from `SMOOTH_AUTO_MODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoMode {
    /// Default. Run the rule engine; an `ask` verdict fails closed (deny) until
    /// the interactive approval queue lands (th-1f7fd7).
    Ask,
    /// Auto-accept `ask` verdicts (still honours explicit `deny` rules + narc).
    AcceptEdits,
    /// Headless: anything not explicitly `allow`ed is denied (`ask` → deny too).
    Deny,
    /// Skip the permission gate entirely — every call proceeds. Surveillance
    /// (narc) and the engine's own hard circuit-breakers still run.
    Bypass,
}

impl AutoMode {
    /// Parse `SMOOTH_AUTO_MODE`. Unset / unknown ⇒ [`AutoMode::Ask`] (fail-safe).
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var("SMOOTH_AUTO_MODE")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("accept-edits" | "accept_edits" | "acceptedits" | "accept") => Self::AcceptEdits,
            Some("bypass" | "allow") => Self::Bypass,
            Some("deny") => Self::Deny,
            // "ask", "", unknown → the fail-safe default.
            _ => Self::Ask,
        }
    }
}

/// Path to the permission rule set (`~/.smooth/permissions.toml`).
fn rules_path() -> PathBuf {
    dirs_next::home_dir().map_or_else(|| PathBuf::from("permissions.toml"), |h| h.join(".smooth").join("permissions.toml"))
}

/// Load `~/.smooth/permissions.toml` if present, else the fail-safe default
/// (every call `ask`s). A malformed file logs and falls back to the default.
//
// ponytail: no invented baseline allow-list. With the default (empty) rule set
// every call is `ask`, so in the default `ask` mode the gate denies everything
// until the operator supplies a permissions.toml OR runs with
// SMOOTH_AUTO_MODE=bypass / accept-edits. That is the fail-closed posture the
// security model wants; a usable default allow-list is the operator's call (or a
// follow-up once th-1f7fd7 lands the interactive queue).
fn load_rules() -> PermissionRules {
    let path = rules_path();
    match std::fs::read_to_string(&path) {
        Ok(toml) => match PermissionRules::from_toml(&toml) {
            Ok(rules) => {
                tracing::info!(path = %path.display(), "auto-mode: loaded permission rules");
                rules
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "auto-mode: malformed permissions.toml — falling back to fail-safe default (all ask)");
                PermissionRules::default()
            }
        },
        // Missing file is the normal case — fail-safe default.
        Err(_) => PermissionRules::default(),
    }
}

/// Map an operator tool call to `(capability, primary_arg, is_shell)` for the
/// rule engine. Shell tools carry their command; file tools carry their path;
/// everything else maps its name to a capitalized capability and an empty arg
/// (so a bare `Tool` matcher can still target it).
fn classify(call: &ToolCall) -> (String, String, bool) {
    let arg = |keys: &[&str]| -> String {
        for k in keys {
            if let Some(s) = call.arguments.get(*k).and_then(serde_json::Value::as_str) {
                return s.to_string();
            }
        }
        String::new()
    };
    match call.name.as_str() {
        "bash" | "shell_exec" | "bg_run" => ("Bash".to_string(), arg(&["command", "cmd"]), true),
        "read_file" | "read" | "grep" | "glob" | "list_files" | "list" | "ls" => ("Read".to_string(), arg(&["path", "file_path", "pattern"]), false),
        "write_file" | "write" | "edit" | "edit_file" | "apply_patch" => ("Write".to_string(), arg(&["path", "file_path"]), false),
        // Unknown tool: capitalize the name as a bespoke capability with no arg,
        // so a bare `Tool` matcher in permissions.toml still applies.
        other => {
            let mut caps = other.chars();
            let capitalized = caps.next().map_or_else(String::new, |c| c.to_uppercase().collect::<String>() + caps.as_str());
            (capitalized, String::new(), false)
        }
    }
}

/// The permission-gate hook. Installed FIRST on the operator's registry.
pub struct AutoModeHook {
    rules: PermissionRules,
    mode: AutoMode,
}

impl AutoModeHook {
    /// Build with an explicit rule set + mode (used by tests).
    #[must_use]
    pub fn new(rules: PermissionRules, mode: AutoMode) -> Self {
        Self { rules, mode }
    }

    /// Build from the environment: rules from `~/.smooth/permissions.toml`,
    /// mode from `SMOOTH_AUTO_MODE`.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(load_rules(), AutoMode::from_env())
    }

    /// The rule-engine verdict for a call (shell tools are compound-aware).
    fn decide(&self, call: &ToolCall) -> Decision {
        let (cap, arg, is_shell) = classify(call);
        if is_shell {
            self.rules.decide_bash(&arg)
        } else {
            self.rules.decide(&cap, &arg)
        }
    }

    /// Resolve a call to Ok (proceed) or Err (block) under the active mode.
    fn gate(&self, call: &ToolCall) -> anyhow::Result<()> {
        // Bypass skips the gate entirely — narc still runs (separate hook).
        if self.mode == AutoMode::Bypass {
            return Ok(());
        }
        match self.decide(call) {
            Decision::Allow => Ok(()),
            Decision::Deny => {
                anyhow::bail!("auto-mode: tool `{}` denied by permission policy", call.name)
            }
            Decision::Ask => match self.mode {
                // accept-edits auto-accepts asks; Bypass is unreachable here
                // (handled above) but shares the proceed arm.
                AutoMode::AcceptEdits | AutoMode::Bypass => Ok(()),
                // ponytail: fail-closed-on-ask is the current ceiling. There is
                // no interactive approval queue in the daemon yet, so an `ask`
                // verdict can only deny. th-1f7fd7 wires the queue (surfaced via
                // the operator's write-confirmation HITL) — swap this arm for a
                // park-and-await once it lands.
                AutoMode::Ask => anyhow::bail!(
                    "auto-mode: tool `{}` requires approval (interactive approval not yet wired — th-1f7fd7); \
                     run with SMOOTH_AUTO_MODE=accept-edits/bypass or allow it in ~/.smooth/permissions.toml",
                    call.name
                ),
                AutoMode::Deny => anyhow::bail!("auto-mode: tool `{}` denied (headless deny mode; not explicitly allowed)", call.name),
            },
        }
    }
}

#[async_trait]
impl ToolHook for AutoModeHook {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        self.gate(call)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn rules() -> PermissionRules {
        // deny rm; ask git push; allow ls + Read.
        PermissionRules::from_lists(["Bash(rm:*)"], ["Bash(git push:*)"], ["Bash(ls:*)", "Read"]).unwrap()
    }

    fn bash(cmd: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": cmd }),
        }
    }

    fn read(path: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": path }),
        }
    }

    #[tokio::test]
    async fn allow_passes() {
        let hook = AutoModeHook::new(rules(), AutoMode::Ask);
        assert!(hook.pre_call(&bash("ls -la")).await.is_ok(), "allowed command proceeds");
        assert!(hook.pre_call(&read("/tmp/x")).await.is_ok(), "allowed Read proceeds");
    }

    #[tokio::test]
    async fn deny_blocks() {
        let hook = AutoModeHook::new(rules(), AutoMode::Ask);
        let err = hook.pre_call(&bash("rm -rf /")).await.unwrap_err();
        assert!(err.to_string().contains("denied"), "deny rule blocks: {err}");
    }

    #[tokio::test]
    async fn deny_wins_inside_compound_command() {
        // `ls && rm -rf /` — the allowed ls must not smuggle the denied rm through.
        let hook = AutoModeHook::new(rules(), AutoMode::Ask);
        assert!(hook.pre_call(&bash("ls && rm -rf /")).await.is_err(), "deny subcommand wins");
    }

    #[tokio::test]
    async fn ask_fails_closed_in_default_mode() {
        // `git push` is an ask rule; default (ask) mode fails closed to deny.
        let hook = AutoModeHook::new(rules(), AutoMode::Ask);
        let err = hook.pre_call(&bash("git push origin main")).await.unwrap_err();
        assert!(err.to_string().contains("requires approval"), "ask → fail-closed: {err}");
    }

    #[tokio::test]
    async fn ask_denies_in_deny_mode() {
        let hook = AutoModeHook::new(rules(), AutoMode::Deny);
        assert!(hook.pre_call(&bash("git push origin main")).await.is_err(), "deny mode blocks ask");
    }

    #[tokio::test]
    async fn accept_edits_auto_accepts_ask() {
        let hook = AutoModeHook::new(rules(), AutoMode::AcceptEdits);
        assert!(hook.pre_call(&bash("git push origin main")).await.is_ok(), "accept-edits auto-accepts ask");
        // …but an explicit deny still blocks.
        assert!(hook.pre_call(&bash("rm -rf /")).await.is_err(), "accept-edits still honours deny");
    }

    #[tokio::test]
    async fn bypass_allows_everything() {
        let hook = AutoModeHook::new(rules(), AutoMode::Bypass);
        assert!(hook.pre_call(&bash("rm -rf /")).await.is_ok(), "bypass proceeds even past a deny rule");
        assert!(hook.pre_call(&bash("anything at all")).await.is_ok());
    }

    #[tokio::test]
    async fn default_rules_ask_everything_and_fail_closed() {
        // Fail-safe: with no permissions.toml, every call asks → default mode denies.
        let hook = AutoModeHook::new(PermissionRules::default(), AutoMode::Ask);
        assert!(hook.pre_call(&bash("ls")).await.is_err(), "empty rules + ask mode = fail closed");
    }

    #[test]
    fn mode_from_env_parses_and_defaults() {
        for (val, want) in [
            ("accept-edits", AutoMode::AcceptEdits),
            ("bypass", AutoMode::Bypass),
            ("deny", AutoMode::Deny),
            ("ask", AutoMode::Ask),
            ("", AutoMode::Ask),
            ("nonsense", AutoMode::Ask),
        ] {
            std::env::set_var("SMOOTH_AUTO_MODE", val);
            assert_eq!(AutoMode::from_env(), want, "SMOOTH_AUTO_MODE={val:?}");
        }
        std::env::remove_var("SMOOTH_AUTO_MODE");
        assert_eq!(AutoMode::from_env(), AutoMode::Ask, "unset → ask");
    }

    #[test]
    fn classify_maps_operator_tools_to_capabilities() {
        assert_eq!(classify(&bash("ls")).0, "Bash");
        assert!(classify(&bash("ls")).2, "bash is a shell tool");
        assert_eq!(classify(&read("/x")).0, "Read");
        let (cap, arg, is_shell) = classify(&ToolCall {
            id: "c".into(),
            name: "some_mcp_tool".into(),
            arguments: serde_json::json!({}),
        });
        assert_eq!(cap, "Some_mcp_tool", "unknown tool capitalizes its name");
        assert_eq!(arg, "");
        assert!(!is_shell);
    }
}
