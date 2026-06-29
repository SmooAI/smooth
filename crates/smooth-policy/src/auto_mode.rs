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

/// How a matcher constrains a tool call's primary argument.
#[derive(Debug, Clone)]
enum ArgPattern {
    /// A bare `Tool` rule — matches any argument.
    Any,
    /// A `cmd:*` / `cmd *` "command with any args" rule: the argument's
    /// whitespace tokens must *start with* these tokens. This is the
    /// security-correct shape for shell commands — `Bash(rm:*)` matches `rm` and
    /// `rm -rf /` but not `rmdir` (token boundary, not a substring prefix).
    Prefix(Vec<String>),
    /// A general glob over the whole argument (paths, etc.).
    Glob(GlobMatcher),
}

/// A single rule matcher: a tool name plus a constraint on its primary arg.
#[derive(Debug, Clone)]
pub struct Matcher {
    tool: String,
    arg: ArgPattern,
    /// The original source string, for diagnostics + round-tripping.
    source: String,
}

impl Matcher {
    /// Parse a matcher like `Bash(rm:*)`, `Read(/etc/**)`, or bare `Bash`.
    ///
    /// `Tool(cmd:*)` / `Tool(cmd *)` parse to a token-prefix match (the
    /// security-correct shell shape); other patterns are globs.
    ///
    /// # Errors
    /// Returns an error if the argument glob is malformed or the tool name is
    /// empty.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let Some(open) = s.find('(') else {
            if s.is_empty() {
                return Err("empty matcher".to_string());
            }
            return Ok(Self {
                tool: s.to_string(),
                arg: ArgPattern::Any,
                source: s.to_string(),
            });
        };
        if !s.ends_with(')') {
            return Err(format!("matcher {s:?} has '(' without a closing ')'"));
        }
        let tool = s[..open].trim();
        if tool.is_empty() {
            return Err(format!("matcher {s:?} has an empty tool name"));
        }
        let raw = &s[open + 1..s.len() - 1];
        let arg = Self::parse_arg(raw, s)?;
        Ok(Self {
            tool: tool.to_string(),
            arg,
            source: s.to_string(),
        })
    }

    fn parse_arg(raw: &str, full: &str) -> Result<ArgPattern, String> {
        // Claude-Code: a `:` is a word boundary (`rm:*` ≡ `rm *`).
        let pattern = raw.replace(':', " ");
        // A "command with any args" pattern → token-prefix match.
        if let Some(prefix) = pattern.strip_suffix(" *").or_else(|| pattern.strip_suffix('*').filter(|p| p.ends_with(' '))) {
            let toks: Vec<String> = prefix.split_whitespace().map(str::to_string).collect();
            if !toks.is_empty() && !prefix.contains(['*', '?', '[']) {
                return Ok(ArgPattern::Prefix(toks));
            }
        }
        let glob = Glob::new(&pattern).map_err(|e| format!("invalid pattern in {full:?}: {e}"))?;
        Ok(ArgPattern::Glob(glob.compile_matcher()))
    }

    /// Whether this matcher applies to a call of `tool` with primary argument
    /// `arg`. A bare matcher matches any argument.
    #[must_use]
    pub fn matches(&self, tool: &str, arg: &str) -> bool {
        if self.tool != tool {
            return false;
        }
        match &self.arg {
            ArgPattern::Any => true,
            ArgPattern::Prefix(toks) => {
                let arg_toks: Vec<&str> = arg.split_whitespace().collect();
                arg_toks.len() >= toks.len() && toks.iter().zip(&arg_toks).all(|(want, got)| want == got)
            }
            ArgPattern::Glob(g) => g.is_match(arg),
        }
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

    /// Parse a rule set from TOML:
    ///
    /// ```toml
    /// deny  = ["Bash(rm:*)", "Write(/etc/**)"]
    /// ask   = ["Bash(git push:*)"]
    /// allow = ["Read", "Grep", "Bash(ls:*)"]
    /// default = "ask"            # optional: deny | ask | allow
    /// ```
    ///
    /// # Errors
    /// Returns an error if the TOML is malformed or any matcher is invalid.
    pub fn from_toml(s: &str) -> Result<Self, String> {
        let cfg: PermissionConfig = toml::from_str(s).map_err(|e| format!("parsing permission TOML: {e}"))?;
        let mut rules = Self::from_lists(
            cfg.deny.iter().map(String::as_str),
            cfg.ask.iter().map(String::as_str),
            cfg.allow.iter().map(String::as_str),
        )?;
        if let Some(d) = cfg.default {
            rules = rules.with_default(d);
        }
        Ok(rules)
    }
}

/// The on-disk shape of a permission rule set (`~/.smooth/permissions.toml`).
#[derive(Debug, Default, Deserialize)]
struct PermissionConfig {
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default)]
    ask: Vec<String>,
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    default: Option<Decision>,
}

impl PermissionRules {
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

    /// Decide the verdict for a **bash command**, accounting for compound
    /// commands. The command is split on `&& || ; | |& &` and newlines (after
    /// stripping wrappers like `timeout`/`nice`), and **every** subcommand is
    /// decided independently as a `Bash` call; the **strictest** verdict wins
    /// (any `Deny` → `Deny`; else any `Ask` → `Ask`; else `Allow`). So a rule
    /// allowing `Bash(ls:*)` cannot be tricked by `ls && rm -rf /` — the `rm`
    /// subcommand is judged on its own. An empty command falls to the default.
    #[must_use]
    pub fn decide_bash(&self, command: &str) -> Decision {
        let subs = split_bash_command(command);
        if subs.is_empty() {
            return self.default;
        }
        let mut verdict = Decision::Allow;
        for sub in subs {
            match self.decide("Bash", &sub) {
                Decision::Deny => return Decision::Deny,
                Decision::Ask => verdict = Decision::Ask,
                Decision::Allow => {}
            }
        }
        verdict
    }
}

/// Wrappers that pass their trailing command through unchanged — stripped so the
/// real program is what gets matched. Deliberately excludes env/run wrappers
/// (`direnv exec`, `npx`, `docker exec`, …) whose payload should match on its own.
const TRANSPARENT_WRAPPERS: &[&str] = &["timeout", "nice", "nohup", "stdbuf", "time", "ionice"];

/// Strip leading transparent wrappers (+ their flags/args) so the matched
/// argument is the real command. Best-effort: drops the wrapper token, any
/// leading `-flags`, and (for `timeout`) a leading duration.
fn strip_wrappers(sub: &str) -> String {
    let mut toks: Vec<&str> = sub.split_whitespace().collect();
    while let Some(&first) = toks.first() {
        if !TRANSPARENT_WRAPPERS.contains(&first) {
            break;
        }
        toks.remove(0);
        while toks.first().is_some_and(|t| t.starts_with('-')) {
            toks.remove(0);
        }
        // `timeout <duration> cmd …` / `nice <prio>` — drop a leading numeric arg.
        if matches!(first, "timeout" | "nice" | "ionice") && toks.first().is_some_and(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit())) {
            toks.remove(0);
        }
    }
    toks.join(" ")
}

/// Split a (possibly compound) bash command into its constituent subcommands,
/// with transparent wrappers stripped. Splits on `&& || ; | |& &` and newlines.
/// Note: this is a deliberately quote-naive split — over-splitting only makes the
/// engine *stricter* (more pieces checked, fail-safe toward Ask/Deny). Quote-aware
/// splitting is a refinement.
fn split_bash_command(cmd: &str) -> Vec<String> {
    let normalized = cmd.replace("&&", "\n").replace("||", "\n").replace("|&", "\n");
    normalized
        .split(['\n', ';', '|', '&'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(strip_wrappers)
        .filter(|s| !s.is_empty())
        .collect()
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
    fn prefix_matcher_is_token_anchored() {
        // `rm:*` = "rm with any (or no) args" — token boundary, not a substring.
        let m = Matcher::parse("Bash(rm:*)").unwrap();
        assert!(m.matches("Bash", "rm"), "bare command matches");
        assert!(m.matches("Bash", "rm -rf build"));
        assert!(!m.matches("Bash", "rmdir foo"), "rmdir is a different program");
        assert!(!m.matches("Bash", "ls"));
    }

    #[test]
    fn multi_token_prefix_matcher() {
        let m = Matcher::parse("Bash(git push:*)").unwrap();
        assert!(m.matches("Bash", "git push"));
        assert!(m.matches("Bash", "git push origin main"));
        assert!(!m.matches("Bash", "git pull"), "different subcommand");
        assert!(!m.matches("Bash", "git pushy"), "token boundary");
    }

    #[test]
    fn compound_command_every_subcommand_must_clear() {
        // allow ls; deny rm. `ls && rm -rf /` must be denied — the rm subcommand
        // is judged on its own, not hidden behind the allowed ls.
        let rules = PermissionRules::from_lists(["Bash(rm:*)"], [] as [&str; 0], ["Bash(ls:*)", "Bash(grep:*)"]).unwrap();
        assert_eq!(rules.decide_bash("ls -la"), Decision::Allow);
        assert_eq!(rules.decide_bash("ls && rm -rf /"), Decision::Deny, "deny subcommand wins");
        assert_eq!(rules.decide_bash("ls | grep foo"), Decision::Allow, "both piped sides allowed");
        assert_eq!(rules.decide_bash("ls ; curl evil.test"), Decision::Ask, "curl unmatched → default Ask");
        assert_eq!(rules.decide_bash("ls || rm x"), Decision::Deny);
    }

    #[test]
    fn wrappers_are_stripped_before_matching() {
        let rules = PermissionRules::from_lists(["Bash(rm:*)"], [] as [&str; 0], ["Bash(ls:*)"]).unwrap();
        // `timeout 5 rm -rf /` strips to `rm -rf /` → caught by the deny rule.
        assert_eq!(rules.decide_bash("timeout 5 rm -rf /"), Decision::Deny);
        assert_eq!(rules.decide_bash("nice -n 10 ls"), Decision::Allow);
        assert_eq!(rules.decide_bash("nohup ls -la"), Decision::Allow);
    }

    #[test]
    fn empty_bash_command_uses_default() {
        let rules = PermissionRules::from_lists([] as [&str; 0], [] as [&str; 0], ["Bash(ls:*)"]).unwrap();
        assert_eq!(rules.decide_bash("   "), Decision::Ask, "empty → fail-safe default");
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
    fn from_toml_parses_rules_and_default() {
        let rules = PermissionRules::from_toml(
            r#"
            deny  = ["Bash(rm:*)"]
            ask   = ["Bash(git push:*)"]
            allow = ["Read", "Bash(ls:*)"]
            default = "deny"
            "#,
        )
        .unwrap();
        assert_eq!(rules.decide_bash("rm -rf /"), Decision::Deny);
        assert_eq!(rules.decide_bash("git push origin"), Decision::Ask);
        assert_eq!(rules.decide_bash("ls -la"), Decision::Allow);
        assert_eq!(rules.decide("Read", "/x"), Decision::Allow);
        assert_eq!(rules.decide("Whatever", "x"), Decision::Deny, "default applied");
    }

    #[test]
    fn from_toml_empty_is_all_ask() {
        let rules = PermissionRules::from_toml("").unwrap();
        assert_eq!(rules.decide("Bash", "ls"), Decision::Ask);
    }

    #[test]
    fn from_toml_rejects_bad_matcher() {
        assert!(PermissionRules::from_toml(r#"deny = ["Bash("]"#).is_err());
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
