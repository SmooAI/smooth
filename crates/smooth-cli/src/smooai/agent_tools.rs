//! `th api agents tools …` — per-agent tool enablement.
//!
//! Enabling one tool on one agent used to mean round-tripping the whole
//! `AgentToolConfig` through `th api agents update --tool-config`: read →
//! parse → append → write back, a lost-update race that made raw SQL feel
//! easier. It is also how `verify_identity` shipped invisible — an agent whose
//! `enabledTools` was non-empty silently excluded every tool not on the list,
//! and nothing anywhere would tell you which tools an agent was missing.
//!
//! So: the merge lives on the server (`POST …/tools/{tool_id}` flips exactly
//! one entry, atomically), and this module never rewrites the array. What it
//! adds on top is the part a human needs — the **drift view**: enabled vs.
//! available vs. *missing*, plus a plain statement of whether the agent is
//! restricted at all, because "empty = every tool, non-empty = allowlist" was
//! documented only in a `--help` string.
//!
//! Endpoints (org-scoped, like every other agents route):
//! - `GET  /organizations/{org}/agents/tools/registry`
//! - `GET  /organizations/{org}/agents/{agent_id}/tools`
//! - `POST /organizations/{org}/agents/{agent_id}/tools/{tool_id}`

use std::collections::BTreeSet;

use anstream::println;
use anyhow::{Context, Result};
use clap::{Subcommand, ValueEnum};
use owo_colors::OwoColorize;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum ToolsCmd {
    /// Show which tools this agent has, and — the point — which available
    /// tools it is MISSING. Says plainly whether the agent is restricted.
    List {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the rendered view.
        #[arg(long)]
        json: bool,
    },
    /// List every tool an agent COULD be given: id, description, default auth
    /// level, required product feature, and whether it self-scopes to the
    /// end user. You cannot enable what you cannot enumerate.
    Registry {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the rendered view.
        #[arg(long)]
        json: bool,
    },
    /// Turn ONE tool on for ONE agent. The merge happens server-side.
    ///
    /// On an UNRESTRICTED agent (mode `all`) this is a no-op — the tool is
    /// already enabled implicitly, and writing a one-entry allowlist is
    /// exactly the accident that hid `verify_identity` in production.
    Enable {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// Tool id from `th api agents tools registry` (snake_case).
        tool_id: String,
        /// Auth level required before the tool may execute. Defaults to the
        /// tool's `defaultAuthLevel`. Rejected with 409 on an unrestricted
        /// agent: persisting a per-tool auth level needs an entry, and an
        /// entry restricts the agent.
        #[arg(long = "auth-level", value_enum)]
        auth_level: Option<AuthLevelArg>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the rendered view.
        #[arg(long)]
        json: bool,
    },
    /// Turn ONE tool off for ONE agent. The merge happens server-side.
    ///
    /// On an UNRESTRICTED agent this materialises the allowlist (everything
    /// available, minus this tool) — the agent stops picking up newly shipped
    /// tools from then on. The command says so.
    Disable {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// Tool id from `th api agents tools list` (snake_case).
        tool_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the rendered view.
        #[arg(long)]
        json: bool,
    },
}

/// `AuthLevel` in the smooai agent schema. Values are snake_case on the wire;
/// clap would kebab-case `EndUser` on its own, so name it explicitly and keep
/// the kebab spelling as an alias for muscle memory.
#[derive(Clone, Copy, ValueEnum)]
pub enum AuthLevelArg {
    /// No verification — product search, knowledge base.
    None,
    /// End-user identity verification — order history, account data.
    #[value(name = "end_user", alias = "end-user")]
    EndUser,
    /// Org member authentication — CRM, analytics, all-customer data.
    Admin,
}

impl AuthLevelArg {
    const fn api_value(self) -> &'static str {
        match self {
            AuthLevelArg::None => "none",
            AuthLevelArg::EndUser => "end_user",
            AuthLevelArg::Admin => "admin",
        }
    }
}

// ---- Wire shapes -----------------------------------------------------------
//
// Fields are `#[serde(default)]` on purpose: these endpoints are new, and a CLI
// that hard-fails on an added or renamed optional field is worse than one that
// renders what it understood. `toolId` is the one field we insist on.

/// One entry of `GET …/agents/tools/registry` — a tool that exists in the
/// runtime registry and could be granted to an agent.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryTool {
    pub tool_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `builtin` | `integration`.
    #[serde(default)]
    pub category: Option<String>,
    /// `none` | `end_user` | `admin` — what the tool asks for by default.
    #[serde(default)]
    pub default_auth_level: Option<String>,
    /// Product feature the org must be entitled to, if any.
    #[serde(default)]
    pub required_feature: Option<String>,
    /// Tool scopes itself to the verified end user (`END_USER_SAFE_TOOLS`).
    #[serde(default)]
    pub self_scoping: bool,
    /// Channels the tool works on. A tool listing every channel the registry
    /// knows about is unrestricted, so only a *narrower* set is worth showing.
    #[serde(default)]
    pub channels: Vec<String>,
}

/// One effective entry of `enabled` in the per-agent view.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnabledTool {
    pub tool_id: String,
    #[serde(default)]
    pub auth_level: Option<String>,
}

/// `GET …/agents/{agent_id}/tools` — the drift view.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolsView {
    /// `all` ⇒ no enabled entry ⇒ the agent gets every available tool.
    /// `restricted` ⇒ the list is an allowlist and `missing` is excluded.
    #[serde(default)]
    pub mode: String,
    /// The EFFECTIVE set: in `all`, every available tool at its default auth
    /// level; in `restricted`, the configured entries.
    #[serde(default)]
    pub enabled: Vec<EnabledTool>,
    /// Tool ids whose `isAvailable(orgId)` is true right now.
    #[serde(default)]
    pub available: Vec<String>,
    /// `available − enabled`. Always empty in mode `all`. The drift view: a
    /// newly shipped tool shows up here on day one instead of being silently
    /// absent from production.
    #[serde(default)]
    pub missing: Vec<String>,
    /// Present only on a mutation response.
    #[serde(default)]
    pub change: Option<ToolChange>,
}

impl AgentToolsView {
    /// The agent's `enabledTools` is a non-empty allowlist.
    #[must_use]
    pub fn is_restricted(&self) -> bool {
        self.mode == "restricted"
    }
}

/// What a `POST …/tools/{tool_id}` actually did.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolChange {
    #[serde(default)]
    pub tool_id: String,
    /// `false` when the write was a no-op (enable on an unrestricted agent).
    #[serde(default)]
    pub applied: bool,
    #[serde(default)]
    pub previous_mode: String,
    /// The call flipped the agent from "every tool" to an allowlist. This is
    /// the trap the pearl is about — always worth a loud warning.
    #[serde(default)]
    pub became_restricted: bool,
    /// Tools the agent lost as a side effect of that flip.
    #[serde(default)]
    pub dropped_tools: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

fn parse_registry(raw: &Value) -> Result<Vec<RegistryTool>> {
    serde_json::from_value(raw.get("tools").cloned().unwrap_or_else(|| raw.clone())).context("parse agent tool registry")
}

fn parse_agent_tools(raw: &Value) -> Result<AgentToolsView> {
    serde_json::from_value(raw.clone()).context("parse agent tools view")
}

// ---- Rendering -------------------------------------------------------------

/// First sentence of a description, capped — enough to explain the tool, short
/// enough that a 40-tool catalog stays readable. Empty in, empty out.
fn summarize(description: &str) -> String {
    /// One number, so the truncation and the ellipsis can never disagree —
    /// as two separate `64` literals, they silently could.
    const GIST_MAX: usize = 64;
    let first = description.split(['\n', '.']).map(str::trim).find(|s| !s.is_empty()).unwrap_or_default();
    if first.is_empty() {
        return String::new();
    }
    let mut s: String = first.chars().take(GIST_MAX).collect();
    if first.chars().count() > GIST_MAX {
        s.push('…');
    }
    format!(" — {s}")
}

/// Every channel any tool in this registry declares. A tool listing all of
/// them is channel-agnostic, so comparing against this union — rather than a
/// hardcoded count — keeps the "voice only" hint correct as channels are added.
fn channel_universe(tools: &[RegistryTool]) -> BTreeSet<&str> {
    tools.iter().flat_map(|t| t.channels.iter().map(String::as_str)).collect()
}

fn tool_flags(t: &RegistryTool, universe: &BTreeSet<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(a) = t.default_auth_level.as_deref() {
        parts.push(format!("auth:{a}"));
    }
    if let Some(f) = t.required_feature.as_deref() {
        parts.push(format!("feature:{f}"));
    }
    if t.self_scoping {
        parts.push("self-scoping".to_string());
    }
    let channels: BTreeSet<&str> = t.channels.iter().map(String::as_str).collect();
    if !channels.is_empty() && channels.len() < universe.len() {
        parts.push(format!("channels:{}", t.channels.join(",")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" [{}]", parts.join("; "))
    }
}

/// Render the registry as compact text (shared by the CLI and the MCP tool).
#[must_use]
pub fn render_registry(tools: &[RegistryTool]) -> String {
    use std::fmt::Write as _;
    let universe = channel_universe(tools);
    let mut out = format!("{} registerable tool(s)\n", tools.len());
    for t in tools {
        let _ = writeln!(out, "  {}{}{}", t.tool_id, tool_flags(t, &universe), summarize(&t.description));
    }
    out.trim_end().to_string()
}

/// The sentence that was previously buried in a `--help` string. This is the
/// trap: a non-empty `enabledTools` is an ALLOWLIST, so a tool shipped after
/// the list was written is invisible to the agent until someone adds it.
#[must_use]
pub fn restriction_banner(view: &AgentToolsView) -> String {
    if view.is_restricted() {
        // "The 0 tool(s) under MISSING are NOT available" reads as nonsense on
        // a fully-enabled agent, and this is the one line that has to land.
        if view.missing.is_empty() {
            "RESTRICTED — enabledTools is a non-empty ALLOWLIST. Nothing is missing right now, but any tool shipped from now on is invisible to this agent until it is enabled here.".to_string()
        } else {
            format!(
                "RESTRICTED — enabledTools is a non-empty ALLOWLIST. The {} tool(s) under MISSING are NOT available to this agent, and neither is any tool shipped from now on until it is enabled here.",
                view.missing.len()
            )
        }
    } else {
        format!(
            "UNRESTRICTED — enabledTools has no enabled entry, so all {} tool(s) available to this org are in play, including anything shipped later. Materialising a list (any `disable`) ends that.",
            view.available.len()
        )
    }
}

/// Render the per-agent drift view (shared by the CLI and the MCP tool).
/// `registry` is used only to annotate ids with description/auth/feature —
/// pass an empty slice for bare ids.
#[must_use]
pub fn render_agent_tools(view: &AgentToolsView, registry: &[RegistryTool]) -> String {
    use std::fmt::Write as _;
    let universe = channel_universe(registry);
    let lookup = |id: &str| registry.iter().find(|t| t.tool_id == id);

    let mut out = format!(
        "{} enabled, {} missing, {} available to this org\n{}\n",
        view.enabled.len(),
        view.missing.len(),
        view.available.len(),
        restriction_banner(view)
    );

    let _ = writeln!(out, "\nENABLED ({})", view.enabled.len());
    if view.enabled.is_empty() {
        let _ = writeln!(out, "  (none)");
    }
    for t in &view.enabled {
        let auth = t.auth_level.as_deref().map(|a| format!(" [auth:{a}]")).unwrap_or_default();
        // A toolId with no registry entry binds to nothing at runtime
        // (SMOODEV-981): the agent reads as configured and has no tool.
        let gist = match lookup(&t.tool_id) {
            Some(r) => summarize(&r.description),
            None if registry.is_empty() => String::new(),
            None => "  ⚠ NOT IN REGISTRY — binds to nothing at runtime".to_string(),
        };
        let _ = writeln!(out, "  {}{auth}{gist}", t.tool_id);
    }

    let _ = writeln!(out, "\nMISSING ({}) — available to this org, not enabled on this agent", view.missing.len());
    if view.missing.is_empty() {
        let _ = writeln!(out, "  (none)");
    }
    for id in &view.missing {
        match lookup(id) {
            Some(r) => {
                let _ = writeln!(out, "  {id}{}{}", tool_flags(r, &universe), summarize(&r.description));
            }
            None => {
                let _ = writeln!(out, "  {id}");
            }
        }
    }
    out.trim_end().to_string()
}

/// What a mutation did, in words — including the loud warning when the call
/// turned an unrestricted agent into an allowlist.
#[must_use]
pub fn render_change(change: &ToolChange) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if !change.applied {
        let _ = writeln!(
            out,
            "{} no change written — the agent is UNRESTRICTED, so `{}` is already enabled implicitly. Writing a one-entry allowlist here is what hid verify_identity in production.",
            "NO-OP:".cyan().bold(),
            change.tool_id
        );
    }
    if change.became_restricted {
        let _ = writeln!(
            out,
            "{} this agent was UNRESTRICTED (every available tool, including future ones) and is now an ALLOWLIST. It will NOT pick up newly shipped tools — re-check it with `th api agents tools list` after every release.",
            "WARNING:".yellow().bold(),
        );
    }
    if !change.dropped_tools.is_empty() {
        let _ = writeln!(out, "{} {}", "DROPPED:".red().bold(), change.dropped_tools.join(", "));
    }
    if let Some(n) = change.note.as_deref().filter(|n| !n.is_empty()) {
        let _ = writeln!(out, "server: {n}");
    }
    out.trim_end().to_string()
}

// ---- API calls -------------------------------------------------------------

/// Every registerable tool for the org.
///
/// # Errors
/// Errors without a Smoo session, or if the request fails.
pub async fn fetch_registry(org: Option<String>) -> Result<Vec<RegistryTool>> {
    let client = require_authed().await?;
    let org = require_active_org(&client, org)?;
    let raw = client
        .get(&format!("/organizations/{org}/agents/tools/registry"))
        .await
        .context("GET agent tool registry")?;
    parse_registry(&raw)
}

/// One agent's enabled/available/missing view.
///
/// # Errors
/// Errors without a Smoo session, or if the request fails.
pub async fn fetch_agent_tools(org: Option<String>, agent_id: &str) -> Result<AgentToolsView> {
    let client = require_authed().await?;
    let org = require_active_org(&client, org)?;
    let raw = client
        .get(&format!("/organizations/{org}/agents/{agent_id}/tools"))
        .await
        .context("GET agent tools")?;
    parse_agent_tools(&raw)
}

/// Flip ONE tool on one agent. The server owns the merge — this never sends
/// the whole array, which is the read-modify-write race the pearl is about.
///
/// # Errors
/// Errors without a Smoo session, on an unknown tool/agent (404), on
/// `--auth-level` against an unrestricted agent (409), or if the request fails.
pub async fn set_agent_tool(org: Option<String>, agent_id: &str, tool_id: &str, enabled: bool, auth_level: Option<&str>) -> Result<AgentToolsView> {
    let client = require_authed().await?;
    let org = require_active_org(&client, org)?;
    let mut body = json!({ "enabled": enabled });
    if let Some(a) = auth_level {
        body["authLevel"] = json!(a);
    }
    let raw = client
        .post(&format!("/organizations/{org}/agents/{agent_id}/tools/{tool_id}"), Some(&body))
        .await
        .context("POST agent tool")?;
    parse_agent_tools(&raw)
}

// ---- Dispatch --------------------------------------------------------------

/// # Errors
/// Errors if an API call fails or a response can't be parsed.
pub async fn cmd(cmd: ToolsCmd) -> Result<()> {
    match cmd {
        ToolsCmd::Registry { org, json: json_out } => {
            let tools = fetch_registry(org).await?;
            if json_out {
                print_json(&json!({ "tools": tools.iter().map(registry_json).collect::<Vec<_>>() }));
            } else {
                println!("{}", render_registry(&tools));
            }
        }
        ToolsCmd::List { agent_id, org, json: json_out } => {
            let view = fetch_agent_tools(org.clone(), &agent_id).await?;
            if json_out {
                print_json(&view_json(&view));
            } else {
                // The view returns bare ids; the registry is what makes the
                // MISSING list actionable (what IS this tool, what does it
                // need). Read-only join — no merge logic lives here.
                let registry = fetch_registry(org).await.unwrap_or_default();
                println!("{}", render_agent_tools(&view, &registry));
            }
        }
        ToolsCmd::Enable {
            agent_id,
            tool_id,
            auth_level,
            org,
            json: json_out,
        } => {
            let view = set_agent_tool(org.clone(), &agent_id, &tool_id, true, auth_level.map(AuthLevelArg::api_value)).await?;
            report(&view, org, &agent_id, &tool_id, "enabled", json_out).await;
        }
        ToolsCmd::Disable {
            agent_id,
            tool_id,
            org,
            json: json_out,
        } => {
            let view = set_agent_tool(org.clone(), &agent_id, &tool_id, false, None).await?;
            report(&view, org, &agent_id, &tool_id, "disabled", json_out).await;
        }
    }
    Ok(())
}

/// Print the outcome of a mutation: what the server did, any warning, then the
/// fresh drift view (which the mutation response already carries).
async fn report(view: &AgentToolsView, org: Option<String>, agent_id: &str, tool_id: &str, verb: &str, json_out: bool) {
    if json_out {
        print_json(&view_json(view));
        return;
    }
    let applied = view.change.as_ref().is_none_or(|c| c.applied);
    if applied {
        let colored = if verb == "enabled" {
            verb.green().to_string()
        } else {
            verb.yellow().to_string()
        };
        println!("{colored} {} on {agent_id}", tool_id.bold());
    }
    if let Some(c) = view.change.as_ref() {
        let rendered = render_change(c);
        if !rendered.is_empty() {
            println!("{rendered}");
        }
    }
    let registry = fetch_registry(org).await.unwrap_or_default();
    println!("{}", render_agent_tools(view, &registry));
}

fn registry_json(t: &RegistryTool) -> Value {
    json!({
        "toolId": t.tool_id,
        "name": t.name,
        "description": t.description,
        "category": t.category,
        "defaultAuthLevel": t.default_auth_level,
        "requiredFeature": t.required_feature,
        "selfScoping": t.self_scoping,
        "channels": t.channels,
    })
}

fn view_json(v: &AgentToolsView) -> Value {
    let mut out = json!({
        "mode": v.mode,
        // The trap, in the machine-readable output too — an agent reading this
        // JSON should not have to know the empty/non-empty rule to be safe.
        "restrictionNote": restriction_banner(v),
        "enabled": v.enabled.iter().map(|t| json!({ "toolId": t.tool_id, "authLevel": t.auth_level })).collect::<Vec<_>>(),
        "available": v.available,
        "missing": v.missing,
    });
    if let Some(c) = v.change.as_ref() {
        out["change"] = json!({
            "toolId": c.tool_id,
            "applied": c.applied,
            "previousMode": c.previous_mode,
            "becameRestricted": c.became_restricted,
            "droppedTools": c.dropped_tools,
            "note": c.note,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(raw: Value) -> AgentToolsView {
        parse_agent_tools(&raw).unwrap()
    }

    fn registry(raw: Value) -> Vec<RegistryTool> {
        parse_registry(&raw).unwrap()
    }

    /// The registry as tools-api specified it, with the two tools from the
    /// incident: one the agent has, one it silently didn't.
    fn incident_registry() -> Vec<RegistryTool> {
        registry(json!({ "tools": [
            {
                "toolId": "knowledge_search", "name": "Knowledge Search",
                "description": "Search the org knowledge base. Returns cited passages.",
                "category": "builtin", "defaultAuthLevel": "none", "requiredFeature": null,
                "selfScoping": false, "channels": ["voice","sms","email","web"],
            },
            {
                "toolId": "transfer_call", "name": "Transfer Call",
                "description": "Hand the live call to a human.",
                "category": "builtin", "defaultAuthLevel": "none", "requiredFeature": null,
                "selfScoping": false, "channels": ["voice"],
            },
            {
                "toolId": "verify_identity", "name": "Verify Identity",
                "description": "Verify a caller's identity with a one-time code. Sends an SMS.",
                "category": "builtin", "defaultAuthLevel": "end_user", "requiredFeature": "telephony",
                "selfScoping": true, "channels": ["voice","sms","email","web"],
            },
        ]}))
    }

    #[test]
    fn registry_parses_envelope_or_bare_array() {
        let enveloped = parse_registry(&json!({ "tools": [{ "toolId": "knowledge_search" }] })).unwrap();
        let bare = parse_registry(&json!([{ "toolId": "knowledge_search" }])).unwrap();
        assert_eq!(enveloped.len(), 1);
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].tool_id, "knowledge_search");
    }

    #[test]
    fn registry_render_hides_channels_only_when_tool_is_channel_agnostic() {
        // transfer_call is voice-only and must say so; knowledge_search spans
        // every channel the registry knows and must not add noise.
        let out = render_registry(&incident_registry());
        assert!(out.contains("transfer_call [auth:none; channels:voice]"), "{out}");
        assert!(out.contains("knowledge_search [auth:none] — Search the org knowledge base"), "{out}");
        assert!(!out.contains("knowledge_search [auth:none; channels"), "{out}");
    }

    #[test]
    fn unrestricted_banner_says_future_tools_are_included() {
        let v = view(json!({ "mode": "all", "enabled": [{"toolId":"a"},{"toolId":"b"}], "available": ["a","b"], "missing": [] }));
        let b = restriction_banner(&v);
        assert!(b.starts_with("UNRESTRICTED"), "{b}");
        assert!(b.contains("all 2 tool(s) available"), "{b}");
        assert!(b.contains("including anything shipped later"), "{b}");
    }

    #[test]
    fn restricted_banner_names_the_allowlist_trap() {
        let v = view(json!({
            "mode": "restricted",
            "enabled": [{ "toolId": "knowledge_search", "authLevel": "none" }],
            "available": ["knowledge_search", "verify_identity"],
            "missing": ["verify_identity"],
        }));
        let b = restriction_banner(&v);
        assert!(b.starts_with("RESTRICTED"), "{b}");
        assert!(b.contains("ALLOWLIST"), "{b}");
        assert!(b.contains("1 tool(s) under MISSING"), "{b}");
        assert!(b.contains("neither is any tool shipped from now on"), "{b}");
    }

    #[test]
    fn restricted_banner_with_nothing_missing_still_warns_about_future_tools() {
        // A fully-enabled allowlist is still an allowlist: the next tool we
        // ship is invisible to it. Saying "0 tool(s) are NOT available" would
        // read as "all good".
        let v = view(json!({ "mode": "restricted", "enabled": [{"toolId":"a"}], "available": ["a"], "missing": [] }));
        let b = restriction_banner(&v);
        assert!(b.starts_with("RESTRICTED"), "{b}");
        assert!(b.contains("Nothing is missing right now"), "{b}");
        assert!(b.contains("invisible to this agent"), "{b}");
        assert!(!b.contains("0 tool(s)"), "{b}");
    }

    #[test]
    fn drift_view_annotates_the_missing_tool_from_the_registry() {
        // The regression this command exists for: verify_identity is
        // registered, available and deployed — but absent from the allowlist.
        let v = view(json!({
            "mode": "restricted",
            "enabled": [{ "toolId": "knowledge_search", "authLevel": "none" }],
            "available": ["knowledge_search", "verify_identity"],
            "missing": ["verify_identity"],
        }));
        let out = render_agent_tools(&v, &incident_registry());
        assert!(out.contains("1 enabled, 1 missing, 2 available to this org"), "{out}");
        assert!(out.contains("MISSING (1)"), "{out}");
        assert!(
            out.contains("verify_identity [auth:end_user; feature:telephony; self-scoping] — Verify a caller's identity with a one-time code"),
            "{out}"
        );
    }

    #[test]
    fn enabled_tool_id_absent_from_the_registry_is_flagged() {
        // SMOODEV-981: a camelCase toolId binds to nothing at runtime, so the
        // agent looks configured and has no tool.
        let v = view(json!({
            "mode": "restricted",
            "enabled": [{ "toolId": "knowledgeSearch", "authLevel": "none" }],
            "available": ["knowledge_search"], "missing": ["knowledge_search"],
        }));
        assert!(render_agent_tools(&v, &incident_registry()).contains("NOT IN REGISTRY"));
    }

    #[test]
    fn no_registry_means_bare_ids_not_false_ghost_warnings() {
        // The registry fetch is best-effort; an empty one must not accuse
        // every enabled tool of being unregistered.
        let v = view(json!({ "mode": "restricted", "enabled": [{ "toolId": "knowledge_search" }], "available": [], "missing": [] }));
        let out = render_agent_tools(&v, &[]);
        assert!(!out.contains("NOT IN REGISTRY"), "{out}");
        assert!(out.contains("knowledge_search"), "{out}");
    }

    #[test]
    fn change_warns_when_the_agent_became_an_allowlist() {
        let v = view(json!({
            "mode": "restricted", "enabled": [{"toolId":"knowledge_search"}],
            "available": ["knowledge_search","verify_identity"], "missing": ["verify_identity"],
            "change": {
                "toolId": "verify_identity", "applied": true, "previousMode": "all",
                "becameRestricted": true, "droppedTools": ["verify_identity"],
                "note": "materialised the allowlist",
            },
        }));
        let out = render_change(v.change.as_ref().unwrap());
        assert!(out.contains("WARNING:"), "{out}");
        assert!(out.contains("is now an ALLOWLIST"), "{out}");
        assert!(out.contains("DROPPED:"), "{out}");
        assert!(out.contains("verify_identity"), "{out}");
        assert!(out.contains("server: materialised the allowlist"), "{out}");
    }

    #[test]
    fn enable_on_an_unrestricted_agent_reports_a_no_op_not_a_success() {
        let c: ToolChange = serde_json::from_value(json!({
            "toolId": "verify_identity", "applied": false, "previousMode": "all",
            "becameRestricted": false, "droppedTools": [],
            "note": "agent is unrestricted; tool already enabled",
        }))
        .unwrap();
        let out = render_change(&c);
        assert!(out.contains("NO-OP:"), "{out}");
        assert!(out.contains("already enabled implicitly"), "{out}");
        assert!(!out.contains("WARNING:"), "{out}");
    }

    #[test]
    fn auth_level_arg_maps_to_snake_case_wire_values() {
        assert_eq!(AuthLevelArg::None.api_value(), "none");
        assert_eq!(AuthLevelArg::EndUser.api_value(), "end_user");
        assert_eq!(AuthLevelArg::Admin.api_value(), "admin");
    }

    #[test]
    fn summarize_takes_first_sentence_and_caps_it() {
        assert_eq!(summarize("Do a thing. Then another."), " — Do a thing");
        assert_eq!(summarize(""), "");
        assert!(summarize(&"x".repeat(200)).ends_with('…'));
    }
}
