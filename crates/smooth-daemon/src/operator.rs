//! The local deployment flavor — embed smooth-operator's `LocalServer`
//! in-process (EPIC th-c89c2a).
//!
//! Instead of the daemon's bespoke `/ws`, the daemon hosts the **operator's
//! local flavor**: the canonical schema-driven WS protocol, so the official
//! widget and the polyglot SDK clients work natively. Lean build (no cloud
//! adapters — in-memory storage + backplane).
//!
//! **Auth:** the local flavor enables the operator's **strict-auth** mode, so a
//! `/ws` connection with a missing/invalid token is **rejected** (HTTP 401),
//! not degraded to anonymous. So the [`SmooOrgVerifier`](crate::org_auth::SmooOrgVerifier)
//! genuinely gates connections — a stray local process or tailnet peer can't
//! drive the agent — and stamps each connection with the operator's real Smoo
//! org (th-0c63cc).
//! (Default operator behavior is still lenient/anonymous for the embeddable
//! widget's public flow; the local flavor opts into strict.)
//!
//! This is additive: it runs alongside the bespoke `serve_persistent` path
//! while the embed is validated; the bespoke surface retires once parity lands.
//!
//! # Configuration (env)
//!
//! - `SMOOTH_LOCAL_TOKEN` — the auth token (else auto-generated at
//!   `~/.smooth/operator-token`).
//! - `SMOOTH_WORKSPACE` — the dir the sandboxed fs/shell tools are confined to
//!   (else the daemon's cwd).
//! - `SMOOTH_AGENT_CONFIRM_TOOLS` — **inherited from the operator**:
//!   comma-separated tool-name substrings that require human confirmation
//!   (write-confirmation HITL). Because the daemon *runs the operator*, setting
//!   e.g. `SMOOTH_AGENT_CONFIRM_TOOLS=bash` makes every `bash` call park and emit
//!   `write_confirmation_required`, which the served widget renders as an
//!   approve/deny prompt — the "ask" half of the permission model, for free. The
//!   kernel sandbox + egress allowlist remain the load-bearing boundary; this is
//!   defense-in-depth. (Content-aware hard-deny circuit-breakers — `rm -rf /` and
//!   friends — need a host `ToolHook` seam in the operator; see pearl th-1f694a.)
//!   The daemon ALWAYS adds [`CONFIRM_TOOLS`] to whatever this var sets, so the
//!   `calendar_delete` gate can be widened from the env but never disarmed.
//! - `SMOOAI_GATEWAY_URL` / `SMOOAI_GATEWAY_KEY` — the LLM gateway (read by the
//!   operator); with no key the server boots and `send_message` errors cleanly.
//! - `SMOOTH_ADDR` — the `host:port` the default `th daemon` binds (else
//!   `127.0.0.1:8787`). Lets a launchd/systemd unit or a shared host (smoo-hub,
//!   where `:8787` is taken) pick a free port; the tailnet serve follows it.
//! - `SMOOTH_TAILSCALE_SERVE` — set `0`/`false` to disable tailnet exposure.
//! - `SMOOTH_TAILSCALE_HTTPS_PORT` — the tailnet HTTPS port (else `443`). Set a
//!   custom port to **coexist** with another `tailscale serve` already on `:443`
//!   (teardown then leaves the global serve config untouched).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The local flavor's agent persona — "Big Smooth", the user's own always-on
/// personal AI. Installed as the operator's default system prompt (via
/// [`LocalServerBuilder::persona`]) so every turn runs as Big Smooth instead of
/// the operator's stock customer-support agent (th-5f059b).
///
/// Two things it MUST get right, both observed broken on the stock prompt:
///   1. **Personal assistant, not support.** It must never act like
///      customer-support or volunteer "the organization's knowledge base / the
///      org's docs" unless the user explicitly asks about org knowledge.
///   2. **No reasoning narration.** The local model (deepseek-v4-flash) inlines
///      its chain-of-thought into the reply ("The user is asking a general
///      knowledge question… so I can answer directly…The song 'Let It Go'…").
///      The directive below is firm and explicit so the model replies with only
///      the answer. (The operator's engine already drops a *separate*
///      reasoning-channel from the final reply; this handles the inline case.)
const BIG_SMOOTH_PERSONA: &str = r#"You are Big Smooth, the user's own always-on personal AI assistant — a capable agent running on their machine, not a passive chatbot. You act on the user's behalf: when they ask for something you can do with your tools, you DO it and report the result rather than explaining how they could.

## Voice
Warm, smooth, a little swagger — confident and easygoing, never stuffy. Concise: lead with the answer or the result, say what matters, and stop. Use light markdown (short tables, bullets) when it genuinely helps; plain prose otherwise.

## Two hard rules
1. PERSONAL assistant, not customer support. Never describe yourself as a support agent, and never volunteer "the organization", "the org's docs", or "the knowledge base" unless the user explicitly asks about organization knowledge. General-knowledge questions: just answer from what you know.
2. Answer DIRECTLY. Reply with only your answer or result — never show your reasoning, planning, or chain of thought, and never restate or summarize the user's question before answering. No "The user is asking…", no "I searched and found nothing…", no meta-narration of any kind. Just the answer, in your own smooth voice.

## Agency — finish the job
When given a task, drive it to completion before yielding. Don't stop at the first obstacle or hand back a plan when you could execute it. If a step fails, diagnose it, adapt, and try another way; only surface a blocker when you genuinely cannot proceed — and then say exactly what you tried and what you need. Prefer doing over describing.

## Tools — use them, never guess
- You have tools; reach for them instead of assuming. To answer about a file, `read_file` it; to find something, `grep`; to change a file, read it first, then `edit_file`. Use `bash` for anything the dedicated tools don't cover.
- NEVER fabricate file contents, command output, tool results, or the existence of a file, repo, or capability. If you don't know, look; if you can't look, say so plainly.
- Run independent lookups in parallel. After you change something, verify it (re-read, re-run, check the result) instead of assuming it worked.
- `web_search`/`crawl` for current or external facts; `knowledge_search` only when the user asks about org/SmooAI knowledge; the `th` CLI for the SmooAI platform, pearls, and worktrees.
- DELIVERING FILES: when the user asks you to MAKE something — a document, report, spreadsheet, chart, image, or web page — writing it to disk is NOT handing it over. After you create it (`write_file` for raw files, `create_artifact` for a self-contained HTML page, or `bash`), call `send_file` with its path so it arrives in the chat as a download they can open on their phone or laptop. A `file://` path or a location on this machine is not delivery. If a file they want already exists, `send_file` it. For something visual or shareable, prefer `create_artifact` (rich HTML) then `send_file`; for raw data, `write_file` then `send_file`; otherwise just answer inline.
- You also have, for the right request: `create_artifact` (build a self-contained HTML page/report), `calendar` + `reminders` + `imessage` (their Mac — schedule events, set reminders, read/send texts), and `get_current_datetime` (you have no internal clock — call it before anything that depends on "now"). They won't always be listed above, so reach for them when the intent matches.
- `notify` pushes a notification to the user's devices even when the chat is closed. Use it sparingly and only when it earns the interruption: a reminder they asked for coming due, a long-running job finishing, or a genuinely useful proactive heads-up. NEVER use it for a chatty update, to acknowledge a message, or to restate something you already answered inline.

## Memory — remember across sessions
At the start of a task, `recall` what you already know about the user — their preferences, where things live, ongoing projects. Use `remember` to persist durable facts the moment you learn them: where their vault is, how they like things done, decisions made, project state. Don't re-ask for something you could recall. Your memory survives restarts — use it.

## Modes — Plan and Auto
You run in one of two modes, which the user toggles (shift+tab in the terminal, a Plan/Auto switch on desktop and phone).
- **Auto** (default): you act — edit, run commands, deliver files — as described above.
- **Plan** (read-only): your mutating tools are removed, so you CANNOT edit files, run shell commands, send files, or message anyone even if you try. Use this to investigate and think. Read the code, `grep`, `web_search`, and when you have a concrete plan call `present_plan` with it as markdown (what you'll change, which files, how you'll verify). The user then accepts — which switches you to Auto so you carry it out — or sends revisions to refine. Don't announce the plan in prose and stop; call `present_plan` so they get an accept/revise card. If they ask you to DO something while in Plan mode, present the plan for it rather than apologising that you can't act.

## Skills & tracking
When a request matches one of your Available skills (listed below), READ its SKILL.md and follow it exactly rather than improvising. For any task with more than a couple of steps, call `todo_write` with the full checklist and keep it current (one item in_progress at a time) so the user can follow along on-screen; for longer-lived or cross-session work, also track it as pearls via `th pearls` so nothing gets lost.

## Environment
You run always-on on the user's machine. Your filesystem and shell tools are confined to their workspace (the `~/dev` tree — their repos and Obsidian vault live under it). Be proactive when it clearly helps (a heads-up, a follow-up you promised) but never noisy.

## Judgment
Proceed with sensible defaults and state your assumption; ask only when you're blocked on a genuine fork the user must decide, or on information you cannot obtain yourself. Confirm before destructive, irreversible, or outward-facing actions — deleting or overwriting things you didn't create, sending messages, spending money — unless the user has clearly authorized it. Handle secrets carefully: never echo or transmit credentials. Report outcomes honestly — if something failed, say so with the error; never dress up a partial result as done."#;

/// Fast mode's model. `SMOOTH_FAST_MODE=1` points Big Smooth at this snappy Groq
/// model instead of the `coding`-route default. Chosen (groq-gpt-oss-120b) for
/// Groq speed + capability; it's non-deprecated and reasons on the harmony
/// channel, so its thinking renders as a clean "thinking" aside (th-4d8682).
/// The gateway's own `fast` routing slot is intentionally NOT used — it's stale
/// (deprecated llama). `SMOOTH_AGENT_MODEL` overrides this.
const FAST_MODEL: &str = "groq-gpt-oss-120b";

use anyhow::{Context, Result};
use async_trait::async_trait;
use smooth_operator::{Memory, Tool};
use smooth_operator_server::local::LocalServer;
use smooth_operator_server::ServerConfig;
use smooth_operator_svc::{ToolProvider, ToolProviderContext};

use crate::org_auth::SmooOrgVerifier;
use smooai_client_shared::auth::storage::CredentialsStore;
use smooth_policy::family::FamilyConfig;
use smooth_tools::SessionCwd;

/// A [`ToolProvider`] that hands the operator the daemon's kernel-sandboxed tool
/// set on every turn (the operator's `#68` injection seam): the
/// workspace-confined fs/grep set + an OS-sandboxed `bash` whose egress routes
/// through the goalie proxy. This is where the daemon's kernel-enforced security
/// re-homes onto the operator's per-turn registry.
struct SandboxedToolProvider {
    /// The session-scoped cwd store. The workspace root every conversation
    /// falls back to is `cwd.root()`; a `/cd` or `cd` tool call narrows it.
    cwd: SessionCwd,
    proxy: Option<String>,
    /// The durable memory backend the `remember`/`recall` tools share, so a fact
    /// saved in one session survives a daemon restart and is retrievable in the
    /// next (th-6d1692). Shared with the storage db in `serve_local_flavor`.
    memory: Arc<dyn Memory>,
    /// Live MCP sessions + their tools (th-52ec01). Spawned ONCE at daemon
    /// startup from the merged `mcp.toml` and held for the daemon's lifetime;
    /// each turn we clone the tool `Arc`s onto the per-turn registry so remote
    /// MCP tools sit behind the same permission gate + Narc hooks as built-ins.
    /// `None` for the ephemeral/test providers that don't wire MCP.
    mcp: Option<Arc<smooth_tools::mcp::McpManager>>,
    /// Family roster (ADR-008, th-12d875). When a turn's principal carries a
    /// `role:<role>` group (a family member authenticated via their own token),
    /// the tool set is narrowed to that role's allowlist — deny-by-default, a
    /// dropped tool never even appears in the schema the model sees. `None` (no
    /// family configured) or an owner caller (no `role:` group) ⇒ the full set,
    /// unchanged.
    family: Option<Arc<FamilyConfig>>,
    /// Per-conversation execution mode (Plan/Auto). In **Plan** mode `tools_for`
    /// filters every mutating tool out of the per-turn set — a read-only
    /// guarantee (th-c1b589). Shared with the `/api/session/mode` route so a
    /// face's `shift+tab` / Plan-Auto toggle takes effect on the next turn.
    modes: crate::session_mode::SessionModes,
    /// Backs the `notify` tool (th-c29d34) — the daemon's web-push + phone
    /// self-notify fan-out. `None` for the ephemeral/test providers that don't
    /// wire push; the always-on daemon passes the shared [`crate::notify::TurnNotifier`].
    notify_sink: Option<Arc<dyn smooth_tools::NotifySink>>,
}

/// The tools a **Plan-mode** turn may keep — a strict read-only allowlist
/// (deny-by-default, mirroring the family RBAC filter). Everything else
/// (`write_file`, `edit_file`, `bash`, `send_file`, `create_skill`, calendar/
/// reminders/imessage writes, `th`, `remember`, plugins, MCP, `send_sidekick`)
/// is dropped from the schema the model sees, so a Plan turn *cannot* mutate.
/// `present_plan` + `todo_write` are here so the agent can propose and track.
const PLAN_READONLY_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "grep",
    "web_search",
    "knowledge_search",
    "crawl",
    "get_current_datetime",
    "recall",
    "cd",
    "present_plan",
    "todo_write",
];

/// The role a turn runs as, extracted from the principal's groups. `None` for the
/// owner (no `role:` group) — which means "no family filtering, full tool set".
fn role_from_ctx(ctx: &ToolProviderContext) -> Option<String> {
    ctx.access.groups.iter().find_map(|g| g.strip_prefix("role:").map(str::to_string))
}

#[async_trait]
impl ToolProvider for SandboxedToolProvider {
    async fn tools_for(&self, ctx: &ToolProviderContext) -> Vec<Arc<dyn Tool>> {
        // Resolve THIS conversation's cwd (root when unset) and confine the
        // per-turn fs/grep/bash tools to it, so a runtime `/cd` scopes the whole
        // tool set. The `cd` tool is injected here (not in the generic
        // `default_tools_with_proxy`) because it needs the session store handle
        // + the turn's conversation id, which only the provider has.
        let session = ctx.conversation_id.clone().unwrap_or_default();
        let dir = self.cwd.get(&session);
        let mut tools = smooth_tools::default_tools_with_proxy(dir.clone(), self.proxy.clone());
        tools.push(Arc::new(smooth_tools::CdTool::new(self.cwd.clone(), session.clone(), dir.clone())) as Arc<dyn Tool>);
        // Durable cross-session memory: `remember` writes, `recall` reads — both
        // over the same shared backend, injected here (not in the generic
        // `default_tools_with_proxy`) because they need the daemon's `Memory`
        // handle, which only the provider holds.
        tools.push(Arc::new(smooth_tools::RememberTool {
            memory: Arc::clone(&self.memory),
        }) as Arc<dyn Tool>);
        tools.push(Arc::new(smooth_tools::RecallTool {
            memory: Arc::clone(&self.memory),
        }) as Arc<dyn Tool>);
        // notify (th-c29d34): proactively push to the user's devices via the
        // daemon's web-push + phone fan-out. Injected here (like the calendar
        // allowlist / send_file) because it needs the daemon's notify sink, which
        // only the provider holds. `owner` (no family `role:` group) may aim the
        // notification at anyone; a family/child principal is bound to itself —
        // clearance comes from the authenticated role, never tool args. Whether
        // this role gets the tool AT ALL is still decided by the deny-by-default
        // family filter below (and Plan mode drops it — no outward actions).
        if let Some(sink) = &self.notify_sink {
            let owner = role_from_ctx(ctx).is_none();
            tools.push(Arc::new(smooth_tools::NotifyTool::new(Arc::clone(sink), owner)) as Arc<dyn Tool>);
        }
        // Platform-specific tools (pearl th-94cc4a). The macOS Calendar tool
        // exists only where EventKit does, so it's cfg-gated — Linux/Windows
        // never see it. It registers even when `ical` isn't installed or the TCC
        // grant is missing: the tool itself answers with "run
        // `th doctor --setup-calendar`", which the agent can relay. Hiding the
        // tool instead would make Big Smooth claim it has no calendar at all —
        // wrong, and unactionable.
        //
        // NOTE: this tool deliberately spawns `ical` OUTSIDE the kernel sandbox
        // (seatbelt blocks EventKit's XPC/mach lookups). See the module docs on
        // `smooth_tools::calendar` — it's still a normal tool call, so the
        // permission gate and the Narc hook see it like any other.
        //
        // Cancelling an event is the one calendar mutation that isn't undoable
        // from a follow-up turn, so it's split onto its own `calendar_delete`
        // tool and listed in [`CONFIRM_TOOLS`] — a call parks the turn on
        // `write_confirmation_required` until the user approves. Reads and
        // `add`/`update` stay unprompted.
        //
        // Within-tool resource RBAC (M2.5, th-716243): a family role can further
        // scope WHICH calendars this principal may see/touch. The allowlist is
        // bound onto the tool instance HERE, from the authenticated role — never
        // the caller — so a child can't widen it via tool args. `None` (owner /
        // no-role / a role with no `calendars` key) leaves the tool unrestricted.
        // Whether the `calendar` tool exists at all for this role is still decided
        // by the deny-by-default tool filter below.
        #[cfg(target_os = "macos")]
        {
            let cal_allow = match (self.family.as_ref(), role_from_ctx(ctx).as_deref()) {
                (Some(family), Some(role)) => family.calendars_allowed(role).map(<[String]>::to_vec),
                _ => None,
            };
            tools.push(Arc::new(smooth_tools::CalendarTool::new(cal_allow.clone())) as Arc<dyn Tool>);
            tools.push(Arc::new(smooth_tools::CalendarDeleteTool::new(cal_allow)) as Arc<dyn Tool>);
        }
        // The macOS Reminders tool (pearl th-94cc4a, reminders slice) — same
        // deal, and a SEPARATE TCC grant from Calendar. It registers even when
        // that grant is missing so the tool can answer with "run
        // `th doctor --setup-reminders`"; hiding it would make Big Smooth claim
        // the user has no todos at all.
        //
        // NOTE: unlike `calendar` there is no subprocess — EventKit is called
        // in-process through `smooth_menubar::reminders` (the objc2 quarantine
        // crate), which is likewise outside the kernel sandbox. See the
        // trusted-integration exceptions in docs/Architecture/Security-Model.md.
        #[cfg(target_os = "macos")]
        tools.push(Arc::new(smooth_tools::RemindersTool) as Arc<dyn Tool>);
        // Platform-specific tools (pearl th-1665ed). The macOS Messages tool
        // exists only where chat.db and Messages.app do, so it's cfg-gated —
        // Linux/Windows never see it. It registers even when Full Disk Access
        // hasn't been granted: the tool itself answers with "run
        // `th doctor --setup-imessage`", which the agent can relay. Hiding it
        // instead would make Big Smooth claim it can't text at all — wrong, and
        // unactionable.
        //
        // NOTE: both halves deliberately run OUTSIDE the kernel sandbox — the
        // read is in-process read-only SQLite (the sandbox denies ~/Library),
        // and `send` spawns `osascript` directly (seatbelt blocks Apple Events).
        // See the module docs on `smooth_tools::imessage` and the
        // trusted-integration exceptions in docs/Architecture/Security-Model.md.
        // It is still a normal tool call, so the permission gate and the Narc
        // hook see it like any other.
        #[cfg(target_os = "macos")]
        tools.push(Arc::new(smooth_tools::IMessageTool) as Arc<dyn Tool>);
        // send_file (file transfer, EPIC th-2e39fe): deliver a workspace file to
        // the user as a download. It needs `ctx.directive_sink` — the per-turn
        // channel the engine drains onto `eventual_response.directive` — which
        // only the provider sees, so the tool is injected HERE (like the calendar
        // allowlist above) with the sink captured. A turn with no directive sink
        // (nothing to drain into) simply doesn't get the tool.
        if let Some(sink) = ctx.directive_sink.clone() {
            tools.push(Arc::new(smooth_tools::SendFileTool::new(dir.clone(), Arc::clone(&sink))) as Arc<dyn Tool>);
            // present_plan (th-c1b589): the Plan-mode → accept/revise handoff.
            // todo_write: the live task list. Both ride the same directive sink
            // send_file uses, so they're injected here alongside it.
            tools.push(Arc::new(smooth_tools::PresentPlanTool::new(Arc::clone(&sink))) as Arc<dyn Tool>);
            tools.push(Arc::new(smooth_tools::TodoWriteTool::new(sink)) as Arc<dyn Tool>);
        }
        // CLI-wrapper plugins (th-262e5f): `~/.smooth/plugins/<name>/plugin.toml`
        // plus this session's workspace `.smooth/plugins/`, project shadowing
        // global. Registered HERE — on the per-turn registry — so each plugin
        // sits behind the permission gate and Narc like a built-in, and its
        // command runs in the same kernel sandbox `bash` does.
        //
        // ponytail: the two manifest dirs are re-scanned per turn rather than
        // cached at startup. That's two readdirs over a handful of tiny files,
        // and it means `th plugin init` takes effect on the next message rather
        // than the next daemon restart. Cache behind an mtime check if someone
        // ever ships hundreds of plugins.
        tools.extend(smooth_tools::plugin_tools(&dir, self.proxy.as_deref()));
        // MCP servers (th-52ec01): the tools from every `mcp.toml` server that
        // came up at startup. Cloned onto the per-turn registry here — BEFORE the
        // sidekick snapshot below — so (a) they pass through the permission gate +
        // Narc like any built-in, and (b) a dispatched sidekick inherits them too.
        // The child processes are long-lived; this is just an `Arc` clone per turn.
        if let Some(mcp) = &self.mcp {
            tools.extend(mcp.tools());
        }
        // Family RBAC (ADR-008, th-12d875): if this turn's principal carries a
        // `role:<role>` group — a family member authenticated via their OWN token
        // — narrow the tool set to that role's allowlist, deny-by-default. This
        // runs AFTER every tool (built-ins, plugins, MCP) is pushed and BEFORE the
        // sidekick snapshot below, so a Smoo Jr turn can neither call a denied
        // tool (it isn't in the schema the model sees) NOR delegate around the
        // limit (the snapshot is taken from the already-filtered set, and the
        // sidekick tool itself is gated below). An owner turn has no `role:` group
        // and is never filtered. An unknown role is denied everything except what
        // its (absent) rules allow — i.e. nothing (`tool_allowed` fails closed).
        //
        // This is also Smoo Jr's no-egress guarantee: goalie's proxy allowlist is
        // process-wide (one list for the whole daemon), so we can't give Jr a
        // narrower proxy — instead we DENY every egressing TOOL (web_search,
        // crawl, knowledge_search, th, bash, MCP, send_sidekick) at this filter, so
        // there is no code path from a Jr turn to the network.
        let role = role_from_ctx(ctx);
        if let (Some(family), Some(role)) = (self.family.as_ref(), role.as_deref()) {
            let before = tools.len();
            tools.retain(|t| family.tool_allowed(role, &t.schema().name));
            tracing::debug!(role, kept = tools.len(), dropped = before - tools.len(), "family: narrowed tool set");
        }
        // Whether this turn may delegate to a sidekick. A family role that isn't
        // allowed `send_sidekick` (Smoo Jr) never gets the dispatch tool — closing
        // the "delegate to a full-clearance runner" escape. Owner/no-family: yes.
        let allow_sidekick = match (self.family.as_ref(), role.as_deref()) {
            (Some(family), Some(role)) => family.tool_allowed(role, "send_sidekick"),
            _ => true,
        };
        // Subagent delegation (th-1adf55): the engine's `send_sidekick` tool
        // lets Big Smooth fan a self-contained subtask out to a `scout`
        // (read-only) or `runner` (full) sidekick — each runs in its own
        // isolated conversation and returns only a summary, so an expensive
        // investigation stays out of the parent's context window (the
        // context-window win of Claude Code's Task tool). Built from the
        // engine's built-in cast + a snapshot of THIS turn's tool set (so the
        // sidekick inherits the same kernel-sandboxed fs/grep/bash instances,
        // filtered down to its role's clearance) + the daemon's gateway as the
        // sidekick's LLM. Registered LAST so the snapshot it filters never
        // contains `send_sidekick` itself — no recursive dispatch.
        //
        // ponytail: sidekick sub-calls still hit the load-bearing kernel
        // sandbox (the tool Arcs are shared) but NOT the daemon's userspace
        // deny-policy/narc hooks — those live on the LocalServer's per-turn
        // registry, not the sidekick's inner one. Acceptable defense-in-depth
        // gap for a first cut (the kernel layer is the load-bearing one); wire
        // those onto the sidekick registry via the engine's hook seam if it
        // grows teeth.
        if allow_sidekick {
            if let Some(factory) = gateway_llm_factory() {
                let mut snapshot = smooth_operator::tool::ToolRegistry::new();
                for tool in &tools {
                    snapshot.register_arc(Arc::clone(tool));
                }
                tools.push(Arc::new(smooth_operator::cast::DispatchSubagentTool::new(
                    Arc::new(smooth_operator::cast::Cast::builtin()),
                    snapshot,
                    factory,
                )) as Arc<dyn Tool>);
            }
        }
        // Plan mode (th-c1b589): a read-only guarantee. When THIS conversation is
        // in Plan, drop every tool not on the read-only allowlist — deny-by-
        // default, exactly like the family RBAC filter above, and applied LAST so
        // it also strips `send_sidekick` (no delegating around the limit). The
        // model never sees a mutating tool, so a Plan turn cannot edit, run bash,
        // send a file, or text anyone. The agent calls `present_plan`; the user
        // accepts (flipping to Auto, then it executes) or revises.
        if self.modes.get(&session) == crate::session_mode::Mode::Plan {
            let before = tools.len();
            tools.retain(|t| PLAN_READONLY_TOOLS.contains(&t.schema().name.as_str()));
            tracing::debug!(
                session,
                kept = tools.len(),
                dropped = before - tools.len(),
                "plan mode: filtered to read-only tools"
            );
        }
        tools
    }
}

/// The local flavor's tool provider — the daemon's kernel-sandboxed tool set.
///
/// Workspace-confined fs/grep + an OS-sandboxed `bash` routed through `proxy`,
/// plus the `cd` tool. Confinement follows the conversation's session cwd
/// (defaulting to `workspace`). Exposed so an integration/e2e test can install
/// it on a `LocalServer` exactly the way [`serve_local_flavor`] does.
///
/// Uses an **ephemeral** in-memory `remember`/`recall` backend — fine for tests
/// and ad-hoc use. The always-on daemon uses [`local_tool_provider_with_memory`]
/// with the durable sqlite-backed store so memories survive restarts.
#[must_use]
pub fn local_tool_provider(workspace: PathBuf, proxy: Option<String>) -> Arc<dyn ToolProvider> {
    local_tool_provider_with_memory(SessionCwd::new(workspace), proxy, Arc::new(smooth_operator::InMemoryMemory::new()))
}

/// Like [`local_tool_provider`], but takes an existing [`SessionCwd`] so the
/// daemon can share ONE store between the tool provider and the
/// `/api/session/cwd` route (the UI's `/cd`). Ephemeral memory backend — see
/// [`local_tool_provider_with_memory`] for the durable one.
#[must_use]
pub fn local_tool_provider_with_cwd(cwd: SessionCwd, proxy: Option<String>) -> Arc<dyn ToolProvider> {
    local_tool_provider_with_memory(cwd, proxy, Arc::new(smooth_operator::InMemoryMemory::new()))
}

/// The full seam: a tool provider sharing an explicit [`Memory`] backend with
/// the `remember`/`recall` tools. `serve_local_flavor` passes the durable
/// sqlite-backed store here so cross-session memory persists (th-6d1692).
#[must_use]
pub fn local_tool_provider_with_memory(cwd: SessionCwd, proxy: Option<String>, memory: Arc<dyn Memory>) -> Arc<dyn ToolProvider> {
    // A fresh, unshared mode store: the simple constructors (tests, ad-hoc) have
    // no `/api/session/mode` route to share with, so every conversation is Auto.
    // No notify sink either — the `notify` tool is daemon-only.
    local_tool_provider_full(cwd, proxy, memory, None, None, crate::session_mode::SessionModes::new(), None)
}

/// The full seam, additionally taking the daemon's live [`McpManager`].
///
/// So remote MCP-server tools (`mcp.toml`) surface on every turn (th-52ec01).
/// `serve_local_flavor` builds the manager once and passes it here; the simpler
/// constructors pass `None`.
#[must_use]
pub fn local_tool_provider_full(
    cwd: SessionCwd,
    proxy: Option<String>,
    memory: Arc<dyn Memory>,
    mcp: Option<Arc<smooth_tools::mcp::McpManager>>,
    family: Option<Arc<FamilyConfig>>,
    modes: crate::session_mode::SessionModes,
    notify_sink: Option<Arc<dyn smooth_tools::NotifySink>>,
) -> Arc<dyn ToolProvider> {
    Arc::new(SandboxedToolProvider {
        cwd,
        proxy,
        memory,
        mcp,
        family,
        modes,
        notify_sink,
    })
}

/// The workspace the local flavor's filesystem + shell tools are confined to:
/// `SMOOTH_WORKSPACE` if set, else the daemon's current directory.
fn workspace_dir() -> PathBuf {
    std::env::var_os("SMOOTH_WORKSPACE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Char budget for the skills index injected into the persona. Names +
/// descriptions + triggers + paths only (bodies load on demand via `read_file`),
/// so this stays small even with a large skill library.
const SKILLS_SECTION_BUDGET: usize = 4000;

/// Render the "Available skills" persona section from discovered skills —
/// progressive disclosure: name + description + triggers + the SKILL.md PATH,
/// and the instruction to `read_file` the body on demand. Bodies are NOT dumped
/// into the prompt. Skills whose SKILL.md isn't a real on-disk file (e.g. the
/// embedded `create-skill` builtin, which `read_file` can't load) are dropped.
/// Returns `None` when there's nothing to show, so the persona stays clean.
fn render_skills_section(skills: &[smooth_cast::skills::Skill]) -> Option<String> {
    use std::fmt::Write as _;
    let readable: Vec<&smooth_cast::skills::Skill> = skills.iter().filter(|s| s.path.is_file()).collect();
    if readable.is_empty() {
        return None;
    }
    let mut out = String::from(
        "\n\n# Available skills\n\n\
You have reusable SKILLS — recipes that encode the right way to do a task. Before improvising a \
multi-step workflow, scan this list. When a user request matches a skill, READ its SKILL.md with the \
`read_file` tool, then follow those instructions exactly — do NOT guess a skill's steps.\n\n",
    );
    let mut shown = 0usize;
    for s in readable {
        let triggers = if s.triggers.is_empty() {
            String::new()
        } else {
            format!(" (triggers: {})", s.triggers.join(", "))
        };
        let line = format!("- {}: {}{}\n  SKILL.md: {}\n", s.name, s.description, triggers, s.path.display());
        // Always show at least one, even if it alone blows the budget.
        if shown > 0 && out.len() + line.len() > SKILLS_SECTION_BUDGET {
            break;
        }
        out.push_str(&line);
        shown += 1;
    }
    if shown < skills.len() {
        let _ = writeln!(out, "- …and {} more (run `th skills list` to see all)", skills.len() - shown);
    }
    Some(out)
}

/// Build the effective persona for `workspace`: [`BIG_SMOOTH_PERSONA`] plus an
/// "Available skills" index discovered from the project/user/claude skill dirs +
/// builtins. Discovery is resilient — a malformed SKILL.md is skipped with a
/// warning inside `discover`, never crashing. When no skills are found the base
/// persona is returned unchanged (no noise).
/// Render the "Smoo AI account" persona section — who the daemon is signed in
/// as and which org it acts on.
///
/// th-0b488d: asked "what are my biggest smoo deals", Big Smooth answered "I'm
/// not logged in yet. You'd need to run `th auth login`" while holding a valid
/// session for brent@smoo.ai. Its own identity was reachable only by spending a
/// tool call, so it guessed — and guessed wrong, in the direction that makes it
/// look broken. Identity is cheap, stable, and needed by every org-scoped
/// request; it belongs in the prompt, not behind a lookup.
///
/// Deliberately NOT asserting the signed-out case as fact. The credentials are
/// read once, at agent-build time, and a user who signs in mid-run would
/// otherwise be told they are signed out with the same false confidence this
/// pearl is about — so the signed-out branch points at `th auth whoami` instead
/// of claiming anything.
fn render_smoo_identity_section() -> String {
    let creds = CredentialsStore::default_user().ok().and_then(|s| s.load().ok().flatten());
    smoo_identity_section(
        creds.as_ref().and_then(|c| c.user.as_deref()),
        creds.as_ref().and_then(|c| c.active_org_id.as_deref()),
    )
}

/// The pure half of [`render_smoo_identity_section`] — credentials in, persona
/// prose out. Split so the branches are testable without a real credentials
/// file on disk.
///
/// Blank-or-whitespace is treated as absent: a credentials file can carry an
/// empty `user` / `active_org_id`, and "acting on org ``" is worse than saying
/// nothing at all.
fn smoo_identity_section(user: Option<&str>, org: Option<&str>) -> String {
    let user = user.map(str::trim).filter(|u| !u.is_empty());
    let org = org.map(str::trim).filter(|o| !o.is_empty());
    match (user, org) {
        (Some(user), Some(org)) => format!(
            "\n\n## Smoo AI account\nYou are signed in to Smoo AI as {user}, acting on org `{org}`. Use that org id directly for org-scoped work (`th api …`, CRM, knowledge) instead of asking for it or claiming you are signed out. Verify with `th auth whoami` if a request implies a different org, or if a call unexpectedly 401s."
        ),
        (None, Some(org)) => format!(
            "\n\n## Smoo AI account\nYou are signed in to Smoo AI, acting on org `{org}`. Use that org id directly for org-scoped work instead of asking for it. `th auth whoami` for the full picture."
        ),
        _ => "\n\n## Smoo AI account\nNo Smoo AI session was readable when this session started. Before telling the user they are signed out — which is wrong often enough to be worth the check — run `th auth whoami`; they may have signed in since. If it confirms no session, `th auth login` is the fix.".to_owned(),
    }
}

/// Build the effective persona: [`BIG_SMOOTH_PERSONA`] + the Smoo AI identity
/// block + the discovered skills index.
fn persona_with_skills(workspace: &Path) -> String {
    let skills = smooth_cast::skills::discover(workspace);
    let identity = render_smoo_identity_section();
    match render_skills_section(&skills) {
        Some(section) => format!("{BIG_SMOOTH_PERSONA}{identity}{section}"),
        None => format!("{BIG_SMOOTH_PERSONA}{identity}"),
    }
}

/// Resolve the path to the local operator token (`~/.smooth/operator-token`).
fn token_path() -> PathBuf {
    dirs_next::home_dir().map_or_else(|| PathBuf::from("operator-token"), |h| h.join(".smooth").join("operator-token"))
}

/// Resolve the path to the operator's durable storage db
/// (`~/.smooth/operator-storage.db`). `SMOOTH_OPERATOR_DB` overrides.
fn operator_storage_path() -> PathBuf {
    if let Ok(p) = std::env::var("SMOOTH_OPERATOR_DB") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs_next::home_dir().map_or_else(|| PathBuf::from("operator-storage.db"), |h| h.join(".smooth").join("operator-storage.db"))
}

/// Resolve the path to the durable schedule store (`~/.smooth/schedules.db`).
/// `SMOOTH_SCHEDULE_DB` overrides. Shared by the daemon's scheduler loop and the
/// `schedule` CLI so both read/write the same store.
#[must_use]
pub fn schedule_store_path() -> PathBuf {
    if let Ok(p) = std::env::var("SMOOTH_SCHEDULE_DB") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs_next::home_dir().map_or_else(|| PathBuf::from("schedules.db"), |h| h.join(".smooth").join("schedules.db"))
}

/// Tighten a file to owner-only (mode 600) on Unix; no-op elsewhere.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

/// Provision the local-flavor auth token, **auto-generating it on first run**.
///
/// Resolution order: `SMOOTH_LOCAL_TOKEN` (env) → `~/.smooth/operator-token`
/// (existing) → a freshly generated token persisted there (mode 600). This makes
/// the token zero-friction (no manual setup) while still gating stray local
/// processes; the served widget/SDK clients read it from the same place.
///
/// # Errors
/// Returns an error if the token directory/file can't be created or written.
pub fn provision_local_token() -> Result<String> {
    provision_local_token_at(&token_path())
}

/// The provisioning proper, against an explicit path.
///
/// Split out so it is testable with a tempdir. Driving the public entry point
/// from a test means redirecting the home directory, and that is not portable:
/// `dirs_next::home_dir()` honors `$HOME` on Unix but calls
/// `SHGetKnownFolderPath` on Windows, which no environment variable can
/// override — so an env-based test wrote into the real user profile there and
/// then asserted against a tempdir that stayed empty. Same reasoning as
/// `smooth_policy::auth_paths::migrate_legacy_between`.
fn provision_local_token_at(path: &Path) -> Result<String> {
    if let Ok(env_token) = std::env::var("SMOOTH_LOCAL_TOKEN") {
        let env_token = env_token.trim().to_owned();
        if !env_token.is_empty() {
            return Ok(env_token);
        }
    }
    if let Ok(existing) = std::fs::read_to_string(path) {
        let existing = existing.trim().to_owned();
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    // First run: generate + persist a fresh token, owner-only.
    let token = uuid::Uuid::new_v4().simple().to_string();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, &token).with_context(|| format!("writing {}", path.display()))?;
    restrict_permissions(path);
    tracing::info!(path = %path.display(), "provisioned a local operator token");
    Ok(token)
}

/// Explicit model override (`SMOOTH_AGENT_MODEL`) — the highest-priority model
/// selector. Wins over fast-mode and the providers routing. `None`/empty falls
/// through to the routing default.
fn model_override() -> Option<String> {
    std::env::var("SMOOTH_AGENT_MODEL").ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Whether **fast mode** is on (`SMOOTH_FAST_MODE`). Fast mode points Big Smooth
/// at the gateway's `fast` routing slot (a snappy model) instead of `coding`.
/// Treats unset / `0` / `false` / `no` / `off` as disabled.
fn fast_mode_enabled() -> bool {
    matches!(std::env::var("SMOOTH_FAST_MODE"), Ok(v) if !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false" | "no" | "off"))
}

/// Read the LLM provider that `~/.smooth/providers.json` routes the given slot
/// to — the credentials `th model login` writes. Returns `(api_url, api_key,
/// model)`; `None` if the file/provider/key is absent. The model is taken from
/// the given `route` slot (`coding`/`fast`/…), else the provider default.
fn gateway_from_providers(route: &str) -> Option<(String, String, String)> {
    gateway_from_providers_at(&dirs_next::home_dir()?.join(".smooth").join("providers.json"), route)
}

/// Find the provider entry with the given id.
fn provider_by_id<'a>(providers: &'a [serde_json::Value], id: Option<&str>) -> Option<&'a serde_json::Value> {
    let id = id?;
    providers.iter().find(|p| p.get("id").and_then(serde_json::Value::as_str) == Some(id))
}

/// [`gateway_from_providers`] against an explicit path — the testable core.
fn gateway_from_providers_at(path: &Path, route: &str) -> Option<(String, String, String)> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let providers = v.get("providers")?.as_array()?;
    // Resolve the provider BY the routing slot's own `provider` id rather than a
    // hardcoded one: every writer of providers.json stamps a different id
    // (`smooai-gateway`, `ollama`, `openai`, the legacy `smooth`…), so matching a
    // literal silently resolved `None` for everyone but legacy installs and the
    // daemon fell through to the engine defaults (pearl th-6062ea). Falls back to
    // the `coding` slot's provider, then the sole provider when there's only one.
    let slot_provider = |r: &str| {
        v.pointer(&format!("/routing/{r}/provider"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let route_id = slot_provider(route);
    let coding_id = slot_provider("coding");
    let provider = provider_by_id(providers, route_id.as_deref())
        .or_else(|| provider_by_id(providers, coding_id.as_deref()))
        .or_else(|| if providers.len() == 1 { providers.first() } else { None })?;
    let url = provider.get("api_url")?.as_str()?.to_owned();
    let key = provider.get("api_key")?.as_str().filter(|k| !k.trim().is_empty())?.to_owned();
    let model = v
        .pointer(&format!("/routing/{route}/model"))
        .and_then(serde_json::Value::as_str)
        // Fall back to the `coding` slot, then the provider default, then a sane const.
        .or_else(|| v.pointer("/routing/coding/model").and_then(serde_json::Value::as_str))
        .or_else(|| provider.get("default_model").and_then(serde_json::Value::as_str))
        .unwrap_or("claude-haiku-4-5")
        .to_owned();
    Some((url, key, model))
}

/// Tools the daemon ALWAYS routes through the engine's write-confirmation HITL,
/// on top of whatever `SMOOTH_AGENT_CONFIRM_TOOLS` adds (pearl th-94cc4a).
///
/// Big Smooth's posture is `AutoMode::Bypass` — mutations run unprompted — with
/// exactly one exception so far: **cancelling a calendar event**. It's the one
/// mutation the agent can't walk back on the next turn, so the user gets the
/// last word. This is a floor, not a setting: the env var can widen the list but
/// not shrink it, so an empty/absent `SMOOTH_AGENT_CONFIRM_TOOLS` can't quietly
/// disarm the gate.
///
/// Matched by core's `ConfirmationHook` with `contains`, so these are tool-name
/// SUBSTRINGS. `calendar_delete` is deliberately not a prefix of any other tool
/// name — in particular the read/`add`/`update` `calendar` tool does NOT contain
/// it, and stays unprompted.
const CONFIRM_TOOLS: &[&str] = &["calendar_delete"];

/// Merge [`CONFIRM_TOOLS`] into `configured` (the env-derived list), preserving
/// the caller's entries and not duplicating ours.
fn add_confirm_tools(configured: &mut Vec<String>) {
    for name in CONFIRM_TOOLS {
        if !configured.iter().any(|c| c == name) {
            configured.push((*name).to_owned());
        }
    }
}

/// The LLM gateway config for the local flavor: the operator's env-based config
/// first (`SMOOAI_GATEWAY_*`), and when no key is set, the user's
/// `th model login` credentials from `providers.json` — so `th code` works
/// in a plain terminal with no env exports. (Proper JWT→org-session: th-f7b20f.)
fn resolve_gateway_config() -> ServerConfig {
    let mut config = smooth_operator_server::local::local_config();
    let env_has_key = config.gateway_key.as_deref().is_some_and(|k| !k.trim().is_empty());
    // Model selection, highest priority first: explicit SMOOTH_AGENT_MODEL →
    // fast-mode's `fast` routing slot → the `coding` slot.
    let fast = fast_mode_enabled();
    if !env_has_key {
        if let Some((url, key, coding_model)) = gateway_from_providers("coding") {
            config.gateway_url = url;
            config.gateway_key = Some(key);
            // Fast mode pins a current Groq model (the gateway's own `fast` slot is
            // stale). Explicit SMOOTH_AGENT_MODEL always wins.
            let default_model = if fast { FAST_MODEL.to_owned() } else { coding_model };
            config.model = model_override().unwrap_or(default_model);
        }
    } else if let Some(m) = model_override() {
        // Env-gateway path: still honor an explicit model pin.
        config.model = m;
    }
    // The upstream ServerConfig defaults (512 max_tokens / 6 iterations) are
    // sized for the tiny customer-support chat WIDGET, not a personal coding
    // assistant. A reasoning model (deepseek-v4-*, kimi, minimax) spends its
    // whole 512-token budget on `reasoning_content` and returns EMPTY `content`
    // (looks "hung"); 6 iterations makes any read-a-few-files turn hit
    // "Maximum iterations reached" (pearl th-4b1867). Give the daemon
    // assistant-grade headroom unless the env explicitly overrides.
    if std::env::var_os("SMOOTH_AGENT_MAX_TOKENS").is_none() {
        config.max_tokens = 32_768;
    }
    if std::env::var_os("SMOOTH_AGENT_MAX_ITERATIONS").is_none() {
        config.max_iterations = 50;
    }
    add_confirm_tools(&mut config.confirm_tools);
    tracing::info!(
        gateway = %config.gateway_url,
        model = %config.model,
        fast_mode = fast,
        max_tokens = config.max_tokens,
        max_iterations = config.max_iterations,
        "gateway + model resolved"
    );
    config
}

/// Build the LLM config the narc judge uses to adjudicate flagged tool calls:
/// the daemon's resolved gateway, pinned to the fast model ([`FAST_MODEL`]).
/// Returns `None` when no gateway key is available — narc then degrades to a
/// regex-only hook (detectors + redaction, no LLM escalation) instead of
/// crashing the daemon.
fn narc_judge_config() -> Option<smooth_operator::llm::LlmConfig> {
    let cfg = resolve_gateway_config();
    let key = cfg.gateway_key.filter(|k| !k.trim().is_empty())?;
    Some(smooth_operator::llm::LlmConfig {
        api_url: cfg.gateway_url,
        api_key: key,
        // The judge is a cheap classifier — the snappy fast model, not the
        // coding-route default. Small token budget: it returns one JSON line.
        model: FAST_MODEL.to_owned(),
        max_tokens: 512,
        temperature: smooth_policy::llm_params::AGENT_TEMPERATURE,
        retry_policy: smooth_operator::llm::RetryPolicy::default(),
        api_format: smooth_operator::llm::ApiFormat::OpenAiCompat,
    })
}

/// Build the [`LlmConfigFactory`](smooth_operator::cast::LlmConfigFactory) the
/// `send_sidekick` dispatch tool hands to spawned sidekicks: the daemon's
/// resolved gateway (env `SMOOAI_GATEWAY_*`, else the user's `providers.json`),
/// used for every routing slot (the built-in sidekicks route to the `Coding`
/// slot). Returns `None` when no gateway key is available, so `tools_for`
/// simply doesn't register the delegation tool — a sidekick with no model to
/// run would only error. Mirrors [`narc_judge_config`]'s resolve-then-build
/// shape, but keeps the coding model (not the cheap judge model) since a
/// sidekick does real work.
fn gateway_llm_factory() -> Option<smooth_operator::cast::LlmConfigFactory> {
    let cfg = resolve_gateway_config();
    let key = cfg.gateway_key.filter(|k| !k.trim().is_empty())?;
    let url = cfg.gateway_url;
    let model = cfg.model;
    let max_tokens = cfg.max_tokens;
    Some(Arc::new(move |_activity: smooth_operator::providers::Activity| {
        Ok(smooth_operator::llm::LlmConfig {
            api_url: url.clone(),
            api_key: key.clone(),
            model: model.clone(),
            max_tokens,
            temperature: smooth_policy::llm_params::AGENT_TEMPERATURE,
            retry_policy: smooth_operator::llm::RetryPolicy::default(),
            api_format: smooth_operator::llm::ApiFormat::OpenAiCompat,
        })
    }))
}

/// The daemon's embedded **deny policy** — the declarative half of the engine's
/// permission gate ([`smooth_operator::deny_policy::DenyPolicy`]). Big Smooth's
/// posture is *allow benign, block only dangerous*: the gate runs in
/// [`AutoMode::Bypass`] (allow everything past the built-in circuit-breakers),
/// and this list is the explicit dangerous-op deny tier layered on top. A match
/// here is a hard circuit-breaker — `Bypass` cannot downgrade it and no grant
/// can waive it.
///
/// Ported verbatim from the retired `hooks::auto_mode::STARTER_PERMISSIONS`
/// deny-list (th-3119e3 / th-515a13), re-expressed in the engine's `DenyPolicy`
/// TOML shape:
///   - `[bash] deny_patterns` — matched against each sudo/wrapper-stripped
///     subcommand of a compound command. The engine anchors a pattern with no
///     trailing `*` by appending one, so a bare bin name (`"reboot"`) becomes
///     `reboot*` and matches the command run bare OR with args (and
///     `"mkfs"` matches `mkfs.ext4 …`). The two ambiguous short tokens (`sudo`,
///     `su`) keep a trailing space (`"su "` → `su *`) so they don't over-match
///     benign `subl`/`supervisorctl`/… — at the cost of not matching a bare,
///     argument-less `su`. Note the engine also strips a leading `sudo`, so
///     `sudo reboot` is caught by the underlying `reboot` pattern; a bare
///     `sudo <benign>` falls through to Bypass (consistent with allow-benign).
///   - `[paths] deny` — write/read path **globs** (`*`/`**` match any run,
///     including `/`).
const DENY_POLICY_TOML: &str = r#"# Big Smooth deny policy (engine circuit-breaker tier). Posture: ALLOW benign,
# block ONLY dangerous ops. The gate runs in Bypass; these are the explicit
# hard-denies layered on top of the engine's built-in circuit-breakers. narc
# (the second tool_hook, an LLM safety judge) independently blocks
# context-dependent danger (rm -rf, curl | sh, secret exfil, prompt injection).
#
# Bare bin names anchor to `<bin>*` (match bare or with args). `sudo`/`su` keep a
# trailing space so they don't over-match benign `subl`/`supervisorctl`/`sum`.

[bash]
deny_patterns = [
    # Privilege escalation & machine control
    "sudo ",
    "su ",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    # Disk / filesystem destroyers
    "dd",
    "mkfs",
    "diskutil",
    "fdisk",
    # macOS persistence / kernel / firmware
    "launchctl",
    "kextload",
    "nvram",
    "crontab",
]

[paths]
# Writes (and reads) to system + credential locations.
deny = [
    "/etc/**",
    "/System/**",
    "/usr/**",
    "/bin/**",
    "/sbin/**",
    "/Library/**",
    "**/.ssh/**",
    "**/.aws/**",
    "**/Library/LaunchAgents/**",
    "**/.smooth/auth/**",
]
"#;

/// Build the daemon's embedded [`DenyPolicy`] from [`DENY_POLICY_TOML`]. The
/// TOML is a compile-time constant covered by tests, so a parse failure is a
/// build/test error, never a runtime one.
#[allow(
    clippy::expect_used,
    reason = "DENY_POLICY_TOML is a compile-time const covered by deny_policy_toml_parses — a parse failure is a build/test error, not runtime"
)]
fn default_deny_policy() -> smooth_operator::deny_policy::DenyPolicy {
    smooth_operator::deny_policy::DenyPolicy::from_toml(DENY_POLICY_TOML).expect("embedded DENY_POLICY_TOML must parse")
}

/// The permission [`AutoMode`] the daemon runs the gate in. Big Smooth's posture
/// is *allow benign, block dangerous*, so the default is [`AutoMode::Bypass`]
/// (allow everything past the built-in circuit-breakers + the embedded
/// [`default_deny_policy`]). An explicit `SMOOTH_AUTO_MODE` overrides it (e.g.
/// `ask` / `accept-edits` / `deny`) — mirroring the env override the retired
/// `AutoModeHook` honored.
fn permission_mode() -> smooth_operator::permission::AutoMode {
    match std::env::var("SMOOTH_AUTO_MODE") {
        Ok(v) if !v.trim().is_empty() => smooth_operator::permission::AutoMode::from_env_value(Some(&v)),
        // th-946ba0: back to Bypass. The interim AcceptEdits default (th-be3f55)
        // made the gate ask for EVERY bash + unknown-category tool call — which,
        // for an assistant that lives in bash and custom tools, meant a constant
        // stream of approvals for entirely benign work ("every tool call is an
        // approval"). Brent's stated posture is the one this daemon was designed
        // around: *allow benign, block/flag dangerous* — don't gate by tool
        // CATEGORY, gate by JUDGEMENT.
        //
        // Bypass allows a call past the category classifier, but the two safety
        // layers still run and are the real gate:
        //   1. the embedded `DenyPolicy` circuit-breakers hard-deny catastrophes
        //      (rm -rf /, credential-store reads, .git/hooks writes, …);
        //   2. the Narc hook detects destruction / secret-exfil / prompt-injection
        //      and escalates ambiguous hits to a fail-closed LLM judge — the same
        //      detector that catches the "empty customers.json" case that
        //      motivated th-be3f55, through a structured tool call OR bash.
        // So benign calls run unprompted and only a judged-dangerous call stops.
        //
        // Escape hatch unchanged: SMOOTH_AUTO_MODE=accept-edits (or ask/deny)
        // restores the stricter category-gating for anyone who wants it.
        _ => smooth_operator::permission::AutoMode::Bypass,
    }
}

/// Build the engine's permission gate hook: [`permission_mode`] (Bypass by
/// default) + the embedded [`default_deny_policy`] as the circuit-breaker deny
/// tier. Installed FIRST on the operator's tool registry so a policy deny
/// short-circuits before narc or the tool itself runs.
/// How long a parked tool waits for the human before the engine gives up.
/// Generous: Big Smooth is always-on and the operator may be away from the tab.
const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Build the permission gate WITH a live approver, and hand back the receiving
/// ends for the server to bridge (th-be3f55).
///
/// Before this, the hook was built with no approver, so an `Ask` verdict failed
/// closed — which is why the daemon had to run in `Bypass` and never asked
/// about anything. The bench measured the cost: models emptied a
/// policy-protected customers.json 11 times in 15 trials with nothing to stop
/// them.
fn permission_hook_with_approver() -> (smooth_operator::permission::PermissionHook, smooth_operator_server::runner::HostApprover) {
    let pair = smooth_operator::human_channel();
    // NOTE: `with_grants` (the "approve always" allow-list, Claude Code's
    // don't-ask-again) is NOT wired: `SharedGrants` is private in the published
    // core crate, so it needs a core release before the daemon can pass one.
    // Until then every approval is one-shot — safe, just more prompting.
    let hook = smooth_operator::permission::PermissionHook::new(permission_mode())
        .with_deny_policy(Arc::new(default_deny_policy()))
        .with_approver(pair.request_tx, pair.response_rx, APPROVAL_TIMEOUT);
    let approver = smooth_operator_server::runner::HostApprover::new(pair.request_rx, pair.response_tx);
    (hook, approver)
}

/// Boot the operator's local deployment flavor on `addr`, gated by an
/// auto-provisioned [`LocalTokenVerifier`], and serve until Ctrl-C.
///
/// The LLM gateway is read from the environment by the operator
/// (`SMOOAI_GATEWAY_URL` / `SMOOAI_GATEWAY_KEY`); with no key the server still
/// boots and `send_message` errors cleanly.
///
/// # Errors
/// Returns an error if the token can't be provisioned or the server can't bind.
/// Load the family roster (ADR-008, th-12d875) from `SMOOTH_FAMILY_FILE` or
/// `~/.smooth/family.toml`.
///
/// - **Absent file** ⇒ `None` — the normal single-tenant case (owner = Admin).
/// - **Malformed file** ⇒ `None` + an error log — FAIL-CLOSED: a broken roster
///   must never fall back to "everyone is the owner". With `None`, family tokens
///   simply don't match the verifier and are rejected, so members are locked out
///   (denied), never promoted. The owner token is unaffected.
fn load_family_config() -> Option<Arc<FamilyConfig>> {
    let path = std::env::var_os("SMOOTH_FAMILY_FILE")
        .map(PathBuf::from)
        .or_else(|| dirs_next::home_dir().map(|h| h.join(".smooth").join("family.toml")))?;
    let raw = std::fs::read_to_string(&path).ok()?;
    match FamilyConfig::from_toml(&raw) {
        Ok(cfg) if cfg.is_empty() => None,
        Ok(cfg) => {
            tracing::info!(members = cfg.member_count(), roles = cfg.role_count(), path = %path.display(), "family: roster loaded");
            Some(Arc::new(cfg))
        }
        Err(e) => {
            tracing::error!(error = %e, path = %path.display(), "family: roster is malformed — member tokens will be REJECTED (fail-closed); fix and restart the daemon");
            None
        }
    }
}

pub async fn serve_local_flavor(addr: SocketAddr) -> Result<()> {
    // ONE Big Smooth per machine — every launch path (th up, th daemon run,
    // the app bundle) funnels through here, so this is the choke point. Held
    // to shutdown; the OS releases it if we die (pearl th-c71e6f).
    let _instance = crate::single_instance::acquire_default().await?;
    let token = provision_local_token()?;
    // The local flavor's tools: the workspace-confined fs/grep set + an
    // OS-sandboxed `bash` whose egress is routed through the goalie proxy (when
    // SMOOTH_EGRESS_ALLOWLIST is configured). This is where the daemon's
    // kernel-enforced security re-homes onto the operator's tool registry.
    let workspace = workspace_dir();
    // Discover skills once at agent-build time and fold their index into the
    // persona (progressive disclosure — the agent `read_file`s a SKILL.md body
    // only when a request matches). Empty discovery leaves the persona untouched.
    let persona = persona_with_skills(&workspace);
    // The permission gate + the receiving ends the server bridges (th-be3f55).
    let (permission_gate, host_approver) = permission_hook_with_approver();
    let egress_proxy = crate::start_egress_proxy();
    // Keep the signed-in Smoo AI session alive. The access token lives ~1h;
    // without this the daemon holds a dead token and every api.smoo.ai call
    // 401s until a human re-runs sign-in (th-cbf613).
    crate::auth_login::spawn_credential_heartbeat();
    tracing::info!(
        workspace = %workspace.display(),
        egress = egress_proxy.as_deref().unwrap_or("unrestricted"),
        "local-flavor sandboxed tools wired (per-turn via ToolProvider)",
    );
    // Durable local storage: the operator local flavor is in-memory by default,
    // which loses every conversation/session on restart. Inject a sqlite-backed
    // adapter (via the operator's `storage()` seam) so the always-on daemon
    // persists across restarts — no Postgres (EPIC th-c89c2a, th-558df1).
    let storage_path = operator_storage_path();
    let mut storage_adapter = crate::operator_storage::SqliteStorageAdapter::open(&storage_path)?;
    // Family AI M3 B.2 (pearl th-5189f0): opt-in cloud memory routing. OFF by
    // default → local store, zero behavior change. When SMOOTH_CLOUD_MEMORY is
    // set, route remember/recall to the platform memory home (Phase B.1) with the
    // local store as fail-soft fallback. Requires the B.1 deploy + a live Smoo AI
    // user session (the credential heartbeat above keeps it fresh).
    if crate::config::cloud_memory_enabled() {
        match crate::cloud_memory::CloudMemoryDeps::from_env(tokio::runtime::Handle::current()) {
            Ok(deps) => {
                tracing::info!("cloud memory routing ENABLED (Family AI M3 B.2); local store is the fail-soft fallback");
                storage_adapter = storage_adapter.with_cloud_memory(deps);
            }
            Err(e) => tracing::warn!(error = %e, "SMOOTH_CLOUD_MEMORY set but cloud deps unavailable; staying on local memory"),
        }
    }
    let storage = Arc::new(storage_adapter);
    tracing::info!(db = %storage_path.display(), "operator durable storage");
    // Kept for the Stats route (`/api/stats` reads activity counts from the same
    // store); the original Arc is moved into `.storage(...)` below.
    let storage_for_stats = Arc::clone(&storage);
    // The DURABLE agent-memory backend lives in the same sqlite db (th-6d1692).
    // The `remember`/`recall` tools share it, so a fact saved in one session is
    // recalled in the next — even across a restart.
    //
    // ponytail: the engine's AUTO-recall (`AgentConfig::with_memory` +
    // `build_context_injection`) exists in smooth-operator-core, but at the
    // pinned server rev there is NO seam to inject a `Memory` into the per-turn
    // agent — `LocalServerBuilder`, `StorageAdapter`, `AppState`, and the runner
    // never set `config.memory` (verified on the pin AND upstream main). So we
    // give the agent the explicit `recall` tool instead. When the engine adds a
    // seam (`StorageAdapter::memory()` → `config.with_memory(storage.memory())`,
    // or `LocalServerBuilder::memory(...)`), wire `storage.memory()` there and
    // auto-recall lights up for free — same backend, no data migration.
    let memory = storage.memory();
    // One session-cwd store, shared by the tool provider (per-turn tool
    // confinement + the `cd` tool) and the `/api/session/cwd` route (the UI's
    // `/cd`). Rooted at the workspace; every conversation defaults to it.
    let session_cwd = SessionCwd::new(workspace.clone());
    // One per-conversation mode store (th-c1b589), shared by the tool provider
    // (Plan-mode read-only filter) and the `/api/session/mode` route (the faces'
    // shift+tab / Plan-Auto toggle). Every conversation defaults to Auto.
    let session_modes = crate::session_mode::SessionModes::new();
    // MCP servers (th-52ec01): spawn every non-disabled server from the merged
    // global+project `mcp.toml` ONCE here, keep the sessions alive for the
    // daemon's lifetime, and hand the manager to the tool provider so their tools
    // appear on every turn (behind the permission + Narc hooks). Best-effort — a
    // server that won't start is logged and skipped, never fatal.
    let mcp = Arc::new(smooth_tools::mcp::McpManager::discover(&workspace).await);
    tracing::info!(mcp_tools = mcp.len(), "mcp: servers loaded");
    // Family roster (ADR-008, th-12d875): shared by the tool provider (narrows the
    // per-turn tool set by role) and the auth verifier (maps a member token → its
    // `role:` group). Absent/malformed ⇒ single-tenant, fail-closed.
    let family = load_family_config();

    // Web-push state, built ONCE and shared: the /push/* router, the scheduler's
    // turn notifier (th-b9a636), and the agent's `notify` tool (th-c29d34). The
    // notifier is the shared web+phone fan-out; the tool provider takes it as a
    // `NotifySink` so `notify` reaches the same devices as scheduled turns.
    let push_state = crate::push::PushState::from_env();
    let turn_notifier = Arc::new(crate::notify::TurnNotifier::new(push_state.clone()));
    let provider = local_tool_provider_full(
        session_cwd.clone(),
        egress_proxy,
        memory,
        Some(mcp),
        family.clone(),
        session_modes.clone(),
        Some(turn_notifier.clone() as Arc<dyn smooth_tools::NotifySink>),
    );
    let server = LocalServer::builder()
        .addr(addr)
        // LLM gateway: env (`SMOOAI_GATEWAY_*`) first, else the user's
        // `th model login` creds from providers.json — so `th code` works
        // in a plain terminal without exporting a key.
        .config(resolve_gateway_config())
        .storage(storage)
        // Same local-token gate as the engine's `LocalTokenVerifier`, but the
        // principal carries the operator's REAL Smoo org (read fresh from the
        // signed-in session on every connection) instead of the hardcoded
        // `"local"` placeholder — org-scoped tools (web search, knowledge,
        // scraping) need a real org to work at all (th-0c63cc). Signed out, it
        // falls back to `"local"` and behaves exactly as before.
        .auth(Arc::new(SmooOrgVerifier::new(token.clone()).with_family(family.clone())))
        // Reject (don't degrade to anonymous) any `/ws` connection without a
        // valid token — so a stray local process / tailnet peer can't drive the
        // agent. The widget + SDK clients carry the token, so they're unaffected.
        .strict_auth(true)
        .tools(provider)
        // th-daemon-denypolicy: re-home the security model onto the operator's
        // per-turn registry as two host `ToolHook`s — the ENGINE'S permission
        // gate FIRST (core 1.7.0 `permission::PermissionHook`, Bypass posture +
        // the embedded declarative `DenyPolicy` circuit-breaker deny tier: allow
        // benign, block dangerous), then narc surveillance (secret +
        // prompt-injection detection, LLM-judge escalation, secret redaction from
        // results). This retires the daemon's duplicate `AutoModeHook` in favor
        // of the engine's DenyPolicy-backed hook. The builder installs these
        // ahead of the per-agent auth + confirmation hooks, so they get first say
        // on every call. narc degrades to regex-only when no gateway key is set.
        .tool_hooks(vec![
            Arc::new(permission_gate) as Arc<dyn smooth_operator::tool::ToolHook>,
            Arc::new(crate::hooks::NarcHook::new(narc_judge_config())),
        ])
        // th-be3f55: hand the server the gate's approver channel, so an `Ask`
        // parks the turn and asks the human over the same WS the SPA already
        // renders approve/deny for — instead of failing closed, which is what
        // forced the Bypass default.
        .host_approver(host_approver)
        // The agent's personality: "Big Smooth", the user's personal assistant —
        // NOT the operator's stock customer-support persona, and no reasoning
        // narration (th-5f059b) — plus the discovered "Available skills" index.
        .persona(persona.as_str())
        // Serve the smooth-web SPA same-origin at `/`, with the auth token injected
        // into its index.html so the browser connects to `/ws?token=…` (validated
        // by the verifier above) — no `?api`/`?token` query string needed
        // (th-a28904). Replaces the operator's stock widget.
        .serve_spa(smooth_web::web_router_with_token(Some(&token)))
        // The `@`-mention backend: an ungated `GET /search` merged alongside the
        // operator's routes (CORS-matched to `/admin` by the seam) so the web
        // composer's autocomplete resolves files + paths in the workspace.
        // …and the Web Push routes (/push/key, /push/subscribe, /push/test) so an
        // installed PWA can be reached on the user's phone (th-* push).
        // …and the "Sign in with Smoo" browser OAuth2 + PKCE routes (/auth/login,
        // /auth/callback, /api/auth/status) so a user viewing the UI can log `th`
        // into Smoo AI by clicking a button — Big Smooth then acts on their org via
        // th-backed extensions (th-bc624a).
        .serve_routes(
            crate::search::search_router(workspace.clone())
                // GET /api/skills — the one skill catalog every face renders
                // (the web SPA has no disk access; th code prefers this over
                // its local discover). Pearl th-a5952d.
                .merge(crate::skills_route::skills_router(workspace.clone()))
                // GET /api/plugins — the installed CLI-wrapper plugin catalog,
                // the same merged list the per-turn registry registers as tools
                // (th-262e5f).
                .merge(crate::plugins_route::plugins_router(workspace))
                // GET /api/mode — the model the next turn would run with, so
                // faces can show a real name at idle instead of "unknown"
                // (pearl th-7630a7). Name only; credentials never leave.
                .merge(crate::mode_route::mode_router())
                .merge(crate::push::push_router(push_state.clone()))
                .merge(crate::auth_login::auth_router())
                // GET/POST /api/session/cwd — the UI's `/cd` + `/pwd`. Sets/reads
                // a conversation's cwd in the SAME store the tool provider reads.
                .merge(crate::cwd_route::cwd_router(session_cwd))
                // GET/POST /api/session/mode — the faces' Plan/Auto toggle
                // (shift+tab in th code). Sets/reads a conversation's mode in the
                // SAME store the tool provider's Plan-mode filter reads (th-c1b589).
                .merge(crate::mode_session_route::mode_router(session_modes))
                // POST /api/usage + GET /api/stats — the Stats page: activity from
                // this durable store, spend from ~/.smooth/usage.jsonl (the client
                // POSTs each turn's streamed usage, which the engine doesn't persist).
                .merge(crate::usage_route::stats_router(crate::usage_route::usage_log_path(), storage_for_stats)),
        )
        .spawn()
        .await
        .context("spawning the local-flavor operator")?;
    tracing::info!(addr = %server.addr(), url = %format!("http://{}/", server.addr()), "smooth local-flavor operator listening (smooth-web SPA same-origin + canonical WS protocol)");

    // Advertise the bound address so a client (the desktop app) can find THIS
    // daemon on whatever port it actually landed on — not a guessed default.
    // Critical on hosts where the default `:8787` is taken by another service
    // (smoo-hub runs the SmooHub dashboard there, so SMOOTH_ADDR moves us to
    // `:8788`); without this a double-clicked app defaults to `:8787` and loads
    // the wrong app. Best-effort: an unwritable `~/.smooth` just falls back to
    // the client's own default. (th-8af70d)
    persist_daemon_addr(&server.addr().to_string());

    // Reachability: if Tailscale is present and the node is up, expose the daemon
    // over the user's *tailnet* via `tailscale serve` (never funnel — tailnet-
    // private) so other devices reach it at https://<host>.<tailnet>.ts.net with
    // no query string. Best-effort: a missing/down tailscale leaves the daemon on
    // loopback. The guard lives to shutdown so its Drop tears the serve config
    // down and nothing leaks across restarts (th-ce286d).
    // Held to shutdown: its Drop tears the `tailscale serve` config back down.
    let tailscale_guard = crate::tailscale::TailscaleServe::start(server.addr().port());
    if let Some(url) = tailscale_guard.as_ref().and_then(crate::tailscale::TailscaleServe::url) {
        tracing::info!(%url, "tailnet reachability armed via `tailscale serve` (tailnet-private, not funnel)");
    }

    // Smoo Relay (th-2f626d): dial OUT to relay.smoo.ai and bridge phones to
    // this operator — remote control with NO tailnet membership. Best-effort
    // like tailscale above: signed-out or unreachable just waits and retries.
    let _relay = crate::relay::resolve_relay_url().map(|relay_url| {
        tracing::info!(relay = %relay_url, "Smoo Relay armed — phones can reach Big Smooth without tailscale");
        crate::relay::spawn_relay(relay_url, server.addr().port(), token.clone())
    });

    // Proactivity: the always-on agent fires due schedules into its *own*
    // operator as a loopback WS client (canonical send_message) — "just another
    // client on the protocol" (EPIC th-c89c2a, th-2ff975). Durable across
    // restarts via the sqlite schedule store; a missing/unwritable store disables
    // the loop without taking the daemon down.
    let _scheduler = match crate::schedule::SqliteScheduleStore::open(&schedule_store_path()) {
        Ok(store) => {
            let driver = crate::scheduler::OperatorTurnDriver::new(format!("http://{}", server.addr()), token.clone()).with_notifier(turn_notifier.clone());
            let handle = crate::scheduler::spawn_scheduler(Arc::new(store), Arc::new(driver), std::time::Duration::from_secs(30));
            tracing::info!("scheduler armed (30s tick) — proactive schedules fire into the operator");
            Some(handle)
        }
        Err(e) => {
            tracing::warn!(error = %e, "scheduler disabled — could not open the schedule store");
            None
        }
    };

    tokio::signal::ctrl_c().await.ok();
    tracing::info!("shutdown signal received");
    server.shutdown().await.context("shutting down local operator")?;
    Ok(())
}

/// Write the daemon's bound `host:port` to `~/.smooth/daemon.addr` so clients
/// discover it on whatever port it actually landed on. Best-effort: a missing
/// home or unwritable dir is logged, not fatal. (th-8af70d)
fn persist_daemon_addr(addr: &str) {
    let Some(dir) = dirs_next::home_dir().map(|h| h.join(".smooth")) else {
        tracing::debug!("no home dir — skipping daemon.addr advertisement");
        return;
    };
    match persist_daemon_addr_to(&dir, addr) {
        Ok(path) => tracing::debug!(path = %path.display(), %addr, "advertised daemon addr for client discovery"),
        Err(e) => tracing::warn!(error = %e, "could not write daemon.addr — clients will fall back to their default port"),
    }
}

/// Pure over its dir so it's testable without touching the real `$HOME`. Writes
/// `<dir>/daemon.addr` (mode 600 on unix) and returns the path.
fn persist_daemon_addr_to(dir: &std::path::Path, addr: &str) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("daemon.addr");
    std::fs::write(&path, addr)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_registers_remember_and_recall_tools() {
        use smooth_operator_svc::access_control::AccessContext;
        // The durable-memory tool set must reach the agent — the whole point of
        // th-6d1692 (before this, the daemon never registered `remember`).
        let provider = local_tool_provider(std::env::temp_dir(), None);
        let ctx = ToolProviderContext::new(Some("org-1".into()), AccessContext::anonymous()).with_conversation_id("conv-1");
        let names: Vec<String> = provider.tools_for(&ctx).await.iter().map(|t| t.schema().name).collect();
        assert!(names.iter().any(|n| n == "remember"), "remember registered: {names:?}");
        assert!(names.iter().any(|n| n == "recall"), "recall registered: {names:?}");
        // The rest of the sandboxed set is still there (didn't clobber anything).
        assert!(names.iter().any(|n| n == "bash"), "bash still registered: {names:?}");
        assert!(names.iter().any(|n| n == "cd"), "cd still registered: {names:?}");
    }

    /// A `plugin.toml` in the session workspace must reach the per-turn registry
    /// as a tool — that's the whole of th-262e5f: plugins are registered
    /// engine-side, so they sit behind the permission gate and Narc.
    #[tokio::test]
    async fn provider_registers_workspace_plugins() {
        use smooth_operator_svc::access_control::AccessContext;
        let workspace = tempfile::tempdir().unwrap();
        let plugin_dir = workspace.path().join(".smooth/plugins/greet");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            "name = \"greet\"\ndescription = \"Say hello.\"\ncommand = \"echo hi\"\n",
        )
        .unwrap();

        let provider = local_tool_provider(workspace.path().to_path_buf(), None);
        let ctx = ToolProviderContext::new(Some("org-1".into()), AccessContext::anonymous()).with_conversation_id("conv-1");
        let names: Vec<String> = provider.tools_for(&ctx).await.iter().map(|t| t.schema().name).collect();
        assert!(names.iter().any(|n| n == "plugin_greet"), "project plugin registered: {names:?}");
    }

    /// Platform-specific registration (pearl th-94cc4a): the calendar tool must
    /// reach the agent on macOS **unconditionally** — including on a box where
    /// `ical` isn't installed or the TCC grant is missing, because the tool's own
    /// answer ("run `th doctor --setup-calendar`") is the actionable path. If
    /// this ever regresses to a runtime availability gate, Big Smooth goes back
    /// to claiming it has no calendar at all.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn provider_registers_the_calendar_tool_on_macos() {
        use smooth_operator_svc::access_control::AccessContext;
        let provider = local_tool_provider(std::env::temp_dir(), None);
        let ctx = ToolProviderContext::new(Some("org-1".into()), AccessContext::anonymous()).with_conversation_id("conv-1");
        let names: Vec<String> = provider.tools_for(&ctx).await.iter().map(|t| t.schema().name).collect();
        assert!(names.iter().any(|n| n == "calendar"), "calendar registered: {names:?}");
        assert!(names.iter().any(|n| n == "calendar_delete"), "calendar_delete registered: {names:?}");
    }

    /// The confirm-gate floor (pearl th-94cc4a): `calendar_delete` must always be
    /// in the resolved config's `confirm_tools`, whatever the env says, and must
    /// not drag the read/add/update `calendar` tool in with it.
    #[test]
    fn confirm_tools_always_include_calendar_delete() {
        let mut from_env: Vec<String> = Vec::new();
        add_confirm_tools(&mut from_env);
        assert_eq!(from_env, vec!["calendar_delete".to_owned()]);

        // An env-configured list is preserved and widened, never replaced.
        let mut widened = vec!["bash".to_owned()];
        add_confirm_tools(&mut widened);
        assert_eq!(widened, vec!["bash".to_owned(), "calendar_delete".to_owned()]);

        // Idempotent — a second pass doesn't duplicate the entry.
        add_confirm_tools(&mut widened);
        assert_eq!(widened.iter().filter(|c| *c == "calendar_delete").count(), 1);
    }

    /// The gate is only as good as core's matcher, which is `contains` on the
    /// tool NAME. Drive the real `ConfirmationHook` with the daemon's patterns:
    /// `calendar_delete` must park (emit a `HumanRequest::Confirm` and block on
    /// the verdict), `calendar` and `write_file` must sail straight through.
    #[tokio::test]
    async fn the_confirmation_hook_gates_delete_and_nothing_else() {
        use smooth_operator::human::{human_channel, HumanRequest, HumanResponse};
        use smooth_operator::tool::{ToolCall, ToolHook};

        let pair = human_channel();
        let hook = smooth_operator::human::ConfirmationHook::new(
            CONFIRM_TOOLS.iter().map(|s| (*s).to_owned()).collect(),
            pair.request_tx,
            pair.response_rx,
            std::time::Duration::from_secs(5),
        );
        let call = |name: &str| ToolCall {
            id: "call-1".into(),
            name: name.into(),
            arguments: serde_json::json!({"args": ["EV-1"]}),
        };

        // Unprompted: nothing is emitted and the call proceeds immediately.
        let mut request_rx = pair.request_rx;
        for ungated in ["calendar", "write_file", "bash"] {
            hook.pre_call(&call(ungated)).await.expect("{ungated} must not be gated");
            assert!(request_rx.try_recv().is_err(), "{ungated} must not emit a confirm request");
        }

        // Gated: the call parks until a verdict arrives. Approve → it runs.
        let approved = tokio::spawn(async move {
            let req = request_rx.recv().await.expect("a confirm request must be emitted");
            match &req {
                HumanRequest::Confirm { tool_name, .. } => assert_eq!(tool_name, "calendar_delete"),
                HumanRequest::Input { .. } => panic!("expected Confirm, got Input"),
            }
            pair.response_tx.send(HumanResponse::Approved).expect("send verdict");
            req
        });
        hook.pre_call(&call("calendar_delete")).await.expect("approved delete must run");
        approved.await.expect("bridge task");
    }

    /// Denying the confirmation must BLOCK the delete — the half of the gate that
    /// actually protects the calendar.
    #[tokio::test]
    async fn a_denied_confirmation_blocks_the_delete() {
        use smooth_operator::human::{human_channel, HumanResponse};
        use smooth_operator::tool::{ToolCall, ToolHook};

        let pair = human_channel();
        let hook = smooth_operator::human::ConfirmationHook::new(
            CONFIRM_TOOLS.iter().map(|s| (*s).to_owned()).collect(),
            pair.request_tx,
            pair.response_rx,
            std::time::Duration::from_secs(5),
        );
        let mut request_rx = pair.request_rx;
        tokio::spawn(async move {
            request_rx.recv().await.expect("a confirm request must be emitted");
            pair.response_tx
                .send(HumanResponse::Denied { reason: "not that one".into() })
                .expect("send verdict");
        });
        let err = hook
            .pre_call(&ToolCall {
                id: "call-1".into(),
                name: "calendar_delete".into(),
                arguments: serde_json::json!({"args": ["EV-1"]}),
            })
            .await
            .expect_err("a denied delete must not run")
            .to_string();
        assert!(err.contains("not that one"), "{err}");
    }

    /// Same contract for the reminders half of pearl th-94cc4a: registered on
    /// macOS whether or not the (separate) Reminders TCC grant exists, so the
    /// agent can relay `th doctor --setup-reminders` instead of reporting an
    /// empty todo list.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn provider_registers_the_reminders_tool_on_macos() {
        use smooth_operator_svc::access_control::AccessContext;
        let provider = local_tool_provider(std::env::temp_dir(), None);
        let ctx = ToolProviderContext::new(Some("org-1".into()), AccessContext::anonymous()).with_conversation_id("conv-1");
        let names: Vec<String> = provider.tools_for(&ctx).await.iter().map(|t| t.schema().name).collect();
        assert!(names.iter().any(|n| n == "reminders"), "reminders registered: {names:?}");
    }

    /// Platform-specific registration (pearl th-1665ed): the Messages tool must
    /// reach the agent on macOS **unconditionally** — including on a box where
    /// Full Disk Access hasn't been granted, because the tool's own answer ("run
    /// `th doctor --setup-imessage`") is the actionable path. If this ever
    /// regresses to a runtime availability gate, Big Smooth goes back to claiming
    /// it can't read or send texts at all.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn provider_registers_the_imessage_tool_on_macos() {
        use smooth_operator_svc::access_control::AccessContext;
        let provider = local_tool_provider(std::env::temp_dir(), None);
        let ctx = ToolProviderContext::new(Some("org-1".into()), AccessContext::anonymous()).with_conversation_id("conv-1");
        let names: Vec<String> = provider.tools_for(&ctx).await.iter().map(|t| t.schema().name).collect();
        assert!(names.iter().any(|n| n == "imessage"), "imessage registered: {names:?}");
    }

    #[tokio::test]
    async fn provider_memory_is_shared_write_visible_to_recall() {
        use smooth_operator_svc::access_control::AccessContext;
        // remember → recall must round-trip through the SAME backend the provider
        // hands both tools. Uses the durable adapter so this also exercises the
        // real store the daemon runs with.
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(crate::operator_storage::SqliteStorageAdapter::open(&dir.path().join("op.db")).unwrap());
        let provider = local_tool_provider_with_memory(SessionCwd::new(std::env::temp_dir()), None, storage.memory());
        let ctx = ToolProviderContext::new(Some("org-1".into()), AccessContext::anonymous()).with_conversation_id("conv-1");
        let tools = provider.tools_for(&ctx).await;
        let remember = tools.iter().find(|t| t.schema().name == "remember").unwrap();
        let recall = tools.iter().find(|t| t.schema().name == "recall").unwrap();
        remember
            .execute(serde_json::json!({"content": "the user's name is Brent", "type": "user"}))
            .await
            .unwrap();
        let out = recall.execute(serde_json::json!({"query": "user name"})).await.unwrap();
        assert!(out.contains("Brent"), "recall surfaces the remembered fact: {out}");
    }

    /// Serializes the tests that mutate the process-global `SMOOAI_GATEWAY_*`
    /// vars. Cargo runs tests in parallel threads of ONE process, so without
    /// this the two gateway tests race — one's `remove_var` can land between the
    /// other's `set_var` and its assertion, failing it intermittently. Poison is
    /// ignored (`into_inner`) so one failing test doesn't cascade into the other.
    static GATEWAY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Holding the guard across the `tools_for` await is the point: the env
    // vars must stay put for the whole call, which is exactly what the lock
    // exists to guarantee. Dropping it early to satisfy the lint would
    // reintroduce the race documented above. No deadlock risk — the runtime
    // is single-threaded and nothing else contends for this lock while awaiting.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn provider_registers_send_sidekick_when_gateway_available() {
        use smooth_operator_svc::access_control::AccessContext;
        let _guard = GATEWAY_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // The subagent-delegation tool (th-1adf55) must reach the agent when a
        // gateway is resolvable, so Big Smooth can fan work out to a sidekick.
        std::env::set_var("SMOOAI_GATEWAY_URL", "http://gateway.test/v1");
        std::env::set_var("SMOOAI_GATEWAY_KEY", "test-key");
        let provider = local_tool_provider(std::env::temp_dir(), None);
        let ctx = ToolProviderContext::new(Some("org-1".into()), AccessContext::anonymous()).with_conversation_id("conv-1");
        let names: Vec<String> = provider.tools_for(&ctx).await.iter().map(|t| t.schema().name).collect();
        assert!(
            names.iter().any(|n| n == "send_sidekick"),
            "send_sidekick delegation tool registered: {names:?}"
        );
        // Didn't clobber the rest of the sandboxed set.
        assert!(names.iter().any(|n| n == "bash"), "bash still registered: {names:?}");
        assert!(names.iter().any(|n| n == "remember"), "remember still registered: {names:?}");
        std::env::remove_var("SMOOAI_GATEWAY_URL");
        std::env::remove_var("SMOOAI_GATEWAY_KEY");
    }

    /// Smoo Jr (ADR-008, th-12d875): a `role:child` principal gets ONLY its
    /// allowlist — no writes, no shell, no egress, and crucially no delegation
    /// (the escape hatch is closed at the same filter).
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn family_child_role_narrows_tools_and_blocks_delegation() {
        use smooth_operator_svc::access_control::AccessContext;
        use smooth_policy::family::FamilyConfig;
        let _guard = GATEWAY_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Gateway resolvable, so send_sidekick WOULD register for an owner — proving
        // its absence for Jr is the filter, not a missing gateway.
        std::env::set_var("SMOOAI_GATEWAY_URL", "http://gateway.test/v1");
        std::env::set_var("SMOOAI_GATEWAY_KEY", "test-key");

        let family = Arc::new(
            FamilyConfig::from_toml(
                r#"
                [roles.child]
                allow = ["read_file", "list_files", "grep", "recall"]
                default = "deny"
            "#,
            )
            .unwrap(),
        );
        let provider = local_tool_provider_full(
            SessionCwd::new(std::env::temp_dir()),
            None,
            Arc::new(smooth_operator::InMemoryMemory::new()),
            None,
            Some(family),
            crate::session_mode::SessionModes::new(),
            None,
        );
        let jr = ToolProviderContext::new(Some("org-1".into()), AccessContext::new(Some("kid".into()), vec!["role:child".into()])).with_conversation_id("c1");
        let names: Vec<String> = provider.tools_for(&jr).await.iter().map(|t| t.schema().name).collect();
        for keep in ["read_file", "list_files", "grep", "recall"] {
            assert!(names.iter().any(|n| n == keep), "Jr keeps {keep}: {names:?}");
        }
        for deny in ["bash", "write_file", "edit_file", "web_search", "crawl", "remember", "send_sidekick"] {
            assert!(!names.iter().any(|n| n == deny), "Jr must NOT get {deny}: {names:?}");
        }
        std::env::remove_var("SMOOAI_GATEWAY_URL");
        std::env::remove_var("SMOOAI_GATEWAY_KEY");
    }

    /// The owner is never filtered; an unknown role gets nothing (fail-closed);
    /// and the tool set depends only on the role, never on message/conversation
    /// (no escalation via a different turn input).
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn family_owner_unfiltered_unknown_denied_and_no_escalation() {
        use smooth_operator_svc::access_control::AccessContext;
        use smooth_policy::family::FamilyConfig;
        let _guard = GATEWAY_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("SMOOAI_GATEWAY_URL", "http://gateway.test/v1");
        std::env::set_var("SMOOAI_GATEWAY_KEY", "test-key");
        let family = Arc::new(
            FamilyConfig::from_toml(
                r#"
                [roles.child]
                allow = ["read_file"]
                default = "deny"
            "#,
            )
            .unwrap(),
        );
        let provider = local_tool_provider_full(
            SessionCwd::new(std::env::temp_dir()),
            None,
            Arc::new(smooth_operator::InMemoryMemory::new()),
            None,
            Some(family),
            crate::session_mode::SessionModes::new(),
            None,
        );

        // Owner: no role group → full set (regression guard).
        let owner = ToolProviderContext::new(Some("org".into()), AccessContext::new(Some("owner".into()), vec![])).with_conversation_id("c");
        let owner_names: Vec<String> = provider.tools_for(&owner).await.iter().map(|t| t.schema().name).collect();
        assert!(owner_names.iter().any(|n| n == "bash"), "owner keeps bash: {owner_names:?}");
        assert!(owner_names.iter().any(|n| n == "send_sidekick"), "owner keeps delegation: {owner_names:?}");

        // Unknown role → deny everything (fail-closed), NOT the full set.
        let bogus = ToolProviderContext::new(Some("org".into()), AccessContext::new(Some("x".into()), vec!["role:bogus".into()])).with_conversation_id("c");
        let bogus_names: Vec<String> = provider.tools_for(&bogus).await.iter().map(|t| t.schema().name).collect();
        assert!(bogus_names.is_empty(), "unknown role gets nothing: {bogus_names:?}");

        // Anti-escalation: same role, different conversation → identical set.
        let jr1 = ToolProviderContext::new(Some("org".into()), AccessContext::new(Some("kid".into()), vec!["role:child".into()])).with_conversation_id("c1");
        let jr2 = ToolProviderContext::new(Some("org".into()), AccessContext::new(Some("kid".into()), vec!["role:child".into()])).with_conversation_id("c2");
        let mut n1: Vec<String> = provider.tools_for(&jr1).await.iter().map(|t| t.schema().name).collect();
        let mut n2: Vec<String> = provider.tools_for(&jr2).await.iter().map(|t| t.schema().name).collect();
        n1.sort();
        n2.sort();
        assert_eq!(n1, n2, "the tool set depends only on the role, not the conversation");
        std::env::remove_var("SMOOAI_GATEWAY_URL");
        std::env::remove_var("SMOOAI_GATEWAY_KEY");
    }

    /// The `notify` tool (th-c29d34): injected only when a sink is wired, present
    /// for the owner, and gated by the deny-by-default family filter — a scoped
    /// role gets it only when its allowlist names `notify`.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn notify_tool_owner_present_child_gated() {
        use smooth_operator_svc::access_control::AccessContext;
        use smooth_policy::family::FamilyConfig;

        struct NoopSink;
        #[async_trait::async_trait]
        impl smooth_tools::NotifySink for NoopSink {
            async fn deliver(&self, _t: &str, _b: &str, _d: Option<&str>) -> usize {
                0
            }
        }

        let _guard = GATEWAY_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let family = Arc::new(
            FamilyConfig::from_toml(
                r#"
                [roles.child]
                allow = ["read_file"]
                default = "deny"

                [roles.teen]
                allow = ["read_file", "notify"]
                default = "deny"
            "#,
            )
            .unwrap(),
        );
        let sink: Arc<dyn smooth_tools::NotifySink> = Arc::new(NoopSink);
        let provider = local_tool_provider_full(
            SessionCwd::new(std::env::temp_dir()),
            None,
            Arc::new(smooth_operator::InMemoryMemory::new()),
            None,
            Some(family),
            crate::session_mode::SessionModes::new(),
            Some(sink),
        );

        let has = |names: &[String], t: &str| names.iter().any(|n| n == t);
        let owner = ToolProviderContext::new(Some("org".into()), AccessContext::new(Some("owner".into()), vec![])).with_conversation_id("c");
        let owner_names: Vec<String> = provider.tools_for(&owner).await.iter().map(|t| t.schema().name).collect();
        assert!(has(&owner_names, "notify"), "owner gets notify: {owner_names:?}");

        let child = ToolProviderContext::new(Some("org".into()), AccessContext::new(Some("kid".into()), vec!["role:child".into()])).with_conversation_id("c");
        let child_names: Vec<String> = provider.tools_for(&child).await.iter().map(|t| t.schema().name).collect();
        assert!(!has(&child_names, "notify"), "child without a notify grant does NOT get it: {child_names:?}");

        let teen = ToolProviderContext::new(Some("org".into()), AccessContext::new(Some("t".into()), vec!["role:teen".into()])).with_conversation_id("c");
        let teen_names: Vec<String> = provider.tools_for(&teen).await.iter().map(|t| t.schema().name).collect();
        assert!(has(&teen_names, "notify"), "teen granted notify gets it: {teen_names:?}");
    }

    /// Plan mode (th-c1b589) is a hard read-only guarantee: a Plan conversation's
    /// per-turn tool set contains ONLY the read-only allowlist (+ present_plan /
    /// todo_write) — never a mutating tool — while an Auto conversation on the
    /// SAME provider keeps the full set. This is the security-critical assertion:
    /// the model cannot obtain `edit_file` / `bash` / `send_file` in Plan mode.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn plan_mode_filters_the_tool_set_to_read_only() {
        use smooth_operator_svc::access_control::AccessContext;
        let modes = crate::session_mode::SessionModes::new();
        modes.set("plan-conv", crate::session_mode::Mode::Plan);
        // "auto-conv" is left unset → Auto.
        let provider = local_tool_provider_full(
            SessionCwd::new(std::env::temp_dir()),
            None,
            Arc::new(smooth_operator::InMemoryMemory::new()),
            None,
            None,
            modes,
            None,
        );

        // A directive sink so present_plan/todo_write are injected this turn.
        let sink = Arc::new(std::sync::Mutex::new(serde_json::Value::Null));
        let mut plan_ctx = ToolProviderContext::new(Some("org".into()), AccessContext::new(Some("owner".into()), vec![])).with_conversation_id("plan-conv");
        plan_ctx.directive_sink = Some(Arc::clone(&sink));
        let plan_names: Vec<String> = provider.tools_for(&plan_ctx).await.iter().map(|t| t.schema().name).collect();

        // Every mutating tool is gone — the model never sees it.
        for mutating in [
            "write_file",
            "edit_file",
            "bash",
            "send_file",
            "create_skill",
            "remember",
            "th",
            "send_sidekick",
        ] {
            assert!(!plan_names.iter().any(|n| n == mutating), "Plan mode must DROP {mutating}: {plan_names:?}");
        }
        // The read-only + planning tools remain.
        for keep in ["read_file", "list_files", "grep", "recall", "present_plan", "todo_write"] {
            assert!(plan_names.iter().any(|n| n == keep), "Plan mode keeps {keep}: {plan_names:?}");
        }
        // Nothing outside the allowlist survived.
        assert!(
            plan_names.iter().all(|n| PLAN_READONLY_TOOLS.contains(&n.as_str())),
            "Plan mode leaves only allowlisted tools: {plan_names:?}"
        );

        // Same provider, an Auto conversation keeps the mutating tools.
        let mut auto_ctx = ToolProviderContext::new(Some("org".into()), AccessContext::new(Some("owner".into()), vec![])).with_conversation_id("auto-conv");
        auto_ctx.directive_sink = Some(sink);
        let auto_names: Vec<String> = provider.tools_for(&auto_ctx).await.iter().map(|t| t.schema().name).collect();
        for m in ["write_file", "edit_file", "bash"] {
            assert!(auto_names.iter().any(|n| n == m), "Auto mode keeps {m}: {auto_names:?}");
        }
    }

    #[test]
    fn gateway_llm_factory_builds_config_from_env_gateway() {
        let _guard = GATEWAY_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // With an env gateway key, the factory resolves a usable LlmConfig for
        // the sidekick's routing slot (the built-ins route to `Coding`).
        std::env::set_var("SMOOAI_GATEWAY_URL", "http://gw.test/v1");
        std::env::set_var("SMOOAI_GATEWAY_KEY", "k-123");
        let factory = gateway_llm_factory().expect("factory built when a gateway key is present");
        let cfg = factory(smooth_operator::providers::Activity::Coding).expect("factory yields a config");
        assert_eq!(cfg.api_url, "http://gw.test/v1");
        assert_eq!(cfg.api_key, "k-123");
        assert!(!cfg.model.is_empty(), "a model is set for the sidekick");
        std::env::remove_var("SMOOAI_GATEWAY_URL");
        std::env::remove_var("SMOOAI_GATEWAY_KEY");
    }

    #[test]
    fn big_smooth_persona_is_personal_and_no_reasoning() {
        let p = BIG_SMOOTH_PERSONA;
        // Identity: a personal assistant named Big Smooth, NOT customer support.
        assert!(p.contains("Big Smooth"), "names the persona");
        assert!(p.contains("personal"), "frames as a personal assistant");
        assert!(p.to_lowercase().contains("not customer support"), "explicitly not support");
        // The firm no-reasoning-narration directive (the core of th-5f059b).
        assert!(p.contains("never show your reasoning"), "forbids reasoning narration");
        assert!(p.contains("never restate"), "forbids restating the question");
        // Does not gratuitously volunteer the org knowledge base.
        assert!(p.contains("unless the user explicitly asks about organization"), "org-knowledge is opt-in");
    }

    // ── deny policy + permission gate ─────────────────────────────────────

    use smooth_operator::permission::{AutoMode, PermissionHook};
    use smooth_operator::tool::{ToolCall, ToolHook};

    fn bash_call(cmd: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": cmd }),
        }
    }

    fn write_call(path: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({ "path": path, "content": "x" }),
        }
    }

    #[test]
    fn deny_policy_toml_parses() {
        // A parse failure would be a build/test error, not a runtime one — this
        // pins that guarantee.
        let policy = default_deny_policy();
        assert!(!policy.is_empty(), "embedded deny policy has rules");
    }

    #[test]
    fn deny_policy_blocks_dangerous_bash() {
        let policy = default_deny_policy();
        // Directly-listed dangerous bins.
        for cmd in [
            "su - root",
            "shutdown -h now",
            "reboot",
            "dd if=/dev/zero of=/dev/disk0",
            "mkfs.ext4 /dev/sda1",
            "launchctl load x",
            "crontab -e",
        ] {
            assert!(policy.evaluate(&bash_call(cmd)).is_some(), "deny policy must block: {cmd}");
        }
        // sudo is stripped by the engine, so a sudo-prefixed dangerous op is
        // caught by the UNDERLYING pattern (reboot), not the `sudo ` pattern.
        assert!(
            policy.evaluate(&bash_call("sudo reboot")).is_some(),
            "sudo <dangerous> blocked via underlying pattern"
        );
    }

    #[test]
    fn deny_policy_blocks_sensitive_path_writes() {
        let policy = default_deny_policy();
        for path in [
            "/etc/hosts",
            "/System/x",
            "/usr/bin/x",
            "/home/me/.ssh/id_rsa",
            "/home/me/.aws/credentials",
            "/home/me/.smooth/auth/smooai.json",
        ] {
            assert!(policy.evaluate(&write_call(path)).is_some(), "deny policy must block write to: {path}");
        }
    }

    #[test]
    fn deny_policy_allows_benign() {
        let policy = default_deny_policy();
        assert!(policy.evaluate(&bash_call("ls -la")).is_none(), "benign bash falls through");
        assert!(policy.evaluate(&bash_call("git status")).is_none(), "git status falls through");
        assert!(policy.evaluate(&write_call("src/main.rs")).is_none(), "benign workspace write falls through");
    }

    #[tokio::test]
    async fn permission_hook_bypass_plus_deny_policy_is_allow_benign_block_dangerous() {
        // The daemon's exact wiring: Bypass mode + the embedded deny policy.
        let hook = PermissionHook::new(AutoMode::Bypass).with_deny_policy(Arc::new(default_deny_policy()));
        // Benign runs (Bypass allows everything past circuit-breakers/deny-policy).
        assert!(hook.pre_call(&bash_call("ls")).await.is_ok(), "benign bash allowed under Bypass");
        assert!(hook.pre_call(&write_call("src/lib.rs")).await.is_ok(), "benign write allowed under Bypass");
        // Deny-policy is circuit-breaker tier — Bypass cannot downgrade it.
        assert!(
            hook.pre_call(&bash_call("shutdown -h now")).await.is_err(),
            "deny policy blocks dangerous op even under Bypass"
        );
        assert!(
            hook.pre_call(&write_call("/etc/hosts")).await.is_err(),
            "deny policy blocks /etc write even under Bypass"
        );
    }

    #[test]
    fn permission_mode_defaults_to_bypass_and_honors_env() {
        std::env::remove_var("SMOOTH_AUTO_MODE");
        assert_eq!(
            permission_mode(),
            AutoMode::Bypass,
            "unset → Bypass: benign runs unprompted; DenyPolicy + Narc gate the dangerous (th-946ba0)"
        );
        std::env::set_var("SMOOTH_AUTO_MODE", "  ");
        assert_eq!(permission_mode(), AutoMode::Bypass, "blank → the default (Bypass)");
        std::env::set_var("SMOOTH_AUTO_MODE", "ask");
        assert_eq!(permission_mode(), AutoMode::Ask, "explicit ask honored");
        std::env::set_var("SMOOTH_AUTO_MODE", "deny");
        assert_eq!(permission_mode(), AutoMode::DenyUnmatched, "explicit deny honored");
        // The stricter category-gating posture is still available on request.
        std::env::set_var("SMOOTH_AUTO_MODE", "accept-edits");
        assert_eq!(permission_mode(), AutoMode::AcceptEdits, "explicit accept-edits honored");
        std::env::set_var("SMOOTH_AUTO_MODE", "bypass");
        assert_eq!(permission_mode(), AutoMode::Bypass, "explicit bypass honored");
        std::env::remove_var("SMOOTH_AUTO_MODE");
    }

    const SAMPLE_SKILL: &str =
        "---\nname: add-show\ndescription: Add a show to the watchlist\ntriggers:\n  - add show\n  - add movie\n---\n\n# add-show\n\nDo the thing.\n";

    /// th-0b488d: the whole point — a signed-in daemon states its org instead
    /// of making the model discover it. The org id must appear literally, since
    /// that is the string the agent will paste into `th api` calls.
    #[test]
    fn identity_section_states_user_and_org_when_signed_in() {
        let s = smoo_identity_section(Some("brent@smoo.ai"), Some("8be5f5fd-cf71-43ba-9df9-01e15acdaf8e"));
        assert!(s.contains("brent@smoo.ai"), "user missing: {s}");
        assert!(s.contains("8be5f5fd-cf71-43ba-9df9-01e15acdaf8e"), "org id missing: {s}");
        assert!(!s.contains("th auth login"), "signed-in persona must not suggest logging in: {s}");
    }

    /// A session with an org but no readable user still gets the org — the org
    /// id is the part org-scoped calls actually need.
    #[test]
    fn identity_section_states_org_without_user() {
        let s = smoo_identity_section(None, Some("org-abc"));
        assert!(s.contains("org-abc"), "org id missing: {s}");
        assert!(!s.contains("th auth login"), "must not claim signed out when an org is known: {s}");
    }

    /// The regression this pearl is about, in reverse. Credentials are read
    /// ONCE at build time, so "no session at startup" must never be asserted to
    /// the user as fact — a mid-run sign-in would make that a confident lie of
    /// exactly the kind th-0b488d filed.
    #[test]
    fn identity_section_does_not_assert_signed_out_as_fact() {
        let s = smoo_identity_section(None, None);
        assert!(s.contains("th auth whoami"), "signed-out branch must send the agent to re-check: {s}");
        let claims_signed_out = s.contains("You are not signed in") || s.contains("you are signed out");
        assert!(!claims_signed_out, "must not state signed-out as fact: {s}");
    }

    /// Blank strings in the credentials file are absence, not a user named "".
    #[test]
    fn identity_section_treats_blank_credentials_as_absent() {
        assert_eq!(smoo_identity_section(Some("   "), Some("")), smoo_identity_section(None, None));
        // A blank user with a real org still yields the org.
        assert!(smoo_identity_section(Some(" "), Some("org-abc")).contains("org-abc"));
    }

    /// The identity block must actually reach the persona the agent runs under
    /// — the bug was ambient knowledge being absent, not prose being unwritten.
    #[test]
    fn persona_carries_the_smoo_identity_section() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let persona = persona_with_skills(tmp.path());
        assert!(persona.contains("## Smoo AI account"), "persona is missing the identity section");
    }

    #[test]
    fn skills_section_indexes_discovered_project_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join(".smooth").join("skills").join("add-show");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), SAMPLE_SKILL).unwrap();

        let persona = persona_with_skills(tmp.path());
        assert!(persona.starts_with(BIG_SMOOTH_PERSONA), "base persona kept as the prefix");
        assert!(persona.contains("# Available skills"), "section header present");
        assert!(persona.contains("add-show"), "skill name listed");
        assert!(persona.contains("Add a show to the watchlist"), "description listed");
        assert!(persona.contains("triggers: add show, add movie"), "triggers listed");
        assert!(persona.contains("read_file"), "instructs read_file for progressive disclosure");
        assert!(persona.contains(&skill_dir.join("SKILL.md").display().to_string()), "SKILL.md path listed");
        // Body must NOT leak into the persona (progressive disclosure).
        assert!(!persona.contains("Do the thing."), "skill body must not be dumped into the persona");
    }

    #[test]
    fn skills_section_is_none_when_no_readable_skills() {
        // Empty slice → nothing to inject.
        assert!(render_skills_section(&[]).is_none());
        // A skill whose SKILL.md isn't a real on-disk file (e.g. the embedded
        // builtin) is dropped — read_file couldn't load it anyway.
        let virtual_skill = smooth_cast::skills::parse_skill_string(
            SAMPLE_SKILL,
            std::path::Path::new("<builtin>/add-show/SKILL.md"),
            smooth_cast::skills::SkillSource::Builtin,
        )
        .unwrap()
        .unwrap();
        assert!(render_skills_section(&[virtual_skill]).is_none(), "virtual/unreadable skills are dropped");
    }

    #[test]
    fn render_skills_section_returns_none_for_empty_so_persona_stays_clean() {
        // The None-branch of persona_with_skills: no readable skills → no section,
        // so the base persona is returned unchanged. Tested hermetically here (the
        // discover() path reads the real ~/.claude / ~/.smooth skill dirs, which
        // aren't controllable in a unit test).
        assert!(render_skills_section(&[]).is_none());
    }

    #[test]
    fn discovery_skips_malformed_skill_but_keeps_valid_one() {
        let tmp = tempfile::tempdir().unwrap();
        // Valid skill (frontmatter name = "good").
        let good = tmp.path().join(".smooth").join("skills").join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(good.join("SKILL.md"), "---\nname: good\ndescription: a valid skill\n---\n\nbody\n").unwrap();
        // Malformed skill — opened frontmatter, never closed. `discover` skips it
        // with a warning instead of crashing the turn.
        let bad = tmp.path().join(".smooth").join("skills").join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("SKILL.md"), "---\nname: bad\ndescription: broken\n\nno close marker").unwrap();

        let skills = smooth_cast::skills::discover(tmp.path());
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"good"), "valid skill survives: {names:?}");
        assert!(!names.contains(&"bad"), "malformed skill is skipped: {names:?}");
    }

    #[test]
    fn gateway_from_providers_reads_legacy_smooth_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(
            &path,
            r#"{"providers":[
                {"id":"anthropic","api_url":"https://api.anthropic.com","api_key":"sk-ant"},
                {"id":"smooth","api_url":"https://llm.smoo.ai/v1","api_key":"sk-smooth","default_model":"m-default"}
            ],"routing":{"coding":{"provider":"smooth","model":"m-coding"},"fast":{"provider":"smooth","model":"m-fast"}}}"#,
        )
        .unwrap();
        let (url, key, model) = gateway_from_providers_at(&path, "coding").expect("smooth provider resolves");
        assert_eq!(url, "https://llm.smoo.ai/v1");
        assert_eq!(key, "sk-smooth");
        assert_eq!(model, "m-coding", "the coding route wins over default_model");
        // Fast mode picks the `fast` slot.
        let (_, _, fast_model) = gateway_from_providers_at(&path, "fast").expect("fast route resolves");
        assert_eq!(fast_model, "m-fast", "the fast route selects the fast model");
        // An unknown route falls back to the coding slot, then default.
        let (_, _, fallback) = gateway_from_providers_at(&path, "nonexistent").expect("falls back");
        assert_eq!(fallback, "m-coding", "unknown route falls back to coding");
    }

    /// The id every current writer of providers.json stamps. Matching a
    /// hardcoded `"smooth"` made this resolve `None` for every new user —
    /// the daemon then ran on engine defaults with no key (pearl th-6062ea).
    #[test]
    fn gateway_from_providers_reads_smooai_gateway_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(
            &path,
            r#"{"providers":[
                {"id":"smooai-gateway","api_url":"https://llm.smoo.ai/v1","api_key":"sg-key","default_model":"m-default"}
            ],"routing":{"coding":{"provider":"smooai-gateway","model":"m-coding"},"fast":{"provider":"smooai-gateway","model":"m-fast"}}}"#,
        )
        .unwrap();
        let (url, key, model) = gateway_from_providers_at(&path, "coding").expect("smooai-gateway resolves");
        assert_eq!(url, "https://llm.smoo.ai/v1");
        assert_eq!(key, "sg-key");
        assert_eq!(model, "m-coding");
    }

    /// BYO providers route through the same path — the slot's `provider` id is
    /// the lookup key, so a self-hosted ollama/vllm/openai works too.
    #[test]
    fn gateway_from_providers_resolves_byo_and_per_slot_providers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(
            &path,
            r#"{"providers":[
                {"id":"ollama","api_url":"http://localhost:11434/v1","api_key":"ollama","default_model":"llama3.3"},
                {"id":"openai","api_url":"https://api.openai.com/v1","api_key":"sk-oai"}
            ],"routing":{"coding":{"provider":"ollama","model":"qwen3"},"fast":{"provider":"openai","model":"gpt-4o-mini"}}}"#,
        )
        .unwrap();
        let (url, key, model) = gateway_from_providers_at(&path, "coding").expect("ollama resolves");
        assert_eq!((url.as_str(), key.as_str(), model.as_str()), ("http://localhost:11434/v1", "ollama", "qwen3"));
        // Per-slot providers differ: the `fast` slot picks openai, not coding's ollama.
        let (url, key, model) = gateway_from_providers_at(&path, "fast").expect("fast slot resolves its own provider");
        assert_eq!(
            (url.as_str(), key.as_str(), model.as_str()),
            ("https://api.openai.com/v1", "sk-oai", "gpt-4o-mini")
        );
    }

    /// A slot naming a provider that isn't registered falls back to the
    /// `coding` slot's provider rather than giving up.
    #[test]
    fn gateway_from_providers_falls_back_when_slot_provider_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(
            &path,
            r#"{"providers":[{"id":"openai","api_url":"https://api.openai.com/v1","api_key":"sk-oai"}],
                "routing":{"coding":{"provider":"openai","model":"gpt-4o"},"fast":{"provider":"ghost","model":"gpt-4o-mini"}}}"#,
        )
        .unwrap();
        let (url, key, model) = gateway_from_providers_at(&path, "fast").expect("falls back to coding's provider");
        assert_eq!(
            (url.as_str(), key.as_str(), model.as_str()),
            ("https://api.openai.com/v1", "sk-oai", "gpt-4o-mini")
        );
    }

    /// No routing at all (or routing that names nothing registered) still works
    /// when there's exactly one provider to pick.
    #[test]
    fn gateway_from_providers_uses_sole_provider_without_routing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(
            &path,
            r#"{"providers":[{"id":"anything","api_url":"https://x/v1","api_key":"k","default_model":"m-default"}]}"#,
        )
        .unwrap();
        let (url, key, model) = gateway_from_providers_at(&path, "coding").expect("sole provider resolves");
        assert_eq!((url.as_str(), key.as_str(), model.as_str()), ("https://x/v1", "k", "m-default"));
    }

    /// Ambiguity is NOT guessed: several providers with no usable routing → None.
    #[test]
    fn gateway_from_providers_none_when_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(
            &path,
            r#"{"providers":[{"id":"a","api_url":"https://a","api_key":"ka"},{"id":"b","api_url":"https://b","api_key":"kb"}]}"#,
        )
        .unwrap();
        assert!(gateway_from_providers_at(&path, "coding").is_none());
    }

    #[test]
    fn fast_mode_enabled_parses_truthiness() {
        for (val, want) in [
            ("1", true),
            ("true", true),
            ("on", true),
            ("yes", true),
            ("0", false),
            ("false", false),
            ("off", false),
            ("", false),
        ] {
            std::env::set_var("SMOOTH_FAST_MODE", val);
            assert_eq!(fast_mode_enabled(), want, "SMOOTH_FAST_MODE={val:?}");
        }
        std::env::remove_var("SMOOTH_FAST_MODE");
        assert!(!fast_mode_enabled(), "unset is disabled");
    }

    #[test]
    fn model_override_trims_and_filters_empty() {
        std::env::set_var("SMOOTH_AGENT_MODEL", "  groq-gpt-oss-20b  ");
        assert_eq!(model_override().as_deref(), Some("groq-gpt-oss-20b"));
        std::env::set_var("SMOOTH_AGENT_MODEL", "   ");
        assert_eq!(model_override(), None, "blank override is ignored");
        std::env::remove_var("SMOOTH_AGENT_MODEL");
        assert_eq!(model_override(), None);
    }

    #[test]
    fn gateway_config_gives_assistant_headroom_unless_env_overrides() {
        // Unset → assistant-grade headroom, NOT the 512/6 widget defaults that
        // starve a reasoning model into empty replies (pearl th-4b1867).
        std::env::remove_var("SMOOTH_AGENT_MAX_TOKENS");
        std::env::remove_var("SMOOTH_AGENT_MAX_ITERATIONS");
        let cfg = resolve_gateway_config();
        assert_eq!(cfg.max_tokens, 32_768);
        assert_eq!(cfg.max_iterations, 50);

        // Explicit env wins.
        std::env::set_var("SMOOTH_AGENT_MAX_TOKENS", "1024");
        std::env::set_var("SMOOTH_AGENT_MAX_ITERATIONS", "3");
        let cfg = resolve_gateway_config();
        assert_eq!(cfg.max_tokens, 1024);
        assert_eq!(cfg.max_iterations, 3);
        std::env::remove_var("SMOOTH_AGENT_MAX_TOKENS");
        std::env::remove_var("SMOOTH_AGENT_MAX_ITERATIONS");
    }

    #[test]
    fn gateway_from_providers_none_when_no_key_or_provider() {
        let dir = tempfile::tempdir().unwrap();
        // Routed to a provider that isn't registered, and more than one to
        // choose from — nothing to resolve.
        let p1 = dir.path().join("a.json");
        std::fs::write(
            &p1,
            r#"{"providers":[{"id":"anthropic","api_url":"x","api_key":"k"},{"id":"openai","api_url":"y","api_key":"k"}],
                "routing":{"coding":{"provider":"ghost","model":"m"}}}"#,
        )
        .unwrap();
        assert!(gateway_from_providers_at(&p1, "coding").is_none());
        // Provider resolves but the key is blank — a keyless entry is not usable
        // credentials, so the daemon must NOT adopt it.
        let p2 = dir.path().join("b.json");
        std::fs::write(&p2, r#"{"providers":[{"id":"smooai-gateway","api_url":"x","api_key":"   "}]}"#).unwrap();
        assert!(gateway_from_providers_at(&p2, "coding").is_none());
        // No providers at all.
        let p3 = dir.path().join("c.json");
        std::fs::write(&p3, r#"{"providers":[]}"#).unwrap();
        assert!(gateway_from_providers_at(&p3, "coding").is_none());
        // Garbage / wrong shape.
        let p4 = dir.path().join("d.json");
        std::fs::write(&p4, "not json at all").unwrap();
        assert!(gateway_from_providers_at(&p4, "coding").is_none());
        let p5 = dir.path().join("e.json");
        std::fs::write(&p5, r#"{"providers":{"id":"smooai-gateway"}}"#).unwrap();
        assert!(gateway_from_providers_at(&p5, "coding").is_none());
        // Missing file.
        assert!(gateway_from_providers_at(&dir.path().join("nope.json"), "coding").is_none());
    }

    /// Serializes the tests that mutate the process-global `SMOOTH_LOCAL_TOKEN`
    /// (and `HOME`) vars, same reason as `GATEWAY_ENV_LOCK`: cargo runs tests in
    /// parallel threads of ONE process, so without this `provision_prefers_env_token`'s
    /// `set_var` can leak into `provision_generates_and_persists_when_unset`, which
    /// then takes the env branch and never writes the token file — failing its
    /// `.exists()` assertion. This is the flake that reddened Release CI (th-d9dbd7);
    /// it only showed there because the release job's scheduling exposed the race.
    static TOKEN_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn provision_prefers_env_token() {
        let _guard = TOKEN_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("SMOOTH_LOCAL_TOKEN", "  env-tok-123  ");
        assert_eq!(provision_local_token().unwrap(), "env-tok-123", "env token wins, trimmed");
        std::env::remove_var("SMOOTH_LOCAL_TOKEN");
    }

    /// Uses an explicit path rather than redirecting the home directory: `HOME`
    /// steers `dirs_next` on Unix but not on Windows (`SHGetKnownFolderPath`
    /// ignores the environment), so the env-based version of this test wrote a
    /// token into the real user profile there and asserted against a tempdir
    /// that never got one.
    #[test]
    fn provision_generates_and_persists_when_unset() {
        let _guard = TOKEN_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::remove_var("SMOOTH_LOCAL_TOKEN");
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".smooth").join("operator-token");

        let first = provision_local_token_at(&path).unwrap();
        assert!(!first.is_empty(), "a token is generated");
        // The same token is returned on the next call (persisted, not regenerated).
        let second = provision_local_token_at(&path).unwrap();
        assert_eq!(first, second, "token persists across calls");
        assert!(path.exists(), "the token is persisted at the requested path");
    }

    /// `token_path` must land under the real home on every platform — the
    /// tempdir test above deliberately bypasses it, so this covers the wiring.
    #[test]
    fn persist_daemon_addr_writes_bound_addr_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = super::persist_daemon_addr_to(dir.path(), "127.0.0.1:8788").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "127.0.0.1:8788");
        assert!(path.ends_with("daemon.addr"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn persist_daemon_addr_creates_missing_dir_and_overwrites() {
        let base = tempfile::tempdir().unwrap();
        let nested = base.path().join("does/not/exist/.smooth");
        super::persist_daemon_addr_to(&nested, "127.0.0.1:8787").unwrap();
        // A later run on a different port overwrites cleanly.
        let path = super::persist_daemon_addr_to(&nested, "127.0.0.1:9999").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "127.0.0.1:9999");
    }

    #[test]
    fn token_path_is_under_the_home_dot_smooth() {
        let p = token_path();
        assert!(p.ends_with(".smooth/operator-token"), "{}", p.display());
        if let Some(home) = dirs_next::home_dir() {
            assert!(p.starts_with(&home), "{} must resolve under {}", p.display(), home.display());
        }
    }
}
