//! # Agent primitives
//!
//! First-class agent definitions that live above the routing-slot layer.
//! An [`AgentInfo`] bundles a prompt, a routing slot ([`Activity`]), a
//! [`PermissionSet`], and optional overrides into a single named unit that
//! call sites can look up by name instead of hard-coding a prompt and a
//! routing call side by side.
//!
//! This module ships the three *hidden* utility agents (`title`,
//! `compaction`, `summary`), the four *primary* agents (`code`,
//! `plan`, `think`, `review`), and the two *subagents* (`explore`,
//! `general`) that primary agents can dispatch work to via the
//! `dispatch_subagent` tool (see [`dispatch`]).

use std::collections::HashMap;

use async_trait::async_trait;

use crate::providers::{Activity, ModelSlot};
use crate::tool::{ToolCall, ToolHook, ToolResult};

pub mod dispatch;
pub use dispatch::{DispatchResult, DispatchSubagentTool, LlmConfigFactory};

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

    /// Iterate only the subagents — agents with
    /// [`AgentKind::Subagent`]. The `dispatch_subagent` tool uses
    /// this to enumerate which agent names it is willing to spawn;
    /// anything else (primary, hidden) is not dispatchable from a
    /// parent agent's tool surface.
    pub fn subagents(&self) -> impl Iterator<Item = &AgentInfo> {
        self.agents.values().filter(|a| a.kind == AgentKind::Subagent)
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
pub const CODE_PROMPT: &str = include_str!("prompts/code.txt");
const PLAN_PROMPT: &str = include_str!("prompts/plan.txt");
const THINK_PROMPT: &str = include_str!("prompts/think.txt");
const REVIEW_PROMPT: &str = include_str!("prompts/review.txt");
const EXPLORE_PROMPT: &str = include_str!("prompts/explore.txt");
const GENERAL_PROMPT: &str = include_str!("prompts/general.txt");

/// Read-only tool set used by `plan`, `think`, and `review`. Anything
/// not in this list is denied. The allowlist is more defensible than
/// a deny-list: when a new mutating tool gets registered (edit_file,
/// write_file, apply_patch, bash, bg_run, http_fetch …) the reasoning
/// agents stay read-only by default instead of inheriting power they
/// weren't designed for.
fn read_only_tools() -> Vec<String> {
    vec![
        "read_file".into(),
        "list_files".into(),
        "grep".into(),
        "glob".into(),
        "lsp".into(),
        "project_inspect".into(),
    ]
}

/// Tools that the `plan` agent is allowed to call on top of
/// [`read_only_tools`]. Plan is still read-only w.r.t. the workspace
/// but may inspect structure more broadly — same set as think/review
/// today, kept as its own helper so future tweaks don't accidentally
/// leak edit capability.
fn plan_tools() -> Vec<String> {
    read_only_tools()
}

/// Tools the `explore` subagent is allowed to call. Read-only
/// investigation set: grep/glob/ls/read/find. Strictly no edit, no
/// bash, no write — `explore` is a scout that returns a summary, not
/// an agent that fixes anything. Kept as its own allowlist (rather
/// than re-using [`read_only_tools`]) so the subagent's surface can
/// evolve separately from the reasoning agents' surface.
fn explore_tools() -> Vec<String> {
    vec![
        "grep".into(),
        "glob".into(),
        "ls".into(),
        "list_files".into(),
        "read_file".into(),
        "find".into(),
    ]
}

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
        // ─── Primary agents ────────────────────────────────────
        //
        // `code` is the default `th` experience: full tool access,
        // Coding-slot routing. Its prompt is the same text that used
        // to live inline in `coding_workflow.rs` as
        // `CODING_SYSTEM_PROMPT` — factoring it here means the
        // coding workflow now looks up the prompt + slot by name
        // instead of hard-coding both, and users can override it
        // from a single place in a future pearl.
        AgentInfo {
            name: "code".into(),
            kind: AgentKind::Primary,
            slot: Activity::Coding,
            model_override: None,
            prompt: CODE_PROMPT.trim().to_string(),
            permissions: PermissionSet::default(),
            steps: None,
            hidden: false,
        },
        // `plan` decomposes without modifying. Allow-list of
        // read-only inspection tools; edit/write/patch/bash are
        // denied so even a confused model can't ship code under the
        // plan agent.
        AgentInfo {
            name: "plan".into(),
            kind: AgentKind::Primary,
            slot: Activity::Planning,
            model_override: None,
            prompt: PLAN_PROMPT.trim().to_string(),
            permissions: PermissionSet {
                allow_tools: plan_tools(),
                deny_tools: vec!["edit_file".into(), "write_file".into(), "apply_patch".into()],
            },
            steps: None,
            hidden: false,
        },
        // `think` is pure reasoning — no bash, no mutation.
        AgentInfo {
            name: "think".into(),
            kind: AgentKind::Primary,
            slot: Activity::Thinking,
            model_override: None,
            prompt: THINK_PROMPT.trim().to_string(),
            permissions: PermissionSet {
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
        // `review` is adversarial critique — read-only, same shape
        // as think but routed through the Reviewing slot.
        AgentInfo {
            name: "review".into(),
            kind: AgentKind::Primary,
            slot: Activity::Reviewing,
            model_override: None,
            prompt: REVIEW_PROMPT.trim().to_string(),
            permissions: PermissionSet {
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
        // ─── Subagents ─────────────────────────────────────────
        //
        // Subagents are dispatched by primary agents through the
        // `dispatch_subagent` tool (see [`dispatch`]). Each call
        // spawns a fresh `Agent` with its own context, its own
        // filtered [`ToolRegistry`], and its own [`PermissionHook`]
        // — the parent only ever sees the final summary string the
        // subagent returns, never the subagent's transcript. This
        // is the context-window win: expensive investigation stays
        // out of the parent's conversation.
        AgentInfo {
            name: "explore".into(),
            kind: AgentKind::Subagent,
            slot: Activity::Coding,
            model_override: None,
            prompt: EXPLORE_PROMPT.trim().to_string(),
            permissions: PermissionSet {
                allow_tools: explore_tools(),
                // Belt-and-suspenders: even if someone adds a write
                // tool to the explore allowlist by mistake, these
                // stay denied outright.
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
        // `general` is the fallback subagent: full tool access,
        // self-contained multi-step tasks. Use this when a primary
        // agent wants to hand off an entire sub-problem (not just a
        // lookup) without polluting its own context.
        AgentInfo {
            name: "general".into(),
            kind: AgentKind::Subagent,
            slot: Activity::Coding,
            model_override: None,
            prompt: GENERAL_PROMPT.trim().to_string(),
            permissions: PermissionSet::default(),
            steps: None,
            hidden: false,
        },
    ]
}

// ─── Permission enforcement hook ──────────────────────────────
//
// `PermissionHook` sits on the [`ToolRegistry`] hook chain and
// blocks any tool call that the active agent's [`PermissionSet`]
// disallows. Permission enforcement happens BEFORE the tool runs,
// so a `plan`-mode agent that tries to call `edit_file` never
// touches disk — the registry returns an error result with an
// explicit "agent '{name}' is not permitted to call '{tool}'"
// message that the LLM sees and can reason about.

/// Tool-dispatch hook that enforces an [`AgentInfo`]'s
/// [`PermissionSet`]. Install this on a [`ToolRegistry`] BEFORE any
/// tool call happens — the hook chain runs in registration order,
/// so an agent permission check should be first to fail fast on
/// denied calls and avoid wasting downstream hooks.
///
/// The hook only reads the agent at construction time; the agent
/// itself is immutable for the lifetime of a run. If a caller wants
/// to swap agents mid-session they should rebuild the registry.
#[derive(Debug, Clone)]
pub struct PermissionHook {
    agent_name: String,
    permissions: PermissionSet,
}

impl PermissionHook {
    /// Build a hook that enforces `agent`'s [`PermissionSet`].
    pub fn new(agent: &AgentInfo) -> Self {
        Self {
            agent_name: agent.name.clone(),
            permissions: agent.permissions.clone(),
        }
    }

    /// Build a hook directly from a name + [`PermissionSet`]. Useful
    /// in tests and when the caller doesn't have an [`AgentInfo`]
    /// handy.
    pub fn from_parts(agent_name: impl Into<String>, permissions: PermissionSet) -> Self {
        Self {
            agent_name: agent_name.into(),
            permissions,
        }
    }

    /// Render the block message for a denied tool call. Kept as its
    /// own function so the wording is the same in `pre_call` and in
    /// tests — the wording is part of the tool's contract with the
    /// LLM (the model reads it as a tool result) and with the human
    /// reader of logs.
    pub fn block_message(agent_name: &str, tool: &str) -> String {
        format!("agent '{agent_name}' is not permitted to call '{tool}'")
    }
}

#[async_trait]
impl ToolHook for PermissionHook {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        if self.permissions.allows(&call.name) {
            Ok(())
        } else {
            Err(anyhow::anyhow!(Self::block_message(&self.agent_name, &call.name)))
        }
    }

    async fn post_call(&self, _call: &ToolCall, _result: &ToolResult) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registers_three_hidden_agents() {
        let registry = AgentRegistry::builtin();
        for name in ["title", "compaction", "summary"] {
            let agent = registry.get(name).unwrap_or_else(|| panic!("{name} not registered"));
            assert!(agent.hidden, "{name} should be hidden");
            assert_eq!(agent.kind, AgentKind::Hidden);
        }
    }

    #[test]
    fn builtin_registers_four_primary_agents() {
        let registry = AgentRegistry::builtin();
        for name in ["code", "plan", "think", "review"] {
            let agent = registry.get(name).unwrap_or_else(|| panic!("{name} not registered"));
            assert!(!agent.hidden, "{name} should not be hidden");
            assert_eq!(agent.kind, AgentKind::Primary);
        }
    }

    #[test]
    fn primary_agents_route_to_expected_slots() {
        let registry = AgentRegistry::builtin();
        assert_eq!(registry.get("code").unwrap().slot, Activity::Coding);
        assert_eq!(registry.get("plan").unwrap().slot, Activity::Planning);
        assert_eq!(registry.get("think").unwrap().slot, Activity::Thinking);
        assert_eq!(registry.get("review").unwrap().slot, Activity::Reviewing);
    }

    #[test]
    fn code_agent_has_full_tool_access() {
        let registry = AgentRegistry::builtin();
        let code = registry.get("code").unwrap();
        // Default PermissionSet is empty allow + empty deny = anything goes.
        assert!(code.permissions.allows("read_file"));
        assert!(code.permissions.allows("write_file"));
        assert!(code.permissions.allows("edit_file"));
        assert!(code.permissions.allows("apply_patch"));
        assert!(code.permissions.allows("bash"));
        assert!(!code.permissions.is_deny_all());
    }

    #[test]
    fn plan_agent_allows_read_and_blocks_writes() {
        let registry = AgentRegistry::builtin();
        let plan = registry.get("plan").unwrap();
        assert!(plan.permissions.allows("read_file"));
        assert!(plan.permissions.allows("list_files"));
        assert!(plan.permissions.allows("grep"));
        assert!(!plan.permissions.allows("edit_file"), "plan must not edit");
        assert!(!plan.permissions.allows("write_file"), "plan must not write");
        assert!(!plan.permissions.allows("apply_patch"), "plan must not patch");
    }

    #[test]
    fn think_agent_is_fully_read_only() {
        let registry = AgentRegistry::builtin();
        let think = registry.get("think").unwrap();
        assert!(think.permissions.allows("read_file"));
        assert!(!think.permissions.allows("bash"), "think must not shell");
        assert!(!think.permissions.allows("edit_file"));
        assert!(!think.permissions.allows("write_file"));
        assert!(!think.permissions.allows("http_fetch"));
    }

    #[test]
    fn review_agent_is_fully_read_only() {
        let registry = AgentRegistry::builtin();
        let review = registry.get("review").unwrap();
        assert!(review.permissions.allows("read_file"));
        assert!(review.permissions.allows("grep"));
        assert!(!review.permissions.allows("edit_file"));
        assert!(!review.permissions.allows("bash"));
    }

    #[test]
    fn primary_agent_prompts_are_loaded_from_files() {
        let registry = AgentRegistry::builtin();

        let code = registry.get("code").unwrap();
        assert!(code.prompt.contains("coding agent"), "code prompt: {}", code.prompt);
        assert!(code.prompt.contains("## Test Results"));

        let plan = registry.get("plan").unwrap();
        assert!(plan.prompt.contains("planning agent"));
        assert!(plan.prompt.contains("do not modify"));

        let think = registry.get("think").unwrap();
        assert!(think.prompt.contains("reasoning"));
        assert!(think.prompt.contains("do not modify code"));

        let review = registry.get("review").unwrap();
        assert!(review.prompt.to_lowercase().contains("review"));
        assert!(review.prompt.contains("Blockers"));
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
        let visible: Vec<_> = registry.list_visible().map(|a| a.name.clone()).collect();
        // Four primary agents + two subagents are visible; title/
        // compaction/summary are hidden.
        assert_eq!(visible.len(), 6, "expected 6 visible agents, got {visible:?}");
        for name in ["code", "plan", "think", "review", "explore", "general"] {
            assert!(visible.iter().any(|v| v == name), "{name} missing from visible list");
        }
    }

    #[test]
    fn register_overwrites_existing_agent() {
        let mut registry = AgentRegistry::builtin();
        let before = registry.len();
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
        assert_eq!(registry.len(), before, "overwrite should not add a new entry");
    }

    #[test]
    fn permission_hook_blocks_denied_tool() {
        use crate::tool::ToolCall;
        let registry = AgentRegistry::builtin();
        let plan = registry.get("plan").unwrap();
        let hook = PermissionHook::new(plan);

        let call = ToolCall {
            id: "call-1".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({}),
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = runtime.block_on(hook.pre_call(&call));
        let err = result.expect_err("plan must not be permitted to edit_file");
        let msg = err.to_string();
        assert!(msg.contains("plan"), "error should name the agent: {msg}");
        assert!(msg.contains("edit_file"), "error should name the tool: {msg}");
        assert!(msg.contains("not permitted"), "error should say 'not permitted': {msg}");
    }

    #[test]
    fn permission_hook_allows_permitted_tool() {
        use crate::tool::ToolCall;
        let registry = AgentRegistry::builtin();
        let plan = registry.get("plan").unwrap();
        let hook = PermissionHook::new(plan);

        let call = ToolCall {
            id: "call-2".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(hook.pre_call(&call)).expect("plan may read_file");
    }

    #[test]
    fn permission_hook_allows_everything_for_code_agent() {
        use crate::tool::ToolCall;
        let registry = AgentRegistry::builtin();
        let code = registry.get("code").unwrap();
        let hook = PermissionHook::new(code);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for tool in ["read_file", "write_file", "edit_file", "apply_patch", "bash", "grep"] {
            let call = ToolCall {
                id: format!("call-{tool}"),
                name: tool.into(),
                arguments: serde_json::json!({}),
            };
            runtime
                .block_on(hook.pre_call(&call))
                .unwrap_or_else(|e| panic!("code should allow {tool}: {e}"));
        }
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

    // ─── Subagent registration tests ─────────────────────────

    #[test]
    fn builtin_registers_two_subagents() {
        let registry = AgentRegistry::builtin();
        for name in ["explore", "general"] {
            let agent = registry.get(name).unwrap_or_else(|| panic!("{name} not registered"));
            assert_eq!(agent.kind, AgentKind::Subagent, "{name} must be a Subagent");
            assert!(!agent.hidden, "{name} should not be hidden");
        }
    }

    #[test]
    fn subagents_helper_returns_only_subagents() {
        let registry = AgentRegistry::builtin();
        let names: Vec<String> = registry.subagents().map(|a| a.name.clone()).collect();
        assert_eq!(names.len(), 2, "expected 2 subagents, got {names:?}");
        for expected in ["explore", "general"] {
            assert!(names.iter().any(|n| n == expected), "{expected} missing from subagents()");
        }
        // Verify no primaries / hidden agents slip through.
        for agent in registry.subagents() {
            assert_eq!(agent.kind, AgentKind::Subagent, "{} leaked into subagents()", agent.name);
        }
    }

    #[test]
    fn explore_subagent_is_read_only() {
        let registry = AgentRegistry::builtin();
        let explore = registry.get("explore").unwrap();
        // Explicitly allowed read-only tools pass.
        assert!(explore.permissions.allows("read_file"));
        assert!(explore.permissions.allows("grep"));
        assert!(explore.permissions.allows("glob"));
        assert!(explore.permissions.allows("ls"));
        assert!(explore.permissions.allows("list_files"));
        assert!(explore.permissions.allows("find"));
        // Mutating / shell tools are denied.
        assert!(!explore.permissions.allows("edit_file"), "explore must not edit");
        assert!(!explore.permissions.allows("write_file"), "explore must not write");
        assert!(!explore.permissions.allows("apply_patch"), "explore must not patch");
        assert!(!explore.permissions.allows("bash"), "explore must not shell");
        assert!(!explore.permissions.allows("bg_run"), "explore must not background-run");
        assert!(!explore.permissions.allows("http_fetch"), "explore must not hit the network");
        // Tools outside the allowlist are denied too (allowlist is
        // the defense, not the deny list).
        assert!(!explore.permissions.allows("some_future_write_tool"));
    }

    #[test]
    fn general_subagent_has_full_tool_access() {
        let registry = AgentRegistry::builtin();
        let general = registry.get("general").unwrap();
        // Empty allow + empty deny = anything is permitted.
        assert!(general.permissions.allows("read_file"));
        assert!(general.permissions.allows("write_file"));
        assert!(general.permissions.allows("edit_file"));
        assert!(general.permissions.allows("apply_patch"));
        assert!(general.permissions.allows("bash"));
        assert!(general.permissions.allows("http_fetch"));
        assert!(!general.permissions.is_deny_all());
    }

    #[test]
    fn subagents_route_to_coding_slot() {
        let registry = AgentRegistry::builtin();
        assert_eq!(registry.get("explore").unwrap().slot, Activity::Coding);
        assert_eq!(registry.get("general").unwrap().slot, Activity::Coding);
    }

    #[test]
    fn subagent_prompts_loaded_from_files() {
        let registry = AgentRegistry::builtin();

        let explore = registry.get("explore").unwrap();
        assert!(explore.prompt.to_lowercase().contains("scout"), "explore prompt: {}", explore.prompt);
        assert!(explore.prompt.contains("DO NOT modify"), "explore prompt must forbid modification");
        assert!(!explore.prompt.ends_with('\n'), "prompt should be trimmed");

        let general = registry.get("general").unwrap();
        assert!(
            general.prompt.to_lowercase().contains("subagent"),
            "general prompt should mention subagent: {}",
            general.prompt
        );
        assert!(general.prompt.contains("isolated"), "general prompt must note isolation");
    }
}
