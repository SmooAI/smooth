//! `th agents …` — agent CRUD plus the regenerate-* and per-agent
//! knowledge endpoints. All calls go through the raw HTTP helper so
//! the CLI doesn't have to keep up with progenitor's typed-body churn.

use anyhow::{bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::user_client::UserClient;
use super::{print_json, print_list_envelope, read_body};

#[derive(Subcommand)]
pub enum Cmd {
    /// List agents in the active (or `--org-id`) organization.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show one agent's full record (config, status, metadata).
    Show {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Fetch the agent's generated summary blurb.
    Summary {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create an agent. Body is JSON (`CreateAgentRequest`); use `-`
    /// for stdin.
    Create {
        /// JSON request body, or `-` to read from stdin.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Mint a branded agent from typed flags — the ergonomic front door
    /// to `create`. Assembles the `CreateAgentRequest` for you, POSTs it,
    /// and (for a public chat agent) prints a ready-to-paste
    /// `<smooai-chat-widget>` embed snippet with the minted creds baked in.
    Mint {
        /// Agent display name (required).
        #[arg(long)]
        name: String,
        /// Agent kind. `chat` = embeddable chat widget (default);
        /// `workflow` = outbound/structured. Maps to types/directions.
        #[arg(long, value_enum, default_value_t = MintKind::Chat)]
        kind: MintKind,
        /// Where the agent is accessible. `public` (default) = embeddable
        /// widget with no login; `internal` = authenticated dashboard.
        #[arg(long, value_enum, default_value_t = MintVisibility::Public)]
        visibility: MintVisibility,
        /// Starting template (customer_support, sales_outreach, …). The
        /// backend fills tools/workflow/knowledge gaps from it.
        #[arg(long)]
        template: Option<String>,
        /// System prompt. Prefix with `@` to read from a file
        /// (e.g. `--instructions @prompt.md`).
        #[arg(long)]
        instructions: Option<String>,
        /// Channel-agnostic initial greeting seed.
        #[arg(long)]
        greeting: Option<String>,
        /// Personality: a preset name (`friendly`, `professional`, `witty`, …)
        /// or a full `PersonalityConfig` JSON object; `@` prefix reads a file.
        #[arg(long)]
        personality: Option<String>,
        /// `ConversationWorkflow` JSON (`{goal, steps: [{id, intent, criteria,
        /// next?}]}`); `@` prefix reads a file (SMOODEV-590 guided agency).
        #[arg(long)]
        workflow: Option<String>,
        /// `AgentToolConfig` JSON (`{enabledTools: [{toolId, enabled,
        /// authLevel, config?}]}`); `@` prefix reads a file. Empty
        /// `enabledTools` = full tool set; non-empty = restrict.
        #[arg(long = "tool-config")]
        tool_config: Option<String>,
        /// `AgentExtensionConfig` JSON (`{enabledExtensions: [{extensionId,
        /// enabled, config?}]}`); `@` prefix reads a file. extensionId is
        /// kebab-case (SEP extension name, e.g. plan-mode). Empty enabledExtensions
        /// = no extensions for this agent (fail-closed).
        #[arg(long = "extension")]
        extension_config: Option<String>,
        /// Short description shown in the agents list (required by the
        /// backend). Defaults to the agent name when omitted.
        #[arg(long)]
        summary: Option<String>,
        /// Allowed origin for the public widget (repeatable). Populates
        /// `authPublicClientAllowedOrigins`.
        #[arg(long = "allowed-origin")]
        allowed_origins: Vec<String>,
        /// Widget color override as `role=hex` (repeatable), e.g.
        /// `--color background=#020618 --color primary=#f2a618`.
        /// Mutually exclusive with `--brand-from-url`.
        #[arg(long = "color")]
        colors: Vec<String>,
        /// Extract a brand palette from this URL after create and PATCH it
        /// onto the agent's `widgetConfig.colors`. Mutually exclusive with
        /// `--color`. Human review is inherent — the CLI shows what it set.
        #[arg(long, conflicts_with = "colors")]
        brand_from_url: Option<String>,
        /// Require the visitor's name before chatting.
        #[arg(long)]
        require_name: bool,
        /// Require the visitor's email before chatting.
        #[arg(long)]
        require_email: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Patch an existing agent — a partial JSON body, or typed per-field
    /// flags (SMOODEV-590 per-agent config). Read fields with `show`.
    Update {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// JSON patch body, or `-` to read from stdin. Omit when using field flags.
        body: Option<String>,
        /// System prompt (`instructions.prompt`). Prefix with `@` to read from a file.
        #[arg(long)]
        instructions: Option<String>,
        /// Channel-agnostic initial greeting seed.
        #[arg(long)]
        greeting: Option<String>,
        /// Personality: a preset name (`friendly`, `professional`, `witty`, …)
        /// or a full `PersonalityConfig` JSON object; `@` prefix reads a file.
        #[arg(long)]
        personality: Option<String>,
        /// Where the agent is accessible.
        #[arg(long, value_enum)]
        visibility: Option<MintVisibility>,
        /// `ConversationWorkflow` JSON (`{goal, steps: [{id, intent, criteria,
        /// next?}]}`); `@` prefix reads a file (SMOODEV-590 guided agency).
        #[arg(long)]
        workflow: Option<String>,
        /// `AgentToolConfig` JSON (`{enabledTools: [{toolId, enabled,
        /// authLevel, config?}]}`); `@` prefix reads a file. Empty
        /// `enabledTools` = full tool set; non-empty = restrict.
        #[arg(long = "tool-config")]
        tool_config: Option<String>,
        /// `AgentExtensionConfig` JSON (`{enabledExtensions: [{extensionId,
        /// enabled, config?}]}`); `@` prefix reads a file. extensionId is
        /// kebab-case (SEP extension name, e.g. plan-mode). Empty enabledExtensions
        /// = no extensions for this agent (fail-closed).
        #[arg(long = "extension")]
        extension_config: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete an agent permanently.
    Delete {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Re-run one of the agent's generators.
    Regenerate {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// Which generator slot to re-run.
        #[arg(value_enum)]
        slot: RegenerateSlot,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// List the knowledge documents attached to an agent.
    ListKnowledge {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Replace the agent's attached knowledge set (JSON body).
    SetKnowledge {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// JSON body listing the knowledge to attach, or `-` for stdin.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Generate an agent config from a JSON prompt without persisting it.
    GenerateConfig {
        /// JSON generation request body, or `-` to read from stdin.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum RegenerateSlot {
    Prompts,
    Summary,
    Persona,
    Instructions,
    Icon,
}

/// What flavor of agent to mint. Sets the agent's `kind` field
/// (SMOODEV-2203: `chat` | `workflow`) and the matching `types`/`directions`.
/// A `workflow` agent has no chat channel — the backend skips the widget
/// auth-client for it.
#[derive(Clone, Copy, ValueEnum)]
pub enum MintKind {
    /// Inbound text agent — the embeddable chat widget (kind=chat).
    Chat,
    /// Structured workflow agent, no chat widget (kind=workflow).
    Workflow,
}

impl MintKind {
    fn api_value(self) -> &'static str {
        match self {
            MintKind::Chat => "chat",
            MintKind::Workflow => "workflow",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum MintVisibility {
    Public,
    Internal,
}

impl MintVisibility {
    fn api_value(self) -> &'static str {
        match self {
            MintVisibility::Public => "public",
            MintVisibility::Internal => "internal",
        }
    }
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    // SMOODEV-1863 — authenticate as the logged-in USER (Supabase JWT), the
    // same way `th api crm` does, NOT the org-locked M2M `client_credentials`
    // token. A master-org M2M can't act on a child org (api-prime enforces the
    // org-lock — see the paired monorepo PR), so `show`/`update` 403'd on child
    // orgs. A parent-org admin's user session carries cross-org access via
    // membership, so every agent verb works on the orgs the user belongs to.
    let client = UserClient::from_user_session().await?;
    match cmd {
        Cmd::List { org } => {
            let org = crate::active_org::resolve(org)?;
            let body = client.get(&format!("/organizations/{org}/agents")).await.context("GET agents")?;
            print_list_envelope(&body, "agents");
        }
        Cmd::Show { agent_id, org } => {
            let org = crate::active_org::resolve(org)?;
            print_json(&client.get(&format!("/organizations/{org}/agents/{agent_id}")).await.context("GET agent")?);
        }
        Cmd::Summary { agent_id, org } => {
            let org = crate::active_org::resolve(org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{org}/agents/{agent_id}/summary"))
                    .await
                    .context("GET agent summary")?,
            );
        }
        Cmd::Create { body, org } => {
            let org = crate::active_org::resolve(org)?;
            let body = read_body(&body)?;
            print_json(&client.post(&format!("/organizations/{org}/agents"), &body).await.context("POST agent")?);
        }
        Cmd::Mint {
            name,
            kind,
            visibility,
            template,
            instructions,
            greeting,
            personality,
            workflow,
            tool_config,
            extension_config,
            summary,
            allowed_origins,
            colors,
            brand_from_url,
            require_name,
            require_email,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let prompt = instructions.map(read_flag_or_file).transpose()?;
            let color_map = parse_colors(&colors)?;
            let personality = personality.map(personality_value).transpose()?;
            let workflow = workflow.map(|v| parse_json_object_flag("workflow", v)).transpose()?;
            let tool_config = tool_config.map(|v| parse_json_object_flag("tool-config", v)).transpose()?;
            let extension_config = extension_config.map(|v| parse_json_object_flag("extension", v)).transpose()?;
            let body = build_mint_body(
                &org,
                &name,
                kind,
                visibility,
                template.as_deref(),
                prompt.as_deref(),
                greeting.as_deref(),
                personality,
                workflow,
                tool_config,
                extension_config,
                summary.as_deref(),
                &allowed_origins,
                &color_map,
                require_name,
                require_email,
            )?;

            // Creating an agent is a user-attributed admin write; the whole
            // command already runs on the user session (see top of `cmd`).
            let created = client.post(&format!("/organizations/{org}/agents"), &body).await.context("POST agent (mint)")?;

            let agent_id = created.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
            println!();
            println!("  {} minted agent {} {}", "✓".green(), agent_id.cyan(), name.bold());

            // --brand-from-url: extract a palette, then PATCH it onto the
            // agent's widgetConfig.colors. Falls back gracefully so a mint
            // is never lost just because the extractor is unreachable.
            let mut applied_colors = color_map;
            if let Some(url) = brand_from_url {
                match extract_and_apply_palette(&client, &org, &agent_id, &url).await {
                    Ok(palette) => {
                        println!("  {} applied brand palette from {}", "✓".green(), url.dimmed());
                        applied_colors = palette;
                    }
                    Err(e) => {
                        println!(
                            "  {} brand extraction failed ({e:#}) — set colors manually with `th api agents update`",
                            "!".yellow()
                        );
                    }
                }
            }

            // For a public chat agent, print the ready-to-paste embed snippet.
            if matches!(kind, MintKind::Chat) && matches!(visibility, MintVisibility::Public) {
                let client_id = created.get("authPublicClientId").and_then(Value::as_str).unwrap_or("");
                let client_secret = created.get("authPublicClientSecret").and_then(Value::as_str).unwrap_or("");
                println!();
                println!("  {} paste this before </body>:", "▸".cyan());
                println!();
                println!("{}", render_embed_snippet(&agent_id, &name, client_id, client_secret, &applied_colors));
            }
            println!();
        }
        Cmd::Update {
            agent_id,
            body,
            instructions,
            greeting,
            personality,
            visibility,
            workflow,
            tool_config,
            extension_config,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let has_flags = instructions.is_some()
                || greeting.is_some()
                || personality.is_some()
                || visibility.is_some()
                || workflow.is_some()
                || tool_config.is_some()
                || extension_config.is_some();
            let body = match (body, has_flags) {
                (Some(_), true) => bail!("pass either a JSON body or field flags, not both"),
                (Some(b), false) => read_body(&b)?,
                (None, _) => {
                    let prompt = instructions.map(read_flag_or_file).transpose()?;
                    build_update_body(
                        prompt.as_deref(),
                        greeting.as_deref(),
                        personality.map(personality_value).transpose()?,
                        visibility,
                        workflow.map(|v| parse_json_object_flag("workflow", v)).transpose()?,
                        tool_config.map(|v| parse_json_object_flag("tool-config", v)).transpose()?,
                        extension_config.map(|v| parse_json_object_flag("extension", v)).transpose()?,
                    )?
                }
            };
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/agents/{agent_id}"), &body)
                    .await
                    .context("PATCH agent")?,
            );
        }
        Cmd::Delete { agent_id, org } => {
            let org = crate::active_org::resolve(org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{org}/agents/{agent_id}"))
                    .await
                    .context("DELETE agent")?,
            );
        }
        Cmd::Regenerate { agent_id, slot, org } => {
            let org = crate::active_org::resolve(org)?;
            let suffix = match slot {
                RegenerateSlot::Prompts => "regenerate-prompts",
                RegenerateSlot::Summary => "regenerate-summary",
                RegenerateSlot::Persona => "regenerate-persona",
                RegenerateSlot::Instructions => "regenerate-instructions",
                RegenerateSlot::Icon => "regenerate-icon",
            };
            print_json(
                &client
                    .post_empty(&format!("/organizations/{org}/agents/{agent_id}/{suffix}"))
                    .await
                    .context("POST regenerate")?,
            );
        }
        Cmd::ListKnowledge { agent_id, org } => {
            let org = crate::active_org::resolve(org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{org}/agents/{agent_id}/knowledge"))
                    .await
                    .context("GET agent knowledge")?,
            );
        }
        Cmd::SetKnowledge { agent_id, body, org } => {
            let org = crate::active_org::resolve(org)?;
            let body = read_body(&body)?;
            print_json(
                &client
                    .put(&format!("/organizations/{org}/agents/{agent_id}/knowledge"), &body)
                    .await
                    .context("PUT agent knowledge")?,
            );
        }
        Cmd::GenerateConfig { body, org } => {
            let org = crate::active_org::resolve(org)?;
            let body = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{org}/agents/generate-config"), &body)
                    .await
                    .context("POST generate-config")?,
            );
        }
    }
    Ok(())
}

/// The widget color roles, in the order the embed snippet renders them.
/// Matches `WidgetConfig.colors` in the smooai agent schema.
const COLOR_ROLES: &[&str] = &[
    "text",
    "background",
    "primary",
    "primaryText",
    "secondary",
    "chatBubbleInbound",
    "chatBubbleInboundText",
    "chatBubbleOutbound",
    "chatBubbleOutboundText",
    "border",
];

/// A flag value that is either a literal string or `@path` to slurp a file.
fn read_flag_or_file(v: String) -> Result<String> {
    if let Some(path) = v.strip_prefix('@') {
        std::fs::read_to_string(path).with_context(|| format!("read {path}"))
    } else {
        Ok(v)
    }
}

/// Parse a JSON-object flag value (literal or `@file`), failing loudly on
/// invalid JSON or a non-object so a stray array/string doesn't reach the
/// backend as a confusing 400.
fn parse_json_object_flag(flag: &str, v: String) -> Result<Value> {
    let raw = read_flag_or_file(v)?;
    let val: Value = serde_json::from_str(&raw).with_context(|| format!("--{flag} must be a JSON object (or @file containing one)"))?;
    if !val.is_object() {
        bail!("--{flag} must be a JSON object, got {}", type_name(&val));
    }
    Ok(val)
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// `--personality` accepts either a bare preset name (`friendly`) or a full
/// `PersonalityConfig` JSON object / `@file`. Preset validity is left to the
/// backend schema, which returns a readable 400 listing the valid presets.
fn personality_value(v: String) -> Result<Value> {
    let raw = read_flag_or_file(v)?;
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        parse_json_object_flag("personality", trimmed.to_string())
    } else {
        Ok(json!({ "preset": trimmed }))
    }
}

/// Assemble a PATCH body from the typed update flags. Pure — no I/O — so
/// it's unit-testable. Bails when no field was set so `th api agents update
/// <id>` with nothing else fails loudly instead of PATCHing `{}`.
fn build_update_body(
    instructions: Option<&str>,
    greeting: Option<&str>,
    personality: Option<Value>,
    visibility: Option<MintVisibility>,
    workflow: Option<Value>,
    tool_config: Option<Value>,
    extension_config: Option<Value>,
) -> Result<Value> {
    let mut obj = serde_json::Map::new();
    if let Some(p) = instructions {
        obj.insert("instructions".into(), json!({ "prompt": p }));
    }
    if let Some(g) = greeting {
        obj.insert("greeting".into(), json!(g));
    }
    if let Some(p) = personality {
        obj.insert("personality".into(), p);
    }
    if let Some(v) = visibility {
        obj.insert("visibility".into(), json!(v.api_value()));
    }
    if let Some(w) = workflow {
        obj.insert("conversationWorkflow".into(), w);
    }
    if let Some(t) = tool_config {
        obj.insert("toolConfig".into(), t);
    }
    if let Some(e) = extension_config {
        obj.insert("extensionConfig".into(), e);
    }
    if obj.is_empty() {
        bail!(
            "nothing to update — pass a JSON body or at least one of --instructions --greeting --personality --visibility --workflow --tool-config --extension"
        );
    }
    Ok(Value::Object(obj))
}

/// Parse repeated `role=hex` flags into a role→hex map, rejecting unknown
/// roles so a typo (`primaryColor=…`) fails loudly instead of being dropped
/// by the backend schema.
fn parse_colors(pairs: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let (role, hex) = pair.split_once('=').with_context(|| format!("--color must be role=hex, got `{pair}`"))?;
        if !COLOR_ROLES.contains(&role) {
            bail!("unknown color role `{role}` — valid roles: {}", COLOR_ROLES.join(", "));
        }
        out.push((role.to_string(), hex.to_string()));
    }
    Ok(out)
}

/// Assemble the `CreateAgentRequest` JSON the backend expects from the mint
/// flags. Pure — no I/O — so it's unit-testable. The backend fills in the
/// auth-public-client credentials and `createdBy`; every other NOT-NULL column
/// (`organizationId`, `summary`, `isBuiltin`, …) must be in the body.
#[allow(clippy::too_many_arguments)]
fn build_mint_body(
    org: &str,
    name: &str,
    kind: MintKind,
    visibility: MintVisibility,
    template: Option<&str>,
    instructions: Option<&str>,
    greeting: Option<&str>,
    personality: Option<Value>,
    workflow: Option<Value>,
    tool_config: Option<Value>,
    extension_config: Option<Value>,
    summary: Option<&str>,
    allowed_origins: &[String],
    colors: &[(String, String)],
    require_name: bool,
    require_email: bool,
) -> Result<Value> {
    // kind (SMOODEV-2203) drives the channel shape. Chat = inbound text
    // widget; workflow = no chat channel (empty types), structured/outbound —
    // the backend skips the widget auth-client for kind=workflow.
    let (types, directions) = match kind {
        MintKind::Chat => (json!(["text"]), json!(["inbound"])),
        MintKind::Workflow => (json!([]), json!(["outbound"])),
    };

    // `organizationId`, `summary`, and `isBuiltin` are NOT-NULL columns the
    // create route requires in the body (it only fills in authPublicClient*/
    // createdBy). summary defaults to the name; it can be regenerated later via
    // `th api agents regenerate-summary`.
    let mut body = json!({
        "organizationId": org,
        "name": name,
        "summary": summary.unwrap_or(name),
        "isBuiltin": false,
        "kind": kind.api_value(),
        "types": types,
        "directions": directions,
        "visibility": visibility.api_value(),
        "instructions": { "prompt": instructions.unwrap_or("") },
    });
    let obj = body.as_object_mut().expect("object literal");

    if let Some(t) = template {
        obj.insert("template".into(), json!(t));
    }
    if let Some(g) = greeting {
        obj.insert("greeting".into(), json!(g));
    }
    if let Some(p) = personality {
        obj.insert("personality".into(), p);
    }
    if let Some(w) = workflow {
        obj.insert("conversationWorkflow".into(), w);
    }
    if let Some(t) = tool_config {
        obj.insert("toolConfig".into(), t);
    }
    if let Some(e) = extension_config {
        obj.insert("extensionConfig".into(), e);
    }
    if !allowed_origins.is_empty() {
        obj.insert("authPublicClientAllowedOrigins".into(), json!(allowed_origins));
    }

    // widgetConfig is only assembled when the user set something on it —
    // otherwise let the backend defaults apply.
    let mut widget = serde_json::Map::new();
    if require_name {
        widget.insert("requireName".into(), json!(true));
    }
    if require_email {
        widget.insert("requireEmail".into(), json!(true));
    }
    if !colors.is_empty() {
        widget.insert("colors".into(), Value::Object(colors.iter().map(|(k, v)| (k.clone(), json!(v))).collect()));
    }
    if !widget.is_empty() {
        obj.insert("widgetConfig".into(), Value::Object(widget));
    }

    Ok(body)
}

/// Render the `<smooai-chat-widget>` embed snippet, mirroring the shape in
/// docs/Customer-Sites/.../Branded-Chat-Widget.md. `colors` is rendered
/// inline only when present (belt-and-suspenders — the widget also pulls the
/// agent's saved palette).
fn render_embed_snippet(agent_id: &str, name: &str, client_id: &str, client_secret: &str, colors: &[(String, String)]) -> String {
    let colors_block = if colors.is_empty() {
        String::new()
    } else {
        let lines: Vec<String> = colors.iter().map(|(role, hex)| format!("                {role}: '{hex}',")).collect();
        format!("\n            colors: {{\n{}\n            }},", lines.join("\n"))
    };
    format!(
        r#"<smooai-chat-widget></smooai-chat-widget>
<script type="module" src="https://cdn.smoo.ai/ui-chat-widget/smooai-chat-widget.main.es.js"></script>
<script>
    window.addEventListener('load', () => {{
        customElements.whenDefined('smooai-chat-widget').then(() => {{
            const el = document.querySelector('smooai-chat-widget');
            if (el?.shadowRoot) {{
                const link = document.createElement('link');
                link.rel = 'stylesheet';
                link.href = 'https://cdn.smoo.ai/ui-chat-widget/smooai-chat-widget.css';
                el.shadowRoot.appendChild(link);
            }}
        }});

        window.SmooAIChatWidget?.setConfig({{
            clientId: '{client_id}', // agent.authPublicClientId
            clientPublicKey: '{client_secret}', // agent.authPublicClientSecret
            agentId: '{agent_id}', // agent UUID
            agentName: '{name}',
            iconType: 'agent-icon',{colors_block}
        }});
    }});
</script>"#
    )
}

/// POST extract-brand-palette, then PATCH the proposed palette onto the
/// agent's widgetConfig.colors. Returns the palette that was applied so the
/// caller can echo it into the embed snippet.
async fn extract_and_apply_palette(client: &super::user_client::UserClient, org: &str, agent_id: &str, url: &str) -> Result<Vec<(String, String)>> {
    let extracted = client
        .post(&format!("/organizations/{org}/agents/{agent_id}/extract-brand-palette"), &json!({ "url": url }))
        .await
        .context("POST extract-brand-palette")?;
    let proposed = extracted
        .get("proposed")
        .and_then(Value::as_object)
        .context("response missing `proposed` palette")?;

    let palette: Vec<(String, String)> = COLOR_ROLES
        .iter()
        .filter_map(|role| proposed.get(*role).and_then(Value::as_str).map(|hex| ((*role).to_string(), hex.to_string())))
        .collect();
    if palette.is_empty() {
        bail!("extractor returned no usable colors");
    }

    let colors = Value::Object(palette.iter().map(|(k, v)| (k.clone(), json!(v))).collect());
    client
        .patch(
            &format!("/organizations/{org}/agents/{agent_id}"),
            &json!({ "widgetConfig": { "colors": colors } }),
        )
        .await
        .context("PATCH widgetConfig.colors")?;
    Ok(palette)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> Vec<(String, String)> {
        vec![("background".into(), "#020618".into()), ("primary".into(), "#f2a618".into())]
    }

    #[test]
    fn chat_body_is_inbound_text() {
        let body = build_mint_body(
            "org-123",
            "Bot",
            MintKind::Chat,
            MintVisibility::Public,
            None,
            Some("be nice"),
            None,
            None,
            None,
            None,
            None,
            None,
            &[],
            &[],
            false,
            false,
        )
        .unwrap();
        assert_eq!(body["organizationId"], "org-123");
        assert_eq!(body["name"], "Bot");
        // summary defaults to the name when not supplied.
        assert_eq!(body["summary"], "Bot");
        assert_eq!(body["isBuiltin"], false);
        assert_eq!(body["kind"], "chat");
        assert_eq!(body["types"], json!(["text"]));
        assert_eq!(body["directions"], json!(["inbound"]));
        assert_eq!(body["visibility"], "public");
        assert_eq!(body["instructions"]["prompt"], "be nice");
        // No widgetConfig when nothing was set on it.
        assert!(body.get("widgetConfig").is_none());
    }

    #[test]
    fn workflow_body_is_outbound() {
        let body = build_mint_body(
            "org-9",
            "Flow",
            MintKind::Workflow,
            MintVisibility::Internal,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("outbound flow"),
            &[],
            &[],
            false,
            false,
        )
        .unwrap();
        assert_eq!(body["organizationId"], "org-9");
        assert_eq!(body["summary"], "outbound flow");
        assert_eq!(body["isBuiltin"], false);
        assert_eq!(body["kind"], "workflow");
        assert_eq!(body["types"], json!([]));
        assert_eq!(body["directions"], json!(["outbound"]));
        assert_eq!(body["visibility"], "internal");
        assert_eq!(body["instructions"]["prompt"], "");
    }

    #[test]
    fn body_carries_optional_fields() {
        let origins = vec!["https://chakrabpc.com".to_string()];
        let body = build_mint_body(
            "org-7",
            "Bot",
            MintKind::Chat,
            MintVisibility::Public,
            Some("customer_support"),
            None,
            Some("Hi there!"),
            None,
            None,
            None,
            None,
            Some("A helpful bot"),
            &origins,
            &colors(),
            true,
            true,
        )
        .unwrap();
        assert_eq!(body["summary"], "A helpful bot");
        assert_eq!(body["template"], "customer_support");
        assert_eq!(body["greeting"], "Hi there!");
        assert_eq!(body["authPublicClientAllowedOrigins"], json!(["https://chakrabpc.com"]));
        assert_eq!(body["widgetConfig"]["requireName"], true);
        assert_eq!(body["widgetConfig"]["requireEmail"], true);
        assert_eq!(body["widgetConfig"]["colors"]["background"], "#020618");
        assert_eq!(body["widgetConfig"]["colors"]["primary"], "#f2a618");
    }

    #[test]
    fn mint_body_carries_per_agent_config() {
        let workflow = json!({ "goal": "book a demo", "steps": [{ "id": "greet", "intent": "greet", "criteria": "name confirmed" }] });
        let tools = json!({ "enabledTools": [{ "toolId": "knowledge_search", "enabled": true, "authLevel": "none" }] });
        let extensions = json!({ "enabledExtensions": [{ "extensionId": "smooai-crm", "enabled": true }] });
        let body = build_mint_body(
            "org-1",
            "Bot",
            MintKind::Chat,
            MintVisibility::Public,
            None,
            None,
            None,
            Some(json!({ "preset": "witty" })),
            Some(workflow.clone()),
            Some(tools.clone()),
            Some(extensions.clone()),
            None,
            &[],
            &[],
            false,
            false,
        )
        .unwrap();
        assert_eq!(body["personality"]["preset"], "witty");
        assert_eq!(body["conversationWorkflow"], workflow);
        assert_eq!(body["toolConfig"], tools);
        assert_eq!(body["extensionConfig"], extensions);
    }

    #[test]
    fn update_body_maps_every_field() {
        let extensions = json!({ "enabledExtensions": [{ "extensionId": "smooai-crm", "enabled": true }] });
        let body = build_update_body(
            Some("be terse"),
            Some("Hey!"),
            Some(json!({ "preset": "zen" })),
            Some(MintVisibility::Internal),
            Some(json!({ "goal": "g", "steps": [] })),
            Some(json!({ "enabledTools": [] })),
            Some(extensions.clone()),
        )
        .unwrap();
        assert_eq!(body["instructions"]["prompt"], "be terse");
        assert_eq!(body["greeting"], "Hey!");
        assert_eq!(body["personality"]["preset"], "zen");
        assert_eq!(body["visibility"], "internal");
        assert_eq!(body["conversationWorkflow"]["goal"], "g");
        assert_eq!(body["toolConfig"]["enabledTools"], json!([]));
        assert_eq!(body["extensionConfig"], extensions);
    }

    #[test]
    fn update_body_omits_unset_fields() {
        let body = build_update_body(None, Some("Hi"), None, None, None, None, None).unwrap();
        assert_eq!(body, json!({ "greeting": "Hi" }));
    }

    #[test]
    fn update_body_rejects_empty() {
        assert!(build_update_body(None, None, None, None, None, None, None).is_err());
    }

    #[test]
    fn personality_accepts_preset_or_json() {
        assert_eq!(personality_value("friendly".into()).unwrap(), json!({ "preset": "friendly" }));
        let full = personality_value(r#"{ "preset": "witty", "creativity": 0.3, "persona": "dry" }"#.into()).unwrap();
        assert_eq!(full["creativity"], 0.3);
        // Looks like JSON but isn't → loud error, not a preset named "{".
        assert!(personality_value("{not json".into()).is_err());
    }

    #[test]
    fn json_object_flag_rejects_non_objects() {
        assert!(parse_json_object_flag("workflow", "[]".into()).is_err());
        assert!(parse_json_object_flag("workflow", "\"str\"".into()).is_err());
        assert!(parse_json_object_flag("workflow", "not json".into()).is_err());
        assert_eq!(parse_json_object_flag("workflow", "{}".into()).unwrap(), json!({}));
    }

    #[test]
    fn json_object_flag_reads_at_file() {
        let dir = std::env::temp_dir().join(format!("th-agents-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wf.json");
        std::fs::write(&path, r#"{ "goal": "g", "steps": [] }"#).unwrap();
        let val = parse_json_object_flag("workflow", format!("@{}", path.display())).unwrap();
        assert_eq!(val["goal"], "g");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_colors_rejects_unknown_role() {
        assert!(parse_colors(&["primaryColor=#fff".into()]).is_err());
        assert!(parse_colors(&["nohex".into()]).is_err());
        let ok = parse_colors(&["primary=#f2a618".into()]).unwrap();
        assert_eq!(ok, vec![("primary".to_string(), "#f2a618".to_string())]);
    }

    #[test]
    fn embed_snippet_bakes_creds_and_colors() {
        let snip = render_embed_snippet("agent-123", "Transformation Posture", "cid-1", "pk-2", &colors());
        assert!(snip.contains("<smooai-chat-widget></smooai-chat-widget>"));
        assert!(snip.contains("clientId: 'cid-1'"));
        assert!(snip.contains("clientPublicKey: 'pk-2'"));
        assert!(snip.contains("agentId: 'agent-123'"));
        assert!(snip.contains("agentName: 'Transformation Posture'"));
        assert!(snip.contains("background: '#020618'"));
        assert!(snip.contains("primary: '#f2a618'"));
    }

    #[test]
    fn embed_snippet_omits_empty_colors_block() {
        let snip = render_embed_snippet("a", "n", "c", "s", &[]);
        assert!(!snip.contains("colors: {"));
    }
}
