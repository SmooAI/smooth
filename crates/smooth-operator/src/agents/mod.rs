//! # Agent primitives
//!
//! First-class agent definitions that live above the routing-slot layer.
//! An [`AgentInfo`] bundles a prompt, a routing slot ([`Activity`]), a
//! [`PermissionSet`], and optional overrides into a single named unit that
//! call sites can look up by name instead of hard-coding a prompt and a
//! routing call side by side.
//!
//! This module intentionally ships only the three *hidden* utility agents
//! (`title`, `compaction`, `summary`). Primary agents (`code`, `plan`,
//! `think`, `review`) and subagents (`explore`, `general`) are added by
//! later pearls in the agents-primitive epic.

use std::collections::HashMap;

use crate::providers::{Activity, ModelSlot};

/// How an agent is surfaced to users.
///
/// - [`AgentKind::Primary`] — top-level agents the user can choose via
///   `--agent` or slash command (e.g. `code`, `plan`).
/// - [`AgentKind::Subagent`] — dispatchable from other agents via a
///   `task`-style tool (e.g. `explore`, `general`).
/// - [`AgentKind::Hidden`] — internal utility agents the runtime calls on
///   the user's behalf (e.g. session auto-naming, transcript compaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Primary,
    Subagent,
    Hidden,
}

/// Tool allow/deny list for an agent.
///
/// An empty `allow_tools` means "any tool is allowed unless denied".
/// `deny_tools` always wins over `allow_tools`. A non-empty `allow_tools`
/// paired with an empty `deny_tools` pins the agent to exactly that set.
///
/// Hidden utility agents (title/compaction/summary) use a `deny_tools`
/// entry of `"*"` to opt out of tool use entirely — they're pure
/// text-in/text-out calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionSet {
    pub allow_tools: Vec<String>,
    pub deny_tools: Vec<String>,
}

impl PermissionSet {
    /// A permission set that denies all tools. Used by hidden utility
    /// agents (title, compaction, summary) — these are stateless text
    /// transformations, not tool-using agents.
    pub fn deny_all() -> Self {
        Self {
            allow_tools: Vec::new(),
            deny_tools: vec!["*".to_string()],
        }
    }

    /// Returns true if this permission set denies every tool.
    pub fn is_deny_all(&self) -> bool {
        self.deny_tools.iter().any(|t| t == "*")
    }

    /// Returns true if the named tool is permitted under this permission
    /// set. `deny` wins over `allow`; a deny entry of `"*"` denies
    /// everything; an empty `allow` means "no allowlist restriction".
    pub fn allows(&self, tool: &str) -> bool {
        if self.deny_tools.iter().any(|t| t == "*" || t == tool) {
            return false;
        }
        if self.allow_tools.is_empty() {
            return true;
        }
        self.allow_tools.iter().any(|t| t == tool)
    }
}

/// A first-class agent definition.
///
/// Agents are looked up by `name` from an [`AgentRegistry`]. Call sites
/// that previously hard-coded a prompt + `llm_config_for(Activity::X)`
/// pair can now resolve that pair from a named agent, which lets users
/// customize prompts and routing per agent in one place.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    /// Unique agent name (e.g. `"title"`, `"code"`, `"explore"`).
    pub name: String,
    pub kind: AgentKind,
    /// Which routing slot this agent defaults to when no `model_override`
    /// is set.
    pub slot: Activity,
    /// Optional per-agent routing override. If set, callers should use
    /// this slot instead of resolving `slot` through the registry.
    pub model_override: Option<ModelSlot>,
    /// System prompt — typically loaded at compile time from a `.txt`
    /// file via `include_str!`.
    pub prompt: String,
    pub permissions: PermissionSet,
    /// Optional internal-iteration cap. `None` means "use the caller's
    /// default". Hidden utility agents are single-shot and leave this
    /// as `None`.
    pub steps: Option<u32>,
    /// Hidden from user-facing agent lists. Hidden agents are always
    /// invoked by the runtime itself, never selected directly by a user.
    pub hidden: bool,
}

/// Registry of known [`AgentInfo`] records, keyed by name.
#[derive(Debug, Clone, Default)]
pub struct AgentRegistry {
    agents: HashMap<String, AgentInfo>,
}

impl AgentRegistry {
    /// Build a registry populated with the built-in agents. For now that's
    /// just the three hidden utility agents (`title`, `compaction`,
    /// `summary`). Primary agents and subagents are added in later pearls.
    pub fn builtin() -> Self {
        let mut registry = Self::default();
        for agent in builtin_agents() {
            registry.register(agent);
        }
        registry
    }

    /// Register an agent. Overwrites any existing entry with the same
    /// name.
    pub fn register(&mut self, agent: AgentInfo) {
        self.agents.insert(agent.name.clone(), agent);
    }

    /// Look up an agent by name.
    pub fn get(&self, name: &str) -> Option<&AgentInfo> {
        self.agents.get(name)
    }

    /// Iterate every registered agent. Order is unspecified.
    pub fn list(&self) -> impl Iterator<Item = &AgentInfo> {
        self.agents.values()
    }

    /// Iterate only the user-visible (non-hidden) agents.
    pub fn list_visible(&self) -> impl Iterator<Item = &AgentInfo> {
        self.agents.values().filter(|a| !a.hidden)
    }

    /// Number of registered agents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// True when no agents are registered.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }
}

const TITLE_PROMPT: &str = include_str!("prompts/title.txt");
const COMPACTION_PROMPT: &str = include_str!("prompts/compaction.txt");
const SUMMARY_PROMPT: &str = include_str!("prompts/summary.txt");

fn builtin_agents() -> Vec<AgentInfo> {
    vec![
        AgentInfo {
            name: "title".into(),
            kind: AgentKind::Hidden,
            slot: Activity::Fast,
            model_override: None,
            prompt: TITLE_PROMPT.trim().to_string(),
            permissions: PermissionSet::deny_all(),
            steps: None,
            hidden: true,
        },
        AgentInfo {
            name: "compaction".into(),
            kind: AgentKind::Hidden,
            slot: Activity::Summarize,
            model_override: None,
            prompt: COMPACTION_PROMPT.trim().to_string(),
            permissions: PermissionSet::deny_all(),
            steps: None,
            hidden: true,
        },
        AgentInfo {
            name: "summary".into(),
            kind: AgentKind::Hidden,
            slot: Activity::Summarize,
            model_override: None,
            prompt: SUMMARY_PROMPT.trim().to_string(),
            permissions: PermissionSet::deny_all(),
            steps: None,
            hidden: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registers_three_hidden_agents() {
        let registry = AgentRegistry::builtin();
        assert_eq!(registry.len(), 3);
        for name in ["title", "compaction", "summary"] {
            let agent = registry.get(name).unwrap_or_else(|| panic!("{name} not registered"));
            assert!(agent.hidden, "{name} should be hidden");
            assert_eq!(agent.kind, AgentKind::Hidden);
        }
    }

    #[test]
    fn hidden_agents_deny_all_tools() {
        let registry = AgentRegistry::builtin();
        for name in ["title", "compaction", "summary"] {
            let agent = registry.get(name).unwrap();
            assert!(agent.permissions.is_deny_all(), "{name} should deny all tools");
            assert!(!agent.permissions.allows("read"), "{name} allowed read");
            assert!(!agent.permissions.allows("bash"), "{name} allowed bash");
        }
    }

    #[test]
    fn hidden_agents_route_to_expected_slots() {
        let registry = AgentRegistry::builtin();
        assert_eq!(registry.get("title").unwrap().slot, Activity::Fast);
        assert_eq!(registry.get("compaction").unwrap().slot, Activity::Summarize);
        assert_eq!(registry.get("summary").unwrap().slot, Activity::Summarize);
    }

    #[test]
    fn hidden_agent_prompts_are_loaded_from_files() {
        let registry = AgentRegistry::builtin();

        let title = registry.get("title").unwrap();
        assert!(!title.prompt.is_empty(), "title prompt empty");
        assert!(title.prompt.contains("3-to-6 word"), "title prompt content mismatch: {}", title.prompt);
        assert!(!title.prompt.ends_with('\n'), "prompt should be trimmed");

        let compaction = registry.get("compaction").unwrap();
        assert!(compaction.prompt.contains("Compress"), "compaction prompt: {}", compaction.prompt);
        assert!(compaction.prompt.contains("verbatim"), "compaction prompt should demand verbatim preservation");

        let summary = registry.get("summary").unwrap();
        assert!(summary.prompt.contains("Summarize"), "summary prompt: {}", summary.prompt);
        assert!(summary.prompt.contains("what's next"), "summary prompt should cover the next-steps axis");
    }

    #[test]
    fn lookup_by_name_returns_none_for_unknown() {
        let registry = AgentRegistry::builtin();
        assert!(registry.get("nonexistent").is_none());
        assert!(registry.get("").is_none());
    }

    #[test]
    fn list_visible_excludes_hidden_utility_agents() {
        let registry = AgentRegistry::builtin();
        assert_eq!(registry.list_visible().count(), 0, "all built-ins are hidden today");
        assert_eq!(registry.list().count(), 3);
    }

    #[test]
    fn register_overwrites_existing_agent() {
        let mut registry = AgentRegistry::builtin();
        registry.register(AgentInfo {
            name: "title".into(),
            kind: AgentKind::Primary,
            slot: Activity::Coding,
            model_override: None,
            prompt: "override".into(),
            permissions: PermissionSet::default(),
            steps: Some(5),
            hidden: false,
        });
        let title = registry.get("title").unwrap();
        assert_eq!(title.prompt, "override");
        assert_eq!(title.kind, AgentKind::Primary);
        assert!(!title.hidden);
        assert_eq!(title.steps, Some(5));
        assert_eq!(registry.len(), 3, "overwrite should not add a new entry");
    }

    #[test]
    fn permission_set_deny_star_blocks_everything() {
        let perms = PermissionSet::deny_all();
        assert!(perms.is_deny_all());
        for tool in ["read", "write", "bash", "anything"] {
            assert!(!perms.allows(tool), "{tool} should be denied by deny-all");
        }
    }

    #[test]
    fn permission_set_deny_wins_over_allow() {
        let perms = PermissionSet {
            allow_tools: vec!["read".into(), "write".into()],
            deny_tools: vec!["write".into()],
        };
        assert!(perms.allows("read"));
        assert!(!perms.allows("write"));
        assert!(!perms.allows("bash"), "tools outside allowlist are denied");
    }

    #[test]
    fn permission_set_empty_allow_means_no_restriction() {
        let perms = PermissionSet::default();
        assert!(perms.allows("read"));
        assert!(perms.allows("bash"));
        assert!(!perms.is_deny_all());
    }
}
