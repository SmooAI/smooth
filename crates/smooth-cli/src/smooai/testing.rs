//! `th testing …` — testing platform (deployments / cases /
//! environments / runs).

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    Deployments {
        #[command(subcommand)]
        cmd: DeploymentsCmd,
    },
    Cases {
        #[command(subcommand)]
        cmd: CasesCmd,
    },
    Environments {
        #[command(subcommand)]
        cmd: EnvironmentsCmd,
    },
    Runs {
        #[command(subcommand)]
        cmd: RunsCmd,
    },
}

#[derive(Subcommand)]
pub enum DeploymentsCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Show {
        deployment_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Create {
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        deployment_id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        deployment_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum CasesCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Show {
        case_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Create {
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        case_id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        case_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum EnvironmentsCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Create {
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        env_id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        env_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RunsCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Show {
        run_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Create {
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        run_id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        run_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Submit results for a run. Body is optional JSON.
    Results {
        run_id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed()?;
    let opt_body = |body: Option<String>| -> Result<Option<serde_json::Value>> {
        match body {
            Some(p) => Ok(Some(read_body(&p)?)),
            None => Ok(None),
        }
    };
    match cmd {
        Cmd::Deployments { cmd } => match cmd {
            DeploymentsCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(
                    &client.get(&format!("/organizations/{o}/testing/deployments")).await.context("GET deployments")?,
                    "deployments",
                );
            }
            DeploymentsCmd::Show { deployment_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .get(&format!("/organizations/{o}/testing/deployments/{deployment_id}"))
                        .await
                        .context("GET deployment")?,
                );
            }
            DeploymentsCmd::Create { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?;
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/testing/deployments"), b.as_ref())
                        .await
                        .context("POST deployment")?,
                );
            }
            DeploymentsCmd::Update { deployment_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?.unwrap_or_else(|| serde_json::json!({}));
                print_json(
                    &client
                        .patch(&format!("/organizations/{o}/testing/deployments/{deployment_id}"), &b)
                        .await
                        .context("PATCH deployment")?,
                );
            }
            DeploymentsCmd::Delete { deployment_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/testing/deployments/{deployment_id}"))
                        .await
                        .context("DELETE deployment")?,
                );
            }
        },
        Cmd::Cases { cmd } => match cmd {
            CasesCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(&client.get(&format!("/organizations/{o}/testing/cases")).await.context("GET cases")?, "cases");
            }
            CasesCmd::Show { case_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(&client.get(&format!("/organizations/{o}/testing/cases/{case_id}")).await.context("GET case")?);
            }
            CasesCmd::Create { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?;
                print_json(&client.post(&format!("/organizations/{o}/testing/cases"), b.as_ref()).await.context("POST case")?);
            }
            CasesCmd::Update { case_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?.unwrap_or_else(|| serde_json::json!({}));
                print_json(&client.patch(&format!("/organizations/{o}/testing/cases/{case_id}"), &b).await.context("PATCH case")?);
            }
            CasesCmd::Delete { case_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(&client.delete(&format!("/organizations/{o}/testing/cases/{case_id}")).await.context("DELETE case")?);
            }
        },
        Cmd::Environments { cmd } => match cmd {
            EnvironmentsCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(
                    &client.get(&format!("/organizations/{o}/testing/environments")).await.context("GET test environments")?,
                    "environments",
                );
            }
            EnvironmentsCmd::Create { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?;
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/testing/environments"), b.as_ref())
                        .await
                        .context("POST test environment")?,
                );
            }
            EnvironmentsCmd::Update { env_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?.unwrap_or_else(|| serde_json::json!({}));
                print_json(
                    &client
                        .patch(&format!("/organizations/{o}/testing/environments/{env_id}"), &b)
                        .await
                        .context("PATCH test environment")?,
                );
            }
            EnvironmentsCmd::Delete { env_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/testing/environments/{env_id}"))
                        .await
                        .context("DELETE test environment")?,
                );
            }
        },
        Cmd::Runs { cmd } => match cmd {
            RunsCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(&client.get(&format!("/organizations/{o}/testing/runs")).await.context("GET runs")?, "runs");
            }
            RunsCmd::Show { run_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(&client.get(&format!("/organizations/{o}/testing/runs/{run_id}")).await.context("GET run")?);
            }
            RunsCmd::Create { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?;
                print_json(&client.post(&format!("/organizations/{o}/testing/runs"), b.as_ref()).await.context("POST run")?);
            }
            RunsCmd::Update { run_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?.unwrap_or_else(|| serde_json::json!({}));
                print_json(&client.patch(&format!("/organizations/{o}/testing/runs/{run_id}"), &b).await.context("PATCH run")?);
            }
            RunsCmd::Delete { run_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(&client.delete(&format!("/organizations/{o}/testing/runs/{run_id}")).await.context("DELETE run")?);
            }
            RunsCmd::Results { run_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?;
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/testing/runs/{run_id}/results"), b.as_ref())
                        .await
                        .context("POST run results")?,
                );
            }
        },
    }
    Ok(())
}
