//! Gate 1 — the deterministic deny/ask/allow permission rule engine (EPIC
//! th-c89c2a, th-515a13).
//!
//! This is the **intent layer** of the security model: a Claude-Code-style rule
//! set that classifies a tool call as [`Decision::Deny`], [`Decision::Ask`]
//! (human confirmation — the operator's `write_confirmation_required` HITL), or
//! [`Decision::Allow`]. It is deterministic (no LLM) and **fail-safe**: the
//! default when nothing matches is `Ask`, and precedence is **deny > ask >
//! allow** so a deny can never be overridden by a looser allow.
//!
//! The load-bearing boundary remains the **kernel sandbox + egress proxy** (a
//! reasoning agent can talk its way around an intent rule, but not the kernel);
//! Gate 1 expresses policy + UX on top of that. This module is the pure
//! model + matcher; wiring it as a `ToolHook` on the operator's registry is a
//! following slice.
//!
//! ## Rule syntax
//!
//! A matcher is `Tool` (bare — matches any invocation of that tool) or
//! `Tool(pattern)` (a glob on the tool's primary argument). Following Claude
//! Code, a `:` in the pattern is a word boundary, so `Bash(rm:*)` ≡ `Bash(rm *)`
//! — it matches a bash command that starts with `rm `. Matching is
//! case-sensitive on the tool name.

use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};

/// The verdict for a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Block the call outright.
    Deny,
    /// Require human confirmation before proceeding (the HITL "ask").
    Ask,
    /// Permit the call without prompting.
    Allow,
}

/// A single rule matcher: a tool name plus an optional glob on its primary arg.
#[derive(Debug, Clone)]
pub struct Matcher {
    tool: String,
    /// `None` = matches any argument (a bare `Tool` rule).
    arg: Option<GlobMatcher>,
    /// The original source string, for diagnostics + round-tripping.
    source: String,
}

impl Matcher {
    /// Parse a matcher like `Bash(rm:*)`, `Read(//etc/**)`, or bare `Bash`.
    ///
    /// # Errors
    /// Returns an error if the argument glob is malformed or the tool name is
    /// empty.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if let Some(open) = s.find('(') {
            if !s.ends_with(')') {
                return Err(format!("matcher {s:?} has '(' without a closing ')'"));
            }
            let tool = s[..open].trim();
            if tool.is_empty() {
                return Err(format!("matcher {s:?} has an empty tool name"));
            }
            let raw = &s[open + 1..s.len() - 1];
            // Claude-Code: a `:` in the pattern is a word boundary (`rm:*` = `rm *`).
            let pattern = raw.replace(':', " ");
            let glob = Glob::new(&pattern).map_err(|e| format!("invalid pattern in {s:?}: {e}"))?;
            Ok(Self {
                tool: tool.to_string(),
                arg: Some(glob.compile_matcher()),
                source: s.to_string(),
            })
        } else {
            if s.is_empty() {
                return Err("empty matcher".to_string());
            }
            Ok(Self {
                tool: s.to_string(),
                arg: None,
                source: s.to_string(),
            })
        }
    }

    /// Whether this matcher applies to a call of `tool` with primary argument
    /// `arg`. A bare matcher (no arg glob) matches any argument.
    #[must_use]
    pub fn matches(&self, tool: &str, arg: &str) -> bool {
        if self.tool != tool {
            return false;
        }
        self.arg.as_ref().map_or(true, |g| g.is_match(arg))
    }

    /// The original rule string.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// The Gate-1 rule set: ordered deny/ask/allow matcher lists + a default verdict
/// for calls that match nothing.
#[derive(Debug, Clone)]
pub struct PermissionRules {
    deny: Vec<Matcher>,
    ask: Vec<Matcher>,
    allow: Vec<Matcher>,
    default: Decision,
}

impl Default for PermissionRules {
    fn default() -> Self {
        // Fail-safe: with no rules, every call asks.
        Self {
            deny: Vec::new(),
            ask: Vec::new(),
            allow: Vec::new(),
            default: Decision::Ask,
        }
    }
}

impl PermissionRules {
    /// Build a rule set from deny/ask/allow matcher-string lists. The default
    /// verdict (no match) is `Ask` — fail-safe.
    ///
    /// # Errors
    /// Returns an error naming the first malformed matcher.
    pub fn from_lists<'a>(
        deny: impl IntoIterator<Item = &'a str>,
        ask: impl IntoIterator<Item = &'a str>,
        allow: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, String> {
        Ok(Self {
            deny: parse_all(deny)?,
            ask: parse_all(ask)?,
            allow: parse_all(allow)?,
            default: Decision::Ask,
        })
    }

    /// Override the no-match default verdict.
    #[must_use]
    pub fn with_default(mut self, default: Decision) -> Self {
        self.default = default;
        self
    }

    /// Decide the verdict for a tool call. Precedence is **deny > ask > allow**
    /// (a deny is never overridden); within a tier, first match wins; no match
    /// falls through to the default.
    #[must_use]
    pub fn decide(&self, tool: &str, arg: &str) -> Decision {
        if self.deny.iter().any(|m| m.matches(tool, arg)) {
            return Decision::Deny;
        }
        if self.ask.iter().any(|m| m.matches(tool, arg)) {
            return Decision::Ask;
        }
        if self.allow.iter().any(|m| m.matches(tool, arg)) {
            return Decision::Allow;
        }
        self.default
    }
}

fn parse_all<'a>(items: impl IntoIterator<Item = &'a str>) -> Result<Vec<Matcher>, String> {
    items.into_iter().map(Matcher::parse).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn bare_matcher_matches_any_arg() {
        let m = Matcher::parse("Bash").unwrap();
        assert!(m.matches("Bash", "anything at all"));
        assert!(m.matches("Bash", ""));
        assert!(!m.matches("Read", "x"), "different tool never matches");
    }

    #[test]
    fn glob_matcher_with_colon_word_boundary() {
        // `rm:*` == `rm *` — matches a command starting with "rm ".
        let m = Matcher::parse("Bash(rm:*)").unwrap();
        assert!(m.matches("Bash", "rm -rf build"));
        assert!(!m.matches("Bash", "rmdir foo"), "rm-space boundary, not a prefix");
        assert!(!m.matches("Bash", "ls"));
    }

    #[test]
    fn path_glob_matcher() {
        let m = Matcher::parse("Read(/etc/**)").unwrap();
        assert!(m.matches("Read", "/etc/passwd"));
        assert!(m.matches("Read", "/etc/ssh/sshd_config"));
        assert!(!m.matches("Read", "/home/me/notes.txt"));
    }

    #[test]
    fn precedence_is_deny_over_ask_over_allow() {
        // Same call matches all three tiers; deny must win.
        let rules = PermissionRules::from_lists(["Bash(rm:*)"], ["Bash"], ["Bash"]).unwrap();
        assert_eq!(rules.decide("Bash", "rm -rf /"), Decision::Deny);
        // A bash call that isn't rm: no deny, ask tier matches first.
        assert_eq!(rules.decide("Bash", "ls -la"), Decision::Ask);
    }

    #[test]
    fn allow_when_only_allow_matches() {
        let rules = PermissionRules::from_lists([] as [&str; 0], [] as [&str; 0], ["Read", "Grep"]).unwrap();
        assert_eq!(rules.decide("Read", "/home/me/x"), Decision::Allow);
        assert_eq!(rules.decide("Grep", "foo"), Decision::Allow);
    }

    #[test]
    fn default_is_ask_fail_safe() {
        let rules = PermissionRules::from_lists([] as [&str; 0], [] as [&str; 0], ["Read"]).unwrap();
        assert_eq!(rules.decide("Bash", "anything"), Decision::Ask, "unmatched call asks");
        assert_eq!(PermissionRules::default().decide("Bash", "x"), Decision::Ask);
    }

    #[test]
    fn default_override() {
        let rules = PermissionRules::default().with_default(Decision::Deny);
        assert_eq!(rules.decide("Whatever", "x"), Decision::Deny);
    }

    #[test]
    fn malformed_matchers_are_rejected() {
        assert!(Matcher::parse("Bash(").is_err(), "unclosed paren");
        assert!(Matcher::parse("(rm)").is_err(), "empty tool");
        assert!(Matcher::parse("").is_err(), "empty matcher");
        assert!(Matcher::parse("Bash([)").is_err(), "malformed glob");
    }

    #[test]
    fn decision_serde_round_trips() {
        for d in [Decision::Deny, Decision::Ask, Decision::Allow] {
            let json = serde_json::to_string(&d).unwrap();
            assert_eq!(serde_json::from_str::<Decision>(&json).unwrap(), d);
        }
        assert_eq!(serde_json::to_string(&Decision::Deny).unwrap(), "\"deny\"");
    }
}
