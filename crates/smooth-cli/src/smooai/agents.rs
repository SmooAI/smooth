//! `th agents …` — agent CRUD plus the regenerate-* and per-agent
//! knowledge endpoints. All calls go through the raw HTTP helper so
//! the CLI doesn't have to keep up with progenitor's typed-body churn.

use anyhow::{bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

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
    /// Patch an existing agent with a partial JSON body.
    Update {
        /// The agent id from `th api agents list`.
        agent_id: String,
        /// JSON patch body, or `-` to read from stdin.
        body: String,
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
    let client = require_authed().await?;
    match cmd {
        Cmd::List { org } => {
            let org = require_active_org(&client, org)?;
            let body = client.get(&format!("/organizations/{org}/agents")).await.context("GET agents")?;
            print_list_envelope(&body, "agents");
        }
        Cmd::Show { agent_id, org } => {
            let org = require_active_org(&client, org)?;
            print_json(&client.get(&format!("/organizations/{org}/agents/{agent_id}")).await.context("GET agent")?);
        }
        Cmd::Summary { agent_id, org } => {
            let org = require_active_org(&client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{org}/agents/{agent_id}/summary"))
                    .await
                    .context("GET agent summary")?,
            );
        }
        Cmd::Create { body, org } => {
            let org = require_active_org(&client, org)?;
            let body = read_body(&body)?;
            print_json(&client.post(&format!("/organizations/{org}/agents"), Some(&body)).await.context("POST agent")?);
        }
        Cmd::Mint {
            name,
            kind,
            visibility,
            template,
            instructions,
            greeting,
            allowed_origins,
            colors,
            brand_from_url,
            require_name,
            require_email,
            org,
        } => {
            let org = require_active_org(&client, org)?;
            let prompt = instructions.map(read_flag_or_file).transpose()?;
            let color_map = parse_colors(&colors)?;
            let body = build_mint_body(
                &name,
                kind,
                visibility,
                template.as_deref(),
                prompt.as_deref(),
                greeting.as_deref(),
                &allowed_origins,
                &color_map,
                require_name,
                require_email,
            )?;

            let created = client
                .post(&format!("/organizations/{org}/agents"), Some(&body))
                .await
                .context("POST agent (mint)")?;

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
        Cmd::Update { agent_id, body, org } => {
            let org = require_active_org(&client, org)?;
            let body = read_body(&body)?;
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/agents/{agent_id}"), &body)
                    .await
                    .context("PATCH agent")?,
            );
        }
        Cmd::Delete { agent_id, org } => {
            let org = require_active_org(&client, org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{org}/agents/{agent_id}"))
                    .await
                    .context("DELETE agent")?,
            );
        }
        Cmd::Regenerate { agent_id, slot, org } => {
            let org = require_active_org(&client, org)?;
            let suffix = match slot {
                RegenerateSlot::Prompts => "regenerate-prompts",
                RegenerateSlot::Summary => "regenerate-summary",
                RegenerateSlot::Persona => "regenerate-persona",
                RegenerateSlot::Instructions => "regenerate-instructions",
                RegenerateSlot::Icon => "regenerate-icon",
            };
            print_json(
                &client
                    .post(&format!("/organizations/{org}/agents/{agent_id}/{suffix}"), None)
                    .await
                    .context("POST regenerate")?,
            );
        }
        Cmd::ListKnowledge { agent_id, org } => {
            let org = require_active_org(&client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{org}/agents/{agent_id}/knowledge"))
                    .await
                    .context("GET agent knowledge")?,
            );
        }
        Cmd::SetKnowledge { agent_id, body, org } => {
            let org = require_active_org(&client, org)?;
            let body = read_body(&body)?;
            print_json(
                &client
                    .put(&format!("/organizations/{org}/agents/{agent_id}/knowledge"), &body)
                    .await
                    .context("PUT agent knowledge")?,
            );
        }
        Cmd::GenerateConfig { body, org } => {
            let org = require_active_org(&client, org)?;
            let body = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{org}/agents/generate-config"), Some(&body))
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
/// flags. Pure — no I/O — so it's unit-testable. The backend generates the
/// summary and owns the auth client; we only send the authored fields.
#[allow(clippy::too_many_arguments)]
fn build_mint_body(
    name: &str,
    kind: MintKind,
    visibility: MintVisibility,
    template: Option<&str>,
    instructions: Option<&str>,
    greeting: Option<&str>,
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

    let mut body = json!({
        "name": name,
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
async fn extract_and_apply_palette(client: &smooth_api_client::SmoothApiClient, org: &str, agent_id: &str, url: &str) -> Result<Vec<(String, String)>> {
    let extracted = client
        .post(
            &format!("/organizations/{org}/agents/{agent_id}/extract-brand-palette"),
            Some(&json!({ "url": url })),
        )
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
            "Bot",
            MintKind::Chat,
            MintVisibility::Public,
            None,
            Some("be nice"),
            None,
            &[],
            &[],
            false,
            false,
        )
        .unwrap();
        assert_eq!(body["name"], "Bot");
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
        let body = build_mint_body("Flow", MintKind::Workflow, MintVisibility::Internal, None, None, None, &[], &[], false, false).unwrap();
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
            "Bot",
            MintKind::Chat,
            MintVisibility::Public,
            Some("customer_support"),
            None,
            Some("Hi there!"),
            &origins,
            &colors(),
            true,
            true,
        )
        .unwrap();
        assert_eq!(body["template"], "customer_support");
        assert_eq!(body["greeting"], "Hi there!");
        assert_eq!(body["authPublicClientAllowedOrigins"], json!(["https://chakrabpc.com"]));
        assert_eq!(body["widgetConfig"]["requireName"], true);
        assert_eq!(body["widgetConfig"]["requireEmail"], true);
        assert_eq!(body["widgetConfig"]["colors"]["background"], "#020618");
        assert_eq!(body["widgetConfig"]["colors"]["primary"], "#f2a618");
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
