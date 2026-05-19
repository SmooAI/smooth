//! `th jobs …` — the async job queue.

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// List jobs. Filterable via query params.
    List {
        #[arg(long)]
        limit: Option<u64>,
        #[arg(long)]
        offset: Option<u64>,
        #[arg(long)]
        organization_id: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, name = "type")]
        type_: Option<String>,
    },
    Show {
        job_id: String,
    },
    Create {
        body: String,
    },
    Update {
        job_id: String,
        body: String,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed()?;
    match cmd {
        Cmd::List {
            limit,
            offset,
            organization_id,
            status,
            type_,
        } => {
            let mut q: Vec<(String, String)> = Vec::new();
            if let Some(v) = limit {
                q.push(("limit".into(), v.to_string()));
            }
            if let Some(v) = offset {
                q.push(("offset".into(), v.to_string()));
            }
            if let Some(v) = organization_id {
                q.push(("organization_id".into(), v));
            }
            if let Some(v) = status {
                q.push(("status".into(), v));
            }
            if let Some(v) = type_ {
                q.push(("type".into(), v));
            }
            let query = if q.is_empty() {
                String::new()
            } else {
                format!("?{}", q.into_iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&"))
            };
            print_list_envelope(&client.get(&format!("/jobs{query}")).await.context("GET jobs")?, "jobs");
        }
        Cmd::Show { job_id } => {
            print_json(&client.get(&format!("/jobs/{job_id}")).await.context("GET job")?);
        }
        Cmd::Create { body } => {
            let b = read_body(&body)?;
            print_json(&client.post("/jobs", Some(&b)).await.context("POST job")?);
        }
        Cmd::Update { job_id, body } => {
            let b = read_body(&body)?;
            print_json(&client.patch(&format!("/jobs/{job_id}"), &b).await.context("PATCH job")?);
        }
    }
    Ok(())
}
