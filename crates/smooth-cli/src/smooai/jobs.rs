//! `th jobs …` — the async job queue.

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// List jobs. Filterable via query params.
    List {
        /// Max number of jobs to return.
        #[arg(long)]
        limit: Option<u64>,
        /// Number of jobs to skip for pagination.
        #[arg(long)]
        offset: Option<u64>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        organization_id: Option<String>,
        /// Filter by job status (e.g. `pending`, `running`, `completed`).
        #[arg(long)]
        status: Option<String>,
        /// Filter by job type.
        #[arg(long, name = "type", value_name = "TYPE")]
        type_: Option<String>,
        /// Print raw JSON instead of the list.
        #[arg(long)]
        json: bool,
    },
    /// Show one job's full record (status, payload, result).
    Show {
        /// The job id from `th api jobs list`.
        job_id: String,
    },
    /// Create a job (JSON body); use `-` for stdin.
    Create {
        /// JSON job body, or `-` to read from stdin.
        body: String,
    },
    /// Patch an existing job with a partial JSON body.
    Update {
        /// The job id from `th api jobs list`.
        job_id: String,
        /// JSON patch body, or `-` to read from stdin.
        body: String,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List {
            limit,
            offset,
            organization_id,
            status,
            type_,
            json,
        } => {
            let mut q: Vec<(String, String)> = Vec::new();
            if let Some(v) = limit {
                q.push(("limit".into(), v.to_string()));
            }
            if let Some(v) = offset {
                q.push(("offset".into(), v.to_string()));
            }
            if let Some(v) = organization_id {
                // The API validates the camelCase spelling and 400s on `organization_id`.
                q.push(("organizationId".into(), v));
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
            let body = client.get(&format!("/jobs{query}")).await.context("GET jobs")?;
            if json {
                print_json(&body);
            } else {
                print_list_envelope(&body, "jobs");
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// CLI-Spec §flags: every platform `list` verb offers `--json`.
    #[test]
    fn list_accepts_json_flag_and_defaults_to_off() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(subcommand)]
            cmd: Cmd,
        }
        let on = Wrap::try_parse_from(["t", "list", "--json"]).expect("--json must parse");
        assert!(matches!(on.cmd, Cmd::List { json: true, .. }));

        let off = Wrap::try_parse_from(["t", "list"]).expect("bare list must still parse");
        assert!(matches!(off.cmd, Cmd::List { json: false, .. }), "--json must default to off");
    }
}
