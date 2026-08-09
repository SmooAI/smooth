//! Family AI — per-member roles and per-role tool RBAC (ADR-008, pearl th-12d875).
//!
//! Big Smooth is single-tenant by default. A **family** turns it into a
//! multi-principal agent: each member authenticates with their OWN local bearer
//! token, and the daemon narrows the per-turn tool set to that member's role.
//! This module is the *pure policy layer* — the config shape, a constant-time
//! token→member lookup, and a deny-by-default per-role tool decision built on
//! [`crate::auto_mode::PermissionRules`]. Enforcement (dropping denied tools from
//! the per-turn registry) lives in the daemon's tool provider; identity (mapping a
//! token to a role) lives in the daemon's auth verifier. Neither is here, so this
//! stays a hermetically testable value type.
//!
//! # Trust model
//!
//! Roles are **token-derived, never self-asserted**: a member holds only their own
//! token, so a child device cannot present the parent's. A "Smoo Jr mode" picker
//! in the app is UX only — the guardrail is which token authenticated the
//! connection, resolved here.
//!
//! # Fail-closed, everywhere
//!
//! - [`FamilyConfig::from_toml`] returns `Err` on malformed input; the daemon
//!   loader then serves NO family config, so family tokens are rejected (members
//!   locked out) rather than promoted.
//! - An **unknown role** grants nothing ([`FamilyConfig::tool_allowed`] → `false`).
//! - A role with no explicit `allow` for a tool denies it (`default = "deny"`).
//!
//! Roles are plain strings keyed into a table, so a household adds a role without a
//! recompile; `child` is the conventional Smoo Jr key.

use std::collections::HashMap;

use serde::Deserialize;

use crate::auto_mode::{Decision, PermissionRules};

/// One non-owner family member: the distinct local bearer token they present, the
/// stable id stamped onto their principal, and the role that selects their
/// [`RoleProfile`].
#[derive(Debug, Clone, Deserialize)]
pub struct MemberAuth {
    /// The distinct local bearer token this member's device presents. Compared
    /// constant-time; never logged.
    pub token: String,
    /// Stable member id → becomes the principal's `user_id`.
    pub id: String,
    /// The role key into [`FamilyConfig::roles`] (e.g. `"child"` for Smoo Jr).
    pub role: String,
    /// Optional human name for the principal's display name.
    #[serde(default)]
    pub display_name: Option<String>,
}

/// A role's clearance: the deny/ask/allow tool rules plus an optional pinned
/// model. `model` is advisory in M1/M2 (it doesn't reach the tool provider yet);
/// the load-bearing field is `rules`.
#[derive(Debug, Clone)]
pub struct RoleProfile {
    /// Per-role tool rules. Matched on ENGINE tool names (lowercase `bash`,
    /// `write_file`, `read_file`, `web_search`, …), NOT the Claude-Code labels
    /// (`Bash`/`Write`) the permission gate uses — so bare-name matchers work
    /// directly against `tool.schema().name` with no label-mapping layer.
    pub rules: PermissionRules,
    /// A model to pin for this role. Advisory in M1/M2 — recorded for M4.
    pub model: Option<String>,
}

/// The whole family: the member roster and the role → profile table.
#[derive(Debug, Clone, Default)]
pub struct FamilyConfig {
    members: Vec<MemberAuth>,
    roles: HashMap<String, RoleProfile>,
}

// --- on-disk shape (parsed, then converted to the validated types above) ---

#[derive(Debug, Deserialize)]
struct FamilyConfigRaw {
    #[serde(default)]
    members: Vec<MemberAuth>,
    #[serde(default)]
    roles: HashMap<String, RoleProfileRaw>,
}

#[derive(Debug, Deserialize)]
struct RoleProfileRaw {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    ask: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
    /// `"deny" | "ask" | "allow"`. Absent ⇒ `deny` (fail-safe for a family role —
    /// stricter than [`PermissionRules`]'s own `Ask` default, because an
    /// unlisted tool for a *scoped* member should be denied, not prompted).
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

impl FamilyConfig {
    /// Parse a `family.toml`. Fail-closed: any malformed rule or unknown default
    /// verdict is an `Err`, so the caller serves no family config rather than a
    /// permissive one.
    ///
    /// # Errors
    /// Returns an error naming the first problem (bad TOML, bad matcher, or an
    /// unknown `default` verdict).
    pub fn from_toml(s: &str) -> Result<Self, String> {
        let raw: FamilyConfigRaw = toml::from_str(s).map_err(|e| format!("parsing family TOML: {e}"))?;
        let mut roles = HashMap::with_capacity(raw.roles.len());
        for (name, raw_role) in raw.roles {
            let default = parse_default(raw_role.default.as_deref())?;
            let role_rules = PermissionRules::from_lists(
                raw_role.deny.iter().map(String::as_str),
                raw_role.ask.iter().map(String::as_str),
                raw_role.allow.iter().map(String::as_str),
            )
            .map_err(|e| format!("role '{name}': {e}"))?
            .with_default(default);
            roles.insert(
                name,
                RoleProfile {
                    rules: role_rules,
                    model: raw_role.model,
                },
            );
        }
        Ok(Self { members: raw.members, roles })
    }

    /// The member presenting `token`, by constant-time compare. `None` for an
    /// empty or unrecognized token (fail-closed). Every candidate is compared in
    /// full so a match leaks neither length nor content through timing.
    #[must_use]
    pub fn member_for_token(&self, token: &str) -> Option<&MemberAuth> {
        if token.is_empty() {
            return None;
        }
        self.members.iter().find(|m| ct_eq(m.token.as_bytes(), token.as_bytes()))
    }

    /// The profile for `role`, if the family defines it.
    #[must_use]
    pub fn profile(&self, role: &str) -> Option<&RoleProfile> {
        self.roles.get(role)
    }

    /// Whether `role` may use the tool named `tool` (an ENGINE tool name).
    ///
    /// **Deny-by-default**: an unknown role, or a role whose rules `decide` the
    /// tool as [`Decision::Deny`] (including the fall-through to a `deny` default),
    /// returns `false`. An `Ask`ed tool stays available (the global gate still
    /// prompts) — but a hardened role uses `default = "deny"`, so nothing falls to
    /// `Ask` unless listed.
    #[must_use]
    pub fn tool_allowed(&self, role: &str, tool: &str) -> bool {
        // `None` (unknown role) ⇒ false: deny-by-default.
        self.roles.get(role).is_some_and(|p| p.rules.decide(tool, "") != Decision::Deny)
    }

    /// Number of configured members (for a startup log line).
    #[must_use]
    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    /// Number of configured roles (for a startup log line).
    #[must_use]
    pub fn role_count(&self) -> usize {
        self.roles.len()
    }

    /// True when nothing is configured — treated as "no family".
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty() && self.roles.is_empty()
    }
}

/// `"deny" | "ask" | "allow"` → [`Decision`]; absent ⇒ `Deny` (family fail-safe).
fn parse_default(s: Option<&str>) -> Result<Decision, String> {
    match s.map(str::trim) {
        None | Some("deny") => Ok(Decision::Deny),
        Some("ask") => Ok(Decision::Ask),
        Some("allow") => Ok(Decision::Allow),
        Some(other) => Err(format!("unknown default verdict '{other}' (want deny|ask|allow)")),
    }
}

/// Length-aware constant-time byte comparison — the same shape the daemon's
/// local-token gate uses, duplicated here so the policy crate stays leaf.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    const JR: &str = r#"
        [[members]]
        token = "jr-token-aaaa"
        id = "kid-alex"
        role = "child"
        display_name = "Alex"

        [[members]]
        token = "teen-token-bbbb"
        id = "kid-sam"
        role = "teen"

        [roles.child]
        allow = ["read_file", "list_files", "grep", "recall", "current_datetime"]
        default = "deny"

        [roles.teen]
        allow = ["read_file", "list_files", "grep", "recall", "web_search"]
        default = "deny"
    "#;

    #[test]
    fn from_toml_round_trips_members_and_roles() {
        let cfg = FamilyConfig::from_toml(JR).unwrap();
        assert_eq!(cfg.member_count(), 2);
        assert_eq!(cfg.role_count(), 2);
        assert!(cfg.profile("child").is_some());
        assert!(cfg.profile("teen").is_some());
        assert!(cfg.profile("nobody").is_none());
    }

    #[test]
    fn deny_by_default_holds_for_the_child_role() {
        let cfg = FamilyConfig::from_toml(JR).unwrap();
        // Allowed: exactly the allowlist.
        for t in ["read_file", "list_files", "grep", "recall", "current_datetime"] {
            assert!(cfg.tool_allowed("child", t), "child should keep {t}");
        }
        // Denied: everything else, especially anything that writes, shells, or egresses.
        for t in [
            "bash",
            "write_file",
            "edit_file",
            "web_search",
            "crawl",
            "knowledge_search",
            "th",
            "remember",
            "send_sidekick",
            "create_skill",
            "some_mcp__tool",
        ] {
            assert!(!cfg.tool_allowed("child", t), "child must NOT get {t}");
        }
    }

    #[test]
    fn teen_is_broader_than_child_but_still_scoped() {
        let cfg = FamilyConfig::from_toml(JR).unwrap();
        assert!(cfg.tool_allowed("teen", "web_search"), "teen may search the web");
        assert!(!cfg.tool_allowed("child", "web_search"), "child may not");
        // Neither gets a shell or writes.
        assert!(!cfg.tool_allowed("teen", "bash"));
        assert!(!cfg.tool_allowed("teen", "write_file"));
    }

    #[test]
    fn unknown_role_is_deny_everything() {
        let cfg = FamilyConfig::from_toml(JR).unwrap();
        // A token that mapped to a role missing from the table grants nothing —
        // never the full set.
        for t in ["read_file", "bash", "grep", "web_search"] {
            assert!(!cfg.tool_allowed("bogus", t), "unknown role must deny {t}");
        }
    }

    #[test]
    fn member_lookup_is_exact_and_fail_closed() {
        let cfg = FamilyConfig::from_toml(JR).unwrap();
        assert_eq!(cfg.member_for_token("jr-token-aaaa").unwrap().role, "child");
        assert_eq!(cfg.member_for_token("teen-token-bbbb").unwrap().id, "kid-sam");
        // Wrong, empty, prefix, and over-length tokens all miss.
        assert!(cfg.member_for_token("nope").is_none());
        assert!(cfg.member_for_token("").is_none());
        assert!(cfg.member_for_token("jr-token").is_none(), "prefix must not match");
        assert!(cfg.member_for_token("jr-token-aaaa-extra").is_none(), "longer must not match");
    }

    #[test]
    fn malformed_toml_fails_closed() {
        // Not TOML at all.
        assert!(FamilyConfig::from_toml("{ this is not toml").is_err());
        // Unknown default verdict — must error, NOT silently allow.
        let bad = r#"
            [roles.child]
            allow = ["read_file"]
            default = "yolo"
        "#;
        assert!(FamilyConfig::from_toml(bad).is_err(), "unknown default must be rejected");
        // A malformed matcher string surfaces as an error too.
        let bad_matcher = r#"
            [roles.child]
            allow = ["Bash("]
            default = "deny"
        "#;
        assert!(FamilyConfig::from_toml(bad_matcher).is_err());
    }

    #[test]
    fn empty_config_is_empty() {
        let cfg = FamilyConfig::from_toml("").unwrap();
        assert!(cfg.is_empty());
        // And a totally empty family grants nothing to any role (fail-closed).
        assert!(!cfg.tool_allowed("child", "read_file"));
        assert!(cfg.member_for_token("anything").is_none());
    }

    #[test]
    fn absent_default_denies_unlisted_tools() {
        // A role that omits `default` entirely must still deny unlisted tools.
        let cfg = FamilyConfig::from_toml(
            r#"
            [roles.child]
            allow = ["read_file"]
        "#,
        )
        .unwrap();
        assert!(cfg.tool_allowed("child", "read_file"));
        assert!(!cfg.tool_allowed("child", "bash"), "no default ⇒ deny unlisted");
    }

    #[test]
    fn ct_eq_matches_only_identical_bytes() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(!ct_eq(b"", b"a"));
        assert!(ct_eq(b"", b""));
    }
}
