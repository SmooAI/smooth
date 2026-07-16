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
//! ## Default posture (allow-benign, deny-dangerous)
//!
//! On first run with no `~/.smooth/permissions.toml`, [`load_rules`] writes a
//! documented [`STARTER_PERMISSIONS`] starter (`default = "allow"` + a static
//! deny list of clearly-dangerous ops) and adopts it. So out of the box the gate
//! runs benign calls without prompting and blocks only the dangerous ones — narc
//! (Gate 2) is the semantic backstop for context-dependent danger.
//!
//! ## Tool-name bridge
//!
//! The rule matchers use Claude-Code capability names (`Bash`, `Read`,
//! `Write`), but the operator's tools have their own names (`bash`,
//! `read_file`, `write_file`, …). [`classify`] maps the operator tool to a
//! capability + its primary argument so a `permissions.toml` written in
//! Claude-Code syntax matches. Shell tools route through
//! [`PermissionRules::decide_bash`] (compound-command aware).

use std::path::{Path, PathBuf};

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

/// The starter `permissions.toml` written on first run when none exists. Posture:
/// **allow everything, deny only clearly-dangerous ops** — narc (Gate 2) is the
/// semantic backstop for context-dependent danger (`rm -rf`, `curl | sh`, …).
/// Single source of truth for both the on-disk write and the read-only fallback.
const STARTER_PERMISSIONS: &str = r#"# Big Smooth auto-mode (Gate 1). Posture: ALLOW everything, block ONLY dangerous ops.
# narc (Gate 2, the LLM safety judge) independently blocks context-dependent danger —
# `rm -rf`, `curl | sh`, secret exfiltration, prompt injection. This static deny list
# covers operations that are catastrophic in ALL forms + writes to sensitive paths.
# Everything benign (read/list/grep/web_search/knowledge_search/th/most bash) runs
# without prompting. Edit this file to tighten or loosen the policy.
default = "allow"

deny = [
    # Privilege escalation & machine control
    "Bash(sudo:*)",
    "Bash(su:*)",
    "Bash(shutdown:*)",
    "Bash(reboot:*)",
    "Bash(halt:*)",
    "Bash(poweroff:*)",
    # Disk / filesystem destroyers
    "Bash(dd:*)",
    "Bash(mkfs:*)",
    "Bash(diskutil:*)",
    "Bash(fdisk:*)",
    # macOS persistence / kernel / firmware
    "Bash(launchctl:*)",
    "Bash(kextload:*)",
    "Bash(nvram:*)",
    "Bash(crontab:*)",
    # Writes to system + credential locations
    "Write(/etc/**)",
    "Write(/System/**)",
    "Write(/usr/**)",
    "Write(/bin/**)",
    "Write(/sbin/**)",
    "Write(/Library/**)",
    "Write(**/.ssh/**)",
    "Write(**/.aws/**)",
    "Write(**/Library/LaunchAgents/**)",
    "Write(**/.smooth/auth/**)",
]
"#;

/// Atomically write the starter policy to `path` (temp file + rename), creating
/// the parent dir if needed. Best-effort atomicity; the temp lives beside the
/// target so the rename stays on the same filesystem.
fn write_starter(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, STARTER_PERMISSIONS)?;
    std::fs::rename(&tmp, path)
}

/// Load the permission rule set from `~/.smooth/permissions.toml`.
fn load_rules() -> PermissionRules {
    load_rules_at(&rules_path())
}

/// Load rules from `path`, or on **first run** (file absent) write + adopt the
/// documented [`STARTER_PERMISSIONS`] starter (posture: allow-benign, deny-dangerous)
/// so the default is a transparent on-disk file the operator can audit and edit —
/// not a hidden in-code default.
///
/// - **Present + valid** → load it (respects user edits; never overwritten).
/// - **Present + malformed** → fail-safe default (all `ask`). A broken user edit
///   still fails closed rather than getting clobbered.
/// - **Absent** → write the starter atomically, then load the embedded starter.
///   If the write fails (read-only fs, perms), still adopt the embedded starter
///   in-memory so the running daemon gets the intended posture — never the old
///   all-`ask` default.
fn load_rules_at(path: &Path) -> PermissionRules {
    match std::fs::read_to_string(path) {
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
        // Missing file → adopt the documented starter (allow-benign, deny-dangerous).
        Err(_) => {
            match write_starter(path) {
                Ok(()) => tracing::info!(path = %path.display(), "auto-mode: no permissions.toml — wrote starter policy (allow-benign, deny-dangerous)"),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "auto-mode: could not persist starter permissions.toml — using embedded starter policy in-memory");
                }
            }
            // The starter is compile-time constant and covered by tests, so a parse
            // failure here is a build error, not a runtime one.
            PermissionRules::from_toml(STARTER_PERMISSIONS).expect("embedded STARTER_PERMISSIONS must parse")
        }
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

    #[test]
    fn starter_policy_parses_allows_benign_denies_dangerous() {
        let r = PermissionRules::from_toml(STARTER_PERMISSIONS).expect("starter policy parses");
        // default = allow ⇒ benign / unknown ops proceed without prompting.
        assert_eq!(r.decide_bash("ls -la"), Decision::Allow, "benign bash allowed");
        assert_eq!(r.decide("Read", "/x"), Decision::Allow, "read allowed");
        assert_eq!(r.decide("Some_mcp_tool", ""), Decision::Allow, "unknown tool → default allow");
        // Dangerous ops are denied.
        assert_eq!(r.decide_bash("sudo rm -rf /"), Decision::Deny, "sudo denied");
        assert_eq!(r.decide_bash("dd if=/dev/zero of=/dev/disk0"), Decision::Deny, "dd denied");
        assert_eq!(r.decide("Write", "/etc/hosts"), Decision::Deny, "write to /etc denied");
        assert_eq!(r.decide("Write", "/home/me/.ssh/id_rsa"), Decision::Deny, "write to .ssh denied");
    }

    #[test]
    fn load_rules_writes_starter_when_absent_then_loads_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("permissions.toml");
        assert!(!path.exists(), "precondition: file absent");

        let rules = load_rules_at(&path);

        assert!(path.exists(), "starter written to disk");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            STARTER_PERMISSIONS,
            "on-disk content is the starter verbatim"
        );
        // Loaded rules carry the allow-benign, deny-dangerous posture.
        assert_eq!(rules.decide("Read", "/x"), Decision::Allow, "loaded starter allows benign");
        assert_eq!(rules.decide_bash("sudo x"), Decision::Deny, "loaded starter denies dangerous");
    }

    #[test]
    fn load_rules_does_not_clobber_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.toml");
        let custom = "default = \"deny\"\n";
        std::fs::write(&path, custom).unwrap();

        let rules = load_rules_at(&path);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), custom, "existing file left untouched");
        // The user's default=deny is honoured, proving we loaded their file, not the starter.
        assert_eq!(
            rules.decide("Read", "/x"),
            Decision::Deny,
            "existing default=deny loaded, not the starter's allow"
        );
    }

    #[test]
    fn load_rules_malformed_file_fails_safe_to_all_ask() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.toml");
        std::fs::write(&path, "default = \"not-a-decision\"\n").unwrap();

        let rules = load_rules_at(&path);

        // Fail-safe default: no starter clobber, everything asks.
        assert_eq!(rules.decide("Read", "/x"), Decision::Ask, "malformed → fail-safe all-ask");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "default = \"not-a-decision\"\n",
            "malformed file not clobbered"
        );
    }
}
