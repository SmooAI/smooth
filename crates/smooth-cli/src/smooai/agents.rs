//! `th agents …` — agent CRUD plus the regenerate-* and per-agent
//! knowledge endpoints. All calls go through the raw HTTP helper so
//! the CLI doesn't have to keep up with progenitor's typed-body churn.

use anyhow::{Context, Result};
use clap::Subcommand;

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
