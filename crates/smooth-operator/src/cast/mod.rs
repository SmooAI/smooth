//! # Cast — operator roles
//!
//! First-class operator role definitions that live above the
//! routing-slot layer. An [`OperatorRole`] bundles a prompt, a
//! routing slot ([`Activity`]), a [`Clearance`], and optional
//! overrides into a single named unit that call sites can look up by
//! name instead of hard-coding a prompt and a routing call side by
//! side.
//!
//! This module ships the three *shadow* utility roles (`tagger`,
//! `presser`, `recapper`), the four *lead* roles (`fixer`,
//! `mapper`, `oracle`, `heckler`), and the two *sidekick* roles
//! (`scout`, `runner`) that lead roles can dispatch work to via the
//! `send_sidekick` tool (see [`dispatch`]).

use std::collections::HashMap;

use async_trait::async_trait;

use crate::providers::{Activity, ModelSlot};
use crate::tool::{ToolCall, ToolHook, ToolResult};

pub mod dispatch;
pub use dispatch::{DispatchResult, DispatchSubagentTool, LlmConfigFactory};

/// How an operator role is surfaced to users.
///
/// - [`RoleKind::Lead`] — top-level roles the user can choose via
///   `--agent` or slash command (e.g. `fixer`, `mapper`).
/// - [`RoleKind::Sidekick`] — dispatchable from other roles via a
///   `task`-style tool (e.g. `scout`, `runner`).
/// - [`RoleKind::Shadow`] — internal utility roles the runtime calls
///   on the user's behalf (e.g. session auto-naming, transcript
///   compaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoleKind {
    Lead,
    Sidekick,
    Shadow,
}

/// Tool allow/deny list for an operator role.
///
/// An empty `allow_tools` means "any tool is allowed unless denied".
/// `deny_tools` always wins over `allow_tools`. A non-empty `allow_tools`
/// paired with an empty `deny_tools` pins the role to exactly that set.
///
/// Shadow utility roles (tagger/presser/recapper) use a `deny_tools`
/// entry of `"*"` to opt out of tool use entirely — they're pure
/// text-in/text-out calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Clearance {
    pub allow_tools: Vec<String>,
    pub deny_tools: Vec<String>,
}

impl Clearance {
    /// A clearance that denies all tools. Used by shadow utility
    /// roles (tagger, presser, recapper) — these are stateless text
    /// transformations, not tool-using roles.
    pub fn deny_all() -> Self {
        Self {
            allow_tools: Vec::new(),
            deny_tools: vec!["*".to_string()],
        }
    }

    /// Returns true if this clearance denies every tool.
    pub fn is_deny_all(&self) -> bool {
        self.deny_tools.iter().any(|t| t == "*")
    }

    /// Returns true if the named tool is permitted under this
    /// clearance. `deny` wins over `allow`; a deny entry of `"*"`
    /// denies everything; an empty `allow` means "no allowlist
    /// restriction".
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

/// A first-class operator role definition.
///
/// Roles are looked up by `name` from a [`Cast`]. Call sites that
/// previously hard-coded a prompt + `llm_config_for(Activity::X)`
/// pair can now resolve that pair from a named role, which lets
/// users customize prompts and routing per role in one place.
#[derive(Debug, Clone)]
pub struct OperatorRole {
    /// Unique role name (e.g. `"tagger"`, `"fixer"`, `"scout"`).
    pub name: String,
    pub kind: RoleKind,
    /// Which routing slot this role defaults to when no
    /// `model_override` is set.
    pub slot: Activity,
    /// Optional per-role routing override. If set, callers should use
    /// this slot instead of resolving `slot` through the registry.
    pub model_override: Option<ModelSlot>,
    /// System prompt — typically loaded at compile time from a `.txt`
    /// file via `include_str!`.
    pub prompt: String,
    pub permissions: Clearance,
    /// Optional internal-iteration cap. `None` means "use the caller's
    /// default". Shadow utility roles are single-shot and leave this
    /// as `None`.
    pub steps: Option<u32>,
    /// Hidden from user-facing role lists. Shadow roles are always
    /// invoked by the runtime itself, never selected directly by a user.
    pub hidden: bool,
}

/// Registry of known [`OperatorRole`] records, keyed by name.
#[derive(Debug, Clone, Default)]
pub struct Cast {
    roles: HashMap<String, OperatorRole>,
}

impl Cast {
    /// Build a cast populated with the built-in roles. Includes the
    /// three shadow utility roles (`tagger`, `presser`, `recapper`),
    /// the four lead roles (`fixer`, `mapper`, `oracle`, `heckler`),
    /// and the two sidekick roles (`scout`, `runner`).
    pub fn builtin() -> Self {
        let mut cast = Self::default();
        for role in builtin_roles() {
            cast.register(role);
        }
        cast
    }

    /// Register a role. Overwrites any existing entry with the same
    /// name.
    pub fn register(&mut self, role: OperatorRole) {
        self.roles.insert(role.name.clone(), role);
    }

    /// Look up a role by name.
    pub fn get(&self, name: &str) -> Option<&OperatorRole> {
        self.roles.get(name)
    }

    /// Iterate every registered role. Order is unspecified.
    pub fn list(&self) -> impl Iterator<Item = &OperatorRole> {
        self.roles.values()
    }

    /// Iterate only the user-visible (non-hidden) roles.
    pub fn list_visible(&self) -> impl Iterator<Item = &OperatorRole> {
        self.roles.values().filter(|a| !a.hidden)
    }

    /// Iterate only the sidekicks — roles with [`RoleKind::Sidekick`].
    /// The `send_sidekick` tool uses this to enumerate which role
    /// names it is willing to spawn; anything else (lead, shadow) is
    /// not dispatchable from a parent role's tool surface.
    pub fn sidekicks(&self) -> impl Iterator<Item = &OperatorRole> {
        self.roles.values().filter(|a| a.kind == RoleKind::Sidekick)
    }

    /// Number of registered roles.
    pub fn len(&self) -> usize {
        self.roles.len()
    }

    /// True when no roles are registered.
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }
}

const TAGGER_PROMPT: &str = include_str!("prompts/tagger.txt");
const PRESSER_PROMPT: &str = include_str!("prompts/presser.txt");
const RECAPPER_PROMPT: &str = include_str!("prompts/recapper.txt");
pub const FIXER_PROMPT: &str = include_str!("prompts/fixer.txt");
const MAPPER_PROMPT: &str = include_str!("prompts/mapper.txt");
const ORACLE_PROMPT: &str = include_str!("prompts/oracle.txt");
const HECKLER_PROMPT: &str = include_str!("prompts/heckler.txt");
const SCOUT_PROMPT: &str = include_str!("prompts/scout.txt");
const RUNNER_PROMPT: &str = include_str!("prompts/runner.txt");

/// Read-only tool set used by `mapper`, `oracle`, and `heckler`. Anything
/// not in this list is denied. The allowlist is more defensible than
/// a deny-list: when a new mutating tool gets registered (edit_file,
/// write_file, apply_patch, bash, bg_run, http_fetch …) the reasoning
/// roles stay read-only by default instead of inheriting power they
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

/// Tools that the `mapper` role is allowed to call on top of
/// [`read_only_tools`]. Mapper is still read-only w.r.t. the workspace
/// but may inspect structure more broadly — same set as oracle/heckler
/// today, kept as its own helper so future tweaks don't accidentally
/// leak edit capability.
fn mapper_tools() -> Vec<String> {
    read_only_tools()
}

/// Tools the `scout` sidekick is allowed to call. Read-only
/// investigation set: grep/glob/ls/read/find. Strictly no edit, no
/// bash, no write — `scout` is a sidekick that returns a summary, not
/// a role that fixes anything. Kept as its own allowlist (rather
/// than re-using [`read_only_tools`]) so the sidekick's surface can
/// evolve separately from the reasoning roles' surface.
fn scout_tools() -> Vec<String> {
    vec![
        "grep".into(),
        "glob".into(),
        "ls".into(),
        "list_files".into(),
        "read_file".into(),
        "find".into(),
    ]
}

fn builtin_roles() -> Vec<OperatorRole> {
    vec![
        OperatorRole {
            name: "tagger".into(),
            kind: RoleKind::Shadow,
            slot: Activity::Fast,
            model_override: None,
            prompt: TAGGER_PROMPT.trim().to_string(),
            permissions: Clearance::deny_all(),
            steps: None,
            hidden: true,
        },
        OperatorRole {
            name: "presser".into(),
            kind: RoleKind::Shadow,
            slot: Activity::Summarize,
            model_override: None,
            prompt: PRESSER_PROMPT.trim().to_string(),
            permissions: Clearance::deny_all(),
            steps: None,
            hidden: true,
        },
        OperatorRole {
            name: "recapper".into(),
            kind: RoleKind::Shadow,
            slot: Activity::Summarize,
            model_override: None,
            prompt: RECAPPER_PROMPT.trim().to_string(),
            permissions: Clearance::deny_all(),
            steps: None,
            hidden: true,
        },
        // ─── Lead roles ────────────────────────────────────────
        //
        // `fixer` is the default `th` experience: full tool access,
        // Coding-slot routing. Its prompt is the same text that used
        // to live inline in `coding_workflow.rs` as
        // `CODING_SYSTEM_PROMPT` — factoring it here means the
        // coding workflow now looks up the prompt + slot by name
        // instead of hard-coding both, and users can override it
        // from a single place in a future pearl.
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
        // `mapper` decomposes without modifying. Allow-list of
        // read-only inspection tools; edit/write/patch/bash are
        // denied so even a confused model can't ship code under the
        // mapper role.
        OperatorRole {
            name: "mapper".into(),
            kind: RoleKind::Lead,
            slot: Activity::Planning,
            model_override: None,
            prompt: MAPPER_PROMPT.trim().to_string(),
            permissions: Clearance {
                allow_tools: mapper_tools(),
                deny_tools: vec!["edit_file".into(), "write_file".into(), "apply_patch".into()],
            },
            steps: None,
            hidden: false,
        },
        // `oracle` is pure reasoning — no bash, no mutation.
        OperatorRole {
            name: "oracle".into(),
            kind: RoleKind::Lead,
            slot: Activity::Thinking,
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
        // `heckler` is adversarial critique — read-only, same shape
        // as oracle but routed through the Reviewing slot.
        OperatorRole {
            name: "heckler".into(),
            kind: RoleKind::Lead,
            slot: Activity::Reviewing,
            model_override: None,
            prompt: HECKLER_PROMPT.trim().to_string(),
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
        // ─── Sidekicks ─────────────────────────────────────────
        //
        // Sidekicks are dispatched by lead roles through the
        // `send_sidekick` tool (see [`dispatch`]). Each call
        // spawns a fresh `Agent` with its own context, its own
        // filtered [`ToolRegistry`], and its own [`PermissionHook`]
        // — the parent only ever sees the final summary string the
        // sidekick returns, never the sidekick's transcript. This
        // is the context-window win: expensive investigation stays
        // out of the parent's conversation.
        OperatorRole {
            name: "scout".into(),
            kind: RoleKind::Sidekick,
            slot: Activity::Coding,
            model_override: None,
            prompt: SCOUT_PROMPT.trim().to_string(),
            permissions: Clearance {
                allow_tools: scout_tools(),
                // Belt-and-suspenders: even if someone adds a write
                // tool to the scout allowlist by mistake, these
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
        // `runner` is the fallback sidekick: full tool access,
        // self-contained multi-step tasks. Use this when a lead
        // role wants to hand off an entire sub-problem (not just a
        // lookup) without polluting its own context.
        OperatorRole {
            name: "runner".into(),
            kind: RoleKind::Sidekick,
            slot: Activity::Coding,
            model_override: None,
            prompt: RUNNER_PROMPT.trim().to_string(),
            permissions: Clearance::default(),
            steps: None,
            hidden: false,
        },
    ]
}

// ─── Permission enforcement hook ──────────────────────────────
//
// `PermissionHook` sits on the [`ToolRegistry`] hook chain and
// blocks any tool call that the active role's [`Clearance`]
// disallows. Permission enforcement happens BEFORE the tool runs,
// so a `mapper`-mode role that tries to call `edit_file` never
// touches disk — the registry returns an error result with an
// explicit "agent '{name}' is not permitted to call '{tool}'"
// message that the LLM sees and can reason about.

/// Tool-dispatch hook that enforces an [`OperatorRole`]'s
/// [`Clearance`]. Install this on a [`ToolRegistry`] BEFORE any
/// tool call happens — the hook chain runs in registration order,
/// so a role permission check should be first to fail fast on
/// denied calls and avoid wasting downstream hooks.
///
/// The hook only reads the role at construction time; the role
/// itself is immutable for the lifetime of a run. If a caller wants
/// to swap roles mid-session they should rebuild the registry.
#[derive(Debug, Clone)]
pub struct PermissionHook {
    agent_name: String,
    permissions: Clearance,
}

impl PermissionHook {
    /// Build a hook that enforces `role`'s [`Clearance`].
    pub fn new(role: &OperatorRole) -> Self {
        Self {
            agent_name: role.name.clone(),
            permissions: role.permissions.clone(),
        }
    }

    /// Build a hook directly from a name + [`Clearance`]. Useful
    /// in tests and when the caller doesn't have an [`OperatorRole`]
    /// handy.
    pub fn from_parts(agent_name: impl Into<String>, permissions: Clearance) -> Self {
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
    fn builtin_registers_three_shadow_roles() {
        let cast = Cast::builtin();
        for name in ["tagger", "presser", "recapper"] {
            let role = cast.get(name).unwrap_or_else(|| panic!("{name} not registered"));
            assert!(role.hidden, "{name} should be hidden");
            assert_eq!(role.kind, RoleKind::Shadow);
        }
    }

    #[test]
    fn builtin_registers_four_lead_roles() {
        let cast = Cast::builtin();
        for name in ["fixer", "mapper", "oracle", "heckler"] {
            let role = cast.get(name).unwrap_or_else(|| panic!("{name} not registered"));
            assert!(!role.hidden, "{name} should not be hidden");
            assert_eq!(role.kind, RoleKind::Lead);
        }
    }

    #[test]
    fn lead_roles_route_to_expected_slots() {
        let cast = Cast::builtin();
        assert_eq!(cast.get("fixer").unwrap().slot, Activity::Coding);
        assert_eq!(cast.get("mapper").unwrap().slot, Activity::Planning);
        assert_eq!(cast.get("oracle").unwrap().slot, Activity::Thinking);
        assert_eq!(cast.get("heckler").unwrap().slot, Activity::Reviewing);
    }

    #[test]
    fn fixer_role_has_full_tool_access() {
        let cast = Cast::builtin();
        let fixer = cast.get("fixer").unwrap();
        // Default Clearance is empty allow + empty deny = anything goes.
        assert!(fixer.permissions.allows("read_file"));
        assert!(fixer.permissions.allows("write_file"));
        assert!(fixer.permissions.allows("edit_file"));
        assert!(fixer.permissions.allows("apply_patch"));
        assert!(fixer.permissions.allows("bash"));
        assert!(!fixer.permissions.is_deny_all());
    }

    #[test]
    fn mapper_role_allows_read_and_blocks_writes() {
        let cast = Cast::builtin();
        let mapper = cast.get("mapper").unwrap();
        assert!(mapper.permissions.allows("read_file"));
        assert!(mapper.permissions.allows("list_files"));
        assert!(mapper.permissions.allows("grep"));
        assert!(!mapper.permissions.allows("edit_file"), "mapper must not edit");
        assert!(!mapper.permissions.allows("write_file"), "mapper must not write");
        assert!(!mapper.permissions.allows("apply_patch"), "mapper must not patch");
    }

    #[test]
    fn oracle_role_is_fully_read_only() {
        let cast = Cast::builtin();
        let oracle = cast.get("oracle").unwrap();
        assert!(oracle.permissions.allows("read_file"));
        assert!(!oracle.permissions.allows("bash"), "oracle must not shell");
        assert!(!oracle.permissions.allows("edit_file"));
        assert!(!oracle.permissions.allows("write_file"));
        assert!(!oracle.permissions.allows("http_fetch"));
    }

    #[test]
    fn heckler_role_is_fully_read_only() {
        let cast = Cast::builtin();
        let heckler = cast.get("heckler").unwrap();
        assert!(heckler.permissions.allows("read_file"));
        assert!(heckler.permissions.allows("grep"));
        assert!(!heckler.permissions.allows("edit_file"));
        assert!(!heckler.permissions.allows("bash"));
    }

    #[test]
    fn lead_role_prompts_are_loaded_from_files() {
        let cast = Cast::builtin();

        let fixer = cast.get("fixer").unwrap();
        assert!(fixer.prompt.contains("coding agent"), "fixer prompt: {}", fixer.prompt);
        assert!(fixer.prompt.contains("## Test Results"));

        let mapper = cast.get("mapper").unwrap();
        assert!(mapper.prompt.contains("planning agent"));
        assert!(mapper.prompt.contains("do not modify"));

        let oracle = cast.get("oracle").unwrap();
        assert!(oracle.prompt.contains("reasoning"));
        assert!(oracle.prompt.contains("do not modify code"));

        let heckler = cast.get("heckler").unwrap();
        assert!(heckler.prompt.to_lowercase().contains("review"));
        assert!(heckler.prompt.contains("Blockers"));
    }

    #[test]
    fn shadow_roles_deny_all_tools() {
        let cast = Cast::builtin();
        for name in ["tagger", "presser", "recapper"] {
            let role = cast.get(name).unwrap();
            assert!(role.permissions.is_deny_all(), "{name} should deny all tools");
            assert!(!role.permissions.allows("read"), "{name} allowed read");
            assert!(!role.permissions.allows("bash"), "{name} allowed bash");
        }
    }

    #[test]
    fn shadow_roles_route_to_expected_slots() {
        let cast = Cast::builtin();
        assert_eq!(cast.get("tagger").unwrap().slot, Activity::Fast);
        assert_eq!(cast.get("presser").unwrap().slot, Activity::Summarize);
        assert_eq!(cast.get("recapper").unwrap().slot, Activity::Summarize);
    }

    #[test]
    fn shadow_role_prompts_are_loaded_from_files() {
        let cast = Cast::builtin();

        let tagger = cast.get("tagger").unwrap();
        assert!(!tagger.prompt.is_empty(), "tagger prompt empty");
        assert!(tagger.prompt.contains("3-to-6 word"), "tagger prompt content mismatch: {}", tagger.prompt);
        assert!(!tagger.prompt.ends_with('\n'), "prompt should be trimmed");

        let presser = cast.get("presser").unwrap();
        assert!(presser.prompt.contains("Compress"), "presser prompt: {}", presser.prompt);
        assert!(presser.prompt.contains("verbatim"), "presser prompt should demand verbatim preservation");

        let recapper = cast.get("recapper").unwrap();
        assert!(recapper.prompt.contains("Summarize"), "recapper prompt: {}", recapper.prompt);
        assert!(recapper.prompt.contains("what's next"), "recapper prompt should cover the next-steps axis");
    }

    #[test]
    fn lookup_by_name_returns_none_for_unknown() {
        let cast = Cast::builtin();
        assert!(cast.get("nonexistent").is_none());
        assert!(cast.get("").is_none());
    }

    #[test]
    fn list_visible_excludes_shadow_utility_roles() {
        let cast = Cast::builtin();
        let visible: Vec<_> = cast.list_visible().map(|a| a.name.clone()).collect();
        // Four lead roles + two sidekicks are visible; tagger/
        // presser/recapper are hidden.
        assert_eq!(visible.len(), 6, "expected 6 visible roles, got {visible:?}");
        for name in ["fixer", "mapper", "oracle", "heckler", "scout", "runner"] {
            assert!(visible.iter().any(|v| v == name), "{name} missing from visible list");
        }
    }

    #[test]
    fn register_overwrites_existing_role() {
        let mut cast = Cast::builtin();
        let before = cast.len();
        cast.register(OperatorRole {
            name: "tagger".into(),
            kind: RoleKind::Lead,
            slot: Activity::Coding,
            model_override: None,
            prompt: "override".into(),
            permissions: Clearance::default(),
            steps: Some(5),
            hidden: false,
        });
        let tagger = cast.get("tagger").unwrap();
        assert_eq!(tagger.prompt, "override");
        assert_eq!(tagger.kind, RoleKind::Lead);
        assert!(!tagger.hidden);
        assert_eq!(tagger.steps, Some(5));
        assert_eq!(cast.len(), before, "overwrite should not add a new entry");
    }

    #[test]
    fn permission_hook_blocks_denied_tool() {
        use crate::tool::ToolCall;
        let cast = Cast::builtin();
        let mapper = cast.get("mapper").unwrap();
        let hook = PermissionHook::new(mapper);

        let call = ToolCall {
            id: "call-1".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({}),
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = runtime.block_on(hook.pre_call(&call));
        let err = result.expect_err("mapper must not be permitted to edit_file");
        let msg = err.to_string();
        assert!(msg.contains("mapper"), "error should name the role: {msg}");
        assert!(msg.contains("edit_file"), "error should name the tool: {msg}");
        assert!(msg.contains("not permitted"), "error should say 'not permitted': {msg}");
    }

    #[test]
    fn permission_hook_allows_permitted_tool() {
        use crate::tool::ToolCall;
        let cast = Cast::builtin();
        let mapper = cast.get("mapper").unwrap();
        let hook = PermissionHook::new(mapper);

        let call = ToolCall {
            id: "call-2".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(hook.pre_call(&call)).expect("mapper may read_file");
    }

    #[test]
    fn permission_hook_allows_everything_for_fixer_role() {
        use crate::tool::ToolCall;
        let cast = Cast::builtin();
        let fixer = cast.get("fixer").unwrap();
        let hook = PermissionHook::new(fixer);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for tool in ["read_file", "write_file", "edit_file", "apply_patch", "bash", "grep"] {
            let call = ToolCall {
                id: format!("call-{tool}"),
                name: tool.into(),
                arguments: serde_json::json!({}),
            };
            runtime
                .block_on(hook.pre_call(&call))
                .unwrap_or_else(|e| panic!("fixer should allow {tool}: {e}"));
        }
    }

    #[test]
    fn clearance_deny_star_blocks_everything() {
        let perms = Clearance::deny_all();
        assert!(perms.is_deny_all());
        for tool in ["read", "write", "bash", "anything"] {
            assert!(!perms.allows(tool), "{tool} should be denied by deny-all");
        }
    }

    #[test]
    fn clearance_deny_wins_over_allow() {
        let perms = Clearance {
            allow_tools: vec!["read".into(), "write".into()],
            deny_tools: vec!["write".into()],
        };
        assert!(perms.allows("read"));
        assert!(!perms.allows("write"));
        assert!(!perms.allows("bash"), "tools outside allowlist are denied");
    }

    #[test]
    fn clearance_empty_allow_means_no_restriction() {
        let perms = Clearance::default();
        assert!(perms.allows("read"));
        assert!(perms.allows("bash"));
        assert!(!perms.is_deny_all());
    }

    // ─── Sidekick registration tests ─────────────────────────

    #[test]
    fn builtin_registers_two_sidekicks() {
        let cast = Cast::builtin();
        for name in ["scout", "runner"] {
            let role = cast.get(name).unwrap_or_else(|| panic!("{name} not registered"));
            assert_eq!(role.kind, RoleKind::Sidekick, "{name} must be a Sidekick");
            assert!(!role.hidden, "{name} should not be hidden");
        }
    }

    #[test]
    fn sidekicks_helper_returns_only_sidekicks() {
        let cast = Cast::builtin();
        let names: Vec<String> = cast.sidekicks().map(|a| a.name.clone()).collect();
        assert_eq!(names.len(), 2, "expected 2 sidekicks, got {names:?}");
        for expected in ["scout", "runner"] {
            assert!(names.iter().any(|n| n == expected), "{expected} missing from sidekicks()");
        }
        // Verify no leads / shadow roles slip through.
        for role in cast.sidekicks() {
            assert_eq!(role.kind, RoleKind::Sidekick, "{} leaked into sidekicks()", role.name);
        }
    }

    #[test]
    fn scout_sidekick_is_read_only() {
        let cast = Cast::builtin();
        let scout = cast.get("scout").unwrap();
        // Explicitly allowed read-only tools pass.
        assert!(scout.permissions.allows("read_file"));
        assert!(scout.permissions.allows("grep"));
        assert!(scout.permissions.allows("glob"));
        assert!(scout.permissions.allows("ls"));
        assert!(scout.permissions.allows("list_files"));
        assert!(scout.permissions.allows("find"));
        // Mutating / shell tools are denied.
        assert!(!scout.permissions.allows("edit_file"), "scout must not edit");
        assert!(!scout.permissions.allows("write_file"), "scout must not write");
        assert!(!scout.permissions.allows("apply_patch"), "scout must not patch");
        assert!(!scout.permissions.allows("bash"), "scout must not shell");
        assert!(!scout.permissions.allows("bg_run"), "scout must not background-run");
        assert!(!scout.permissions.allows("http_fetch"), "scout must not hit the network");
        // Tools outside the allowlist are denied too (allowlist is
        // the defense, not the deny list).
        assert!(!scout.permissions.allows("some_future_write_tool"));
    }

    #[test]
    fn runner_sidekick_has_full_tool_access() {
        let cast = Cast::builtin();
        let runner = cast.get("runner").unwrap();
        // Empty allow + empty deny = anything is permitted.
        assert!(runner.permissions.allows("read_file"));
        assert!(runner.permissions.allows("write_file"));
        assert!(runner.permissions.allows("edit_file"));
        assert!(runner.permissions.allows("apply_patch"));
        assert!(runner.permissions.allows("bash"));
        assert!(runner.permissions.allows("http_fetch"));
        assert!(!runner.permissions.is_deny_all());
    }

    #[test]
    fn sidekicks_route_to_coding_slot() {
        let cast = Cast::builtin();
        assert_eq!(cast.get("scout").unwrap().slot, Activity::Coding);
        assert_eq!(cast.get("runner").unwrap().slot, Activity::Coding);
    }

    #[test]
    fn sidekick_prompts_loaded_from_files() {
        let cast = Cast::builtin();

        let scout = cast.get("scout").unwrap();
        assert!(scout.prompt.to_lowercase().contains("scout"), "scout prompt: {}", scout.prompt);
        assert!(scout.prompt.contains("DO NOT modify"), "scout prompt must forbid modification");
        assert!(!scout.prompt.ends_with('\n'), "prompt should be trimmed");

        let runner = cast.get("runner").unwrap();
        assert!(
            runner.prompt.to_lowercase().contains("subagent"),
            "runner prompt should mention subagent: {}",
            runner.prompt
        );
        assert!(runner.prompt.contains("isolated"), "runner prompt must note isolation");
    }
}
