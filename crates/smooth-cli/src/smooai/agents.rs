//! `th agents …` — agent CRUD plus the regenerate-* and per-agent
//! knowledge endpoints. All calls go through the raw HTTP helper so
//! the CLI doesn't have to keep up with progenitor's typed-body churn.

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// List agents in the active (or `--org`) organization.
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Show {
        agent_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Summary {
        agent_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Create an agent. Body is JSON (`CreateAgentRequest`); use `-`
    /// for stdin.
    Create {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        agent_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        agent_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Re-run one of the agent's generators.
    Regenerate {
        agent_id: String,
        #[arg(value_enum)]
        slot: RegenerateSlot,
        #[arg(long)]
        org: Option<String>,
    },
    ListKnowledge {
        agent_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    SetKnowledge {
        agent_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    GenerateConfig {
        body: String,
        #[arg(long)]
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
