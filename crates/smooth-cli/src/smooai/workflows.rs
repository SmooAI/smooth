//! `smoo workflows …` — "when X happens, do Y" automations (SmooAI ADR-127):
//! list / inspect / author drafts as JSON, publish + enable, run by hand and
//! read the step timeline. CLI twin of the canvas at `/apps/workflows` and the
//! hosted MCP `workflow_*` tools, over the same api-prime routes
//! (`/organizations/{orgId}/workflows…`). Pearl th-b1068f.
//!
//! Config round-trips as code: `export <id> > def.json`, edit, then
//! `update <id> --file def.json`. There is deliberately no `$EDITOR` verb
//! (CLAUDE.md §1a: interactive editor flows don't belong in `th`).
//!
//! What the server owns, and this module never re-implements:
//! - **Validation.** Drafts may be invalid; only publish validates. `show` and
//!   `validate` print the server's live `validation` block for the draft; a
//!   refused publish (422) is rendered one issue per line, pinned to its node.
//! - **Permissions.** Publishing needs `workflow.publish` AND every permission
//!   the graph's steps exercise; the 403 names the missing key and we say so.
//!
//! `run` sends real email / writes CRM records, so it previews unless
//! `--confirm` (CLI-Spec §3). `rm` and `cancel` go through the shared
//! [`crate::destructive`] gate (`--yes` / `--dry-run`).

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use anstream::{eprintln, print, println};
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};
use smooth_api_client::SmoothApiClient;

use super::{print_json, read_body, require_active_org, require_authed};

/// How long `run --wait` polls before handing back a still-running run.
const DEFAULT_WAIT_SECS: u64 = 120;
/// Poll interval for `run --wait`.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Resolved step input/output/error longer than this is cut in the timeline
/// (with a note) unless `--full`.
const TIMELINE_JSON_WIDTH: usize = 240;

/// `--json` + `--org` — shared by every verb.
#[derive(Args, Debug, Clone, Default)]
pub struct Out {
    /// Print the raw response JSON instead of the summary.
    #[arg(long)]
    pub json: bool,
    /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
    #[arg(long = "org-id", visible_alias = "org")]
    pub org: Option<String>,
}

/// A workflow's concurrency policy.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Concurrency {
    /// Every matching event starts a run.
    Allow,
    /// At most one active run per entity; later events are skipped.
    #[value(name = "skip_if_active", alias = "skip-if-active")]
    SkipIfActive,
}

impl Concurrency {
    const fn wire(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::SkipIfActive => "skip_if_active",
        }
    }
}

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's workflows — status, published version, triggers.
    List {
        /// Only workflows in this status: draft, active, paused, archived.
        #[arg(long)]
        status: Option<String>,
        /// Page size (the server caps it at 100).
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Rows to skip, for paging past `--limit`.
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[command(flatten)]
        out: Out,
    },
    /// One workflow: triggers in words, steps, validation, permissions.
    ///
    /// Shows the DRAFT (what `update` edits) and whether it differs from the
    /// published version, the live draft validation with every issue pinned
    /// to its node id, and the permissions publishing will need.
    #[command(visible_alias = "get")]
    Show {
        /// Workflow id (from `smoo workflows list`).
        id: String,
        #[command(flatten)]
        out: Out,
    },
    /// Published versions of a workflow, newest first.
    Versions {
        /// Workflow id.
        id: String,
        #[command(flatten)]
        out: Out,
    },
    /// Print a workflow's definition JSON — redirect it to a file.
    ///
    /// `smoo workflows export <id> > def.json`, edit, then
    /// `smoo workflows update <id> --file def.json`. The draft by default.
    Export {
        /// Workflow id.
        id: String,
        /// Export the PUBLISHED version's definition instead of the draft.
        #[arg(long)]
        published: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a DRAFT workflow from a definition file or a starter template.
    ///
    /// Nothing runs until you `publish` and `enable` it. The draft may be
    /// incomplete — the response shows what publish would still refuse.
    Create {
        /// Display name (1–120 chars).
        #[arg(long)]
        name: String,
        /// Optional description.
        #[arg(long)]
        description: Option<String>,
        /// Definition JSON file (`-` = stdin). A `show --json` dump works too:
        /// its `draftDefinition` is used.
        #[arg(long, required_unless_present = "from_template", conflicts_with = "from_template")]
        file: Option<String>,
        /// Start from a built-in template (see `smoo workflows templates`).
        #[arg(long)]
        from_template: Option<String>,
        /// Concurrency policy (server default: allow).
        #[arg(long, value_enum)]
        concurrency: Option<Concurrency>,
        #[command(flatten)]
        out: Out,
    },
    /// Edit a workflow's DRAFT — definition, name, description, concurrency.
    ///
    /// Never touches the published version: publish again to ship the edit.
    Update {
        /// Workflow id.
        id: String,
        /// New definition JSON file (`-` = stdin). A `show --json` dump works too.
        #[arg(long)]
        file: Option<String>,
        /// New display name.
        #[arg(long)]
        name: Option<String>,
        /// New description.
        #[arg(long, conflicts_with = "clear_description")]
        description: Option<String>,
        /// Remove the description.
        #[arg(long)]
        clear_description: bool,
        /// New concurrency policy.
        #[arg(long, value_enum)]
        concurrency: Option<Concurrency>,
        #[command(flatten)]
        out: Out,
    },
    /// The draft's server-side validation — what publish would say.
    ///
    /// Exits non-zero when the draft would be refused, so it gates CI.
    Validate {
        /// Workflow id.
        id: String,
        #[command(flatten)]
        out: Out,
    },
    /// Publish the draft as a new immutable version.
    ///
    /// Publishing does not change status: a first publish leaves the workflow
    /// `draft` until `enable` (or `--enable` here) turns its triggers on.
    Publish {
        /// Workflow id.
        id: String,
        /// Also enable it (status active, triggers live) after publishing.
        #[arg(long)]
        enable: bool,
        /// Required for an M2M key: the org member this publishes for. Their
        /// grants are checked and re-checked before every run.
        #[arg(long)]
        on_behalf_of: Option<String>,
        #[command(flatten)]
        out: Out,
    },
    /// Turn a published workflow's triggers on (status active).
    Enable {
        /// Workflow id.
        id: String,
        #[command(flatten)]
        out: Out,
    },
    /// Turn a workflow's triggers off (status paused). In-flight runs continue.
    Pause {
        /// Workflow id.
        id: String,
        #[command(flatten)]
        out: Out,
    },
    /// Delete a workflow with its versions and run history.
    ///
    /// Refused (409) while a run is in flight — cancel it first.
    #[command(visible_alias = "delete")]
    Rm {
        /// Workflow id.
        id: String,
        #[command(flatten)]
        confirm: crate::destructive::Confirm,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Run a published workflow now. Previews unless --confirm.
    ///
    /// Its steps have real effects (email, CRM writes, webhooks). Without
    /// `--confirm` this prints what would run and the request body. The run is
    /// pinned to the PUBLISHED version, not the draft.
    Run {
        /// Workflow id.
        id: String,
        /// Run against this contact (entity crm_contact).
        #[arg(long, conflicts_with_all = ["deal", "entity"])]
        contact: Option<String>,
        /// Run against this deal (entity crm_deal).
        #[arg(long, conflicts_with = "entity")]
        deal: Option<String>,
        /// Run against any entity, as `type:id` (e.g. `crm_task:<uuid>`).
        #[arg(long)]
        entity: Option<String>,
        /// Run input as `key=value`, repeatable. A value that parses as JSON
        /// (`3`, `true`, `{"a":1}`) is sent as JSON, anything else as a string.
        #[arg(long = "input", value_name = "KEY=VALUE")]
        input: Vec<String>,
        /// Run input from a JSON-object file (`-` = stdin); `--input` pairs override its keys.
        #[arg(long)]
        input_file: Option<String>,
        /// Actually start the run. Without it, nothing is sent.
        #[arg(long)]
        confirm: bool,
        /// Poll until the run finishes, then print its step timeline.
        #[arg(long)]
        wait: bool,
        /// With --wait: give up polling after this many seconds (the run keeps going).
        #[arg(long, default_value_t = DEFAULT_WAIT_SECS)]
        timeout: u64,
        #[command(flatten)]
        out: Out,
    },
    /// A workflow's runs, newest first.
    Runs {
        /// Workflow id.
        id: String,
        /// Only runs in this status: running, completed, failed, cancelled, blocked_permission, skipped.
        #[arg(long)]
        status: Option<String>,
        /// Only the run started by this event id (from `run`'s response).
        #[arg(long)]
        event_id: Option<String>,
        /// Only the run with this Temporal workflow id.
        #[arg(long)]
        temporal_id: Option<String>,
        /// Page size (the server caps it at 100).
        #[arg(long, default_value_t = 25)]
        limit: u32,
        /// Rows to skip.
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[command(flatten)]
        out: Out,
    },
    /// One run's step timeline — resolved input, output, error per step.
    #[command(name = "run-show", visible_alias = "run-get")]
    RunShow {
        /// Workflow id.
        id: String,
        /// Run id (from `smoo workflows runs`).
        run_id: String,
        /// Don't truncate step input/output/error.
        #[arg(long)]
        full: bool,
        #[command(flatten)]
        out: Out,
    },
    /// Cancel an in-flight run (terminates it; no further steps start).
    Cancel {
        /// Workflow id.
        id: String,
        /// Run id.
        run_id: String,
        #[command(flatten)]
        confirm: crate::destructive::Confirm,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// The events a trigger can listen for, with their fields.
    EventTypes {
        #[command(flatten)]
        out: Out,
    },
    /// The step palette, with the permission each step needs to publish.
    StepTypes {
        #[command(flatten)]
        out: Out,
    },
    /// Built-in starter definitions for `create --from-template`.
    Templates {
        /// Print the templates' full definitions as JSON.
        #[arg(long)]
        json: bool,
    },
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)] // one arm per verb, same shape as the sibling modules
pub async fn cmd(cmd: Cmd) -> Result<()> {
    // Templates are local — no session needed to browse them.
    if let Cmd::Templates { json: as_json } = &cmd {
        if *as_json {
            let all: Vec<Value> = TEMPLATES
                .iter()
                .map(|t| json!({ "id": t.id, "name": t.name, "description": t.description, "definition": (t.definition)() }))
                .collect();
            print_json(&Value::Array(all));
        } else {
            print!("{}", render_templates());
        }
        return Ok(());
    }

    let client = require_authed().await?;
    match cmd {
        Cmd::Templates { .. } => unreachable!("handled above"),
        Cmd::List { status, limit, offset, out } => {
            let org = require_active_org(&client, out.org)?;
            let body = client
                .get(&list_path(&org, status.as_deref(), limit, offset))
                .await
                .map_err(|e| explain(e, "list workflows", None))?;
            emit(&body, out.json, || render_list(&body, offset));
        }
        Cmd::Show { id, out } => {
            let org = require_active_org(&client, out.org)?;
            let body = get_workflow(&client, &org, &id).await?;
            if out.json {
                print_json(&body);
                return Ok(());
            }
            // Best-effort: the catalog only upgrades `crm_contact.tagged` to
            // "Tag added to contact" — a failure must not hide the workflow itself.
            let catalog = client.get(&format!("/organizations/{org}/workflow-event-types")).await.ok();
            print!("{}", render_workflow(&body, catalog.as_ref()));
        }
        Cmd::Versions { id, out } => {
            let org = require_active_org(&client, out.org)?;
            let body = client
                .get(&format!("{}/versions", workflow_path(&org, &id)))
                .await
                .map_err(|e| explain(e, "list versions", Some(&id)))?;
            emit(&body, out.json, || render_versions(&body));
        }
        Cmd::Export { id, published, org } => {
            let org = require_active_org(&client, org)?;
            let body = get_workflow(&client, &org, &id).await?;
            let def = export_definition(&body, published)?;
            // Plain stdout, no leading blank line: this is meant for `> def.json`.
            println!("{}", serde_json::to_string_pretty(&def).unwrap_or_default());
        }
        Cmd::Create {
            name,
            description,
            file,
            from_template,
            concurrency,
            out,
        } => {
            let org = require_active_org(&client, out.org)?;
            let definition = match (file, from_template) {
                (Some(path), _) => definition_from_file(&path)?,
                (None, Some(t)) => template_definition(&t)?,
                (None, None) => bail!("pass --file <def.json> or --from-template <name> (see `smoo workflows templates`)"),
            };
            let req = create_body(&name, description.as_deref(), &definition, concurrency);
            let body = client
                .post(&format!("/organizations/{org}/workflows"), Some(&req))
                .await
                .map_err(|e| explain(e, "create workflow", None))?;
            emit(&body, out.json, || {
                let id = str_at(&body, "id");
                format!(
                    "\n  {} draft workflow {} created {}\n{}\n  {}\n\n",
                    "✚".green(),
                    str_at(&body, "name").bold(),
                    id.dimmed(),
                    render_validation(body.get("validation")),
                    format!("next: smoo workflows publish {id}   (nothing runs until it is published and enabled)").dimmed()
                )
            });
        }
        Cmd::Update {
            id,
            file,
            name,
            description,
            clear_description,
            concurrency,
            out,
        } => {
            let org = require_active_org(&client, out.org)?;
            let definition = file.as_deref().map(definition_from_file).transpose()?;
            let req = update_body(definition, name.as_deref(), description.as_deref(), clear_description, concurrency)?;
            let body = client
                .patch(&workflow_path(&org, &id), &req)
                .await
                .map_err(|e| explain(e, "update workflow", Some(&id)))?;
            emit(&body, out.json, || {
                format!(
                    "\n  {} draft of {} saved {}\n{}\n  {}\n\n",
                    "✓".green(),
                    str_at(&body, "name").bold(),
                    str_at(&body, "id").dimmed(),
                    render_validation(body.get("validation")),
                    draft_saved_note(&body).dimmed()
                )
            });
        }
        Cmd::Validate { id, out } => {
            let org = require_active_org(&client, out.org)?;
            let body = get_workflow(&client, &org, &id).await?;
            let validation = body.get("validation").cloned().unwrap_or(Value::Null);
            if out.json {
                print_json(&validation);
            } else {
                print!(
                    "\n  {} {}\n{}\n",
                    str_at(&body, "name").bold(),
                    str_at(&body, "id").dimmed(),
                    render_validation(Some(&validation))
                );
            }
            let problems = issue_count(&validation);
            if problems > 0 {
                bail!("the draft would be refused by publish ({problems} problem(s))\nfix them, then: smoo workflows update {id} --file def.json");
            }
        }
        Cmd::Publish { id, enable, on_behalf_of, out } => {
            let org = require_active_org(&client, out.org)?;
            let base = workflow_path(&org, &id);
            let published = client
                .post(&format!("{base}/publish"), Some(&publish_body(on_behalf_of.as_deref())))
                .await
                .map_err(|e| explain(e, "publish", Some(&id)))?;
            let enabled = if enable {
                Some(
                    client
                        .post(&format!("{base}/enable"), Some(&json!({})))
                        .await
                        .map_err(|e| explain(e, "enable", Some(&id)))?,
                )
            } else {
                None
            };
            if out.json {
                print_json(
                    &enabled
                        .as_ref()
                        .map_or_else(|| published.clone(), |w| json!({ "publish": published, "workflow": w })),
                );
                return Ok(());
            }
            print!("{}", render_publish(&published, &id));
            match &enabled {
                Some(w) => print!("{}", render_status_change(w, "enabled")),
                None => println!(
                    "  {}\n",
                    format!("status unchanged — `smoo workflows enable {id}` turns its triggers on").dimmed()
                ),
            }
        }
        Cmd::Enable { id, out } => {
            let org = require_active_org(&client, out.org)?;
            let body = client
                .post(&format!("{}/enable", workflow_path(&org, &id)), Some(&json!({})))
                .await
                .map_err(|e| explain(e, "enable", Some(&id)))?;
            emit(&body, out.json, || render_status_change(&body, "enabled"));
        }
        Cmd::Pause { id, out } => {
            let org = require_active_org(&client, out.org)?;
            let body = client
                .post(&format!("{}/pause", workflow_path(&org, &id)), Some(&json!({})))
                .await
                .map_err(|e| explain(e, "pause", Some(&id)))?;
            emit(&body, out.json, || render_status_change(&body, "paused"));
        }
        Cmd::Rm { id, confirm, org } => {
            let org = require_active_org(&client, org)?;
            // Read it first: a wrong id fails here as a clean 404, and the
            // banner names the workflow the operator is about to lose.
            let wf = get_workflow(&client, &org, &id).await?;
            let label = format!("{id} ({})", str_at(&wf, "name"));
            if !crate::destructive::gate_with(
                &crate::destructive::Target {
                    verb: "delete",
                    noun: "workflow (with its versions and run history)",
                    id: &label,
                    org: &org,
                    severity: crate::destructive::Severity::Standard,
                },
                confirm,
            )? {
                return Ok(());
            }
            client.delete(&workflow_path(&org, &id)).await.map_err(|e| explain(e, "delete", Some(&id)))?;
            println!("  {} workflow {} deleted\n", "✓".green(), label.bold());
        }
        Cmd::Run {
            id,
            contact,
            deal,
            entity,
            input,
            input_file,
            confirm,
            wait,
            timeout,
            out,
        } => {
            let org = require_active_org(&client, out.org)?;
            let target = parse_entity(contact.as_deref(), deal.as_deref(), entity.as_deref())?;
            let file_input = input_file.as_deref().map(read_body).transpose()?;
            let input = parse_inputs(file_input, &input)?;
            let req = run_body(target.as_ref(), input);
            if !confirm {
                let wf = get_workflow(&client, &org, &id).await?;
                print!("{}", render_run_preview(&wf, &req));
                // A preview of a run that `--confirm` would refuse (409) must
                // not read as success to a script.
                if wf.get("published").is_none_or(Value::is_null) {
                    bail!("nothing to run — {id} has never been published");
                }
                return Ok(());
            }
            let started = client
                .post(&format!("{}/run", workflow_path(&org, &id)), Some(&req))
                .await
                .map_err(|e| explain(e, "run", Some(&id)))?;
            let event_id = str_at(&started, "eventId").to_string();
            if !wait {
                emit(&started, out.json, || {
                    format!(
                        "\n  {} run requested — event {}\n    temporal id {}\n  {}\n\n",
                        "▶".green(),
                        event_id.cyan(),
                        str_at(&started, "temporalWorkflowId").dimmed(),
                        format!("follow it: smoo workflows runs {id} --event-id {event_id}").dimmed()
                    )
                });
                return Ok(());
            }
            let detail = wait_for_run(&client, &org, &id, &event_id, Duration::from_secs(timeout)).await?;
            if let Some(run) = detail {
                emit(&run, out.json, || render_run_detail(&run, false));
            } else {
                if out.json {
                    print_json(&started);
                }
                eprintln!(
                    "  {} no run recorded for event {event_id} after {timeout}s — the trigger consumer may be behind.\n    check later: smoo workflows runs {id} --event-id {event_id}",
                    "◐".yellow()
                );
            }
        }
        Cmd::Runs {
            id,
            status,
            event_id,
            temporal_id,
            limit,
            offset,
            out,
        } => {
            let org = require_active_org(&client, out.org)?;
            let path = runs_path(&org, &id, status.as_deref(), event_id.as_deref(), temporal_id.as_deref(), limit, offset);
            let body = client.get(&path).await.map_err(|e| explain(e, "list runs", Some(&id)))?;
            emit(&body, out.json, || render_runs(&body, &id, offset));
        }
        Cmd::RunShow { id, run_id, full, out } => {
            let org = require_active_org(&client, out.org)?;
            let body = client.get(&run_path(&org, &id, &run_id)).await.map_err(|e| explain(e, "show run", Some(&id)))?;
            emit(&body, out.json, || render_run_detail(&body, full));
        }
        Cmd::Cancel { id, run_id, confirm, org } => {
            let org = require_active_org(&client, org)?;
            if !crate::destructive::gate_with(
                &crate::destructive::Target {
                    verb: "cancel",
                    noun: "workflow run",
                    id: &run_id,
                    org: &org,
                    severity: crate::destructive::Severity::Standard,
                },
                confirm,
            )? {
                return Ok(());
            }
            let body = client
                .post(&format!("{}/cancel", run_path(&org, &id, &run_id)), Some(&json!({})))
                .await
                .map_err(|e| explain(e, "cancel", Some(&id)))?;
            println!("  {} run {} is now {}\n", "✓".green(), run_id.bold(), str_at(&body, "status"));
        }
        Cmd::EventTypes { out } => {
            let org = require_active_org(&client, out.org)?;
            let body = client
                .get(&format!("/organizations/{org}/workflow-event-types"))
                .await
                .map_err(|e| explain(e, "list event types", None))?;
            emit(&body, out.json, || render_event_types(&body));
        }
        Cmd::StepTypes { out } => {
            let org = require_active_org(&client, out.org)?;
            let body = client
                .get(&format!("/organizations/{org}/workflow-step-types"))
                .await
                .map_err(|e| explain(e, "list step types", None))?;
            emit(&body, out.json, || render_step_types(&body));
        }
    }
    Ok(())
}

/// `--json` prints the response verbatim; otherwise the rendered summary.
fn emit(body: &Value, as_json: bool, render: impl FnOnce() -> String) {
    if as_json {
        print_json(body);
    } else {
        print!("{}", render());
    }
}

async fn get_workflow(client: &SmoothApiClient, org: &str, id: &str) -> Result<Value> {
    client.get(&workflow_path(org, id)).await.map_err(|e| explain(e, "show workflow", Some(id)))
}

/// Poll `runs?eventId=` until the run the trigger consumer started for
/// `event_id` reaches a terminal status, then fetch its detail. Returns the
/// detail as soon as the run exists and is terminal, the latest detail at the
/// deadline if it exists but is still going, and `None` if it never appeared.
async fn wait_for_run(client: &SmoothApiClient, org: &str, id: &str, event_id: &str, timeout: Duration) -> Result<Option<Value>> {
    let deadline = Instant::now() + timeout;
    let path = runs_path(org, id, None, Some(event_id), None, 1, 0);
    let mut last_status = String::new();
    loop {
        let body = client.get(&path).await.map_err(|e| explain(e, "poll run", Some(id)))?;
        if let Some(run) = rows(&body).first() {
            let status = str_at(run, "status").to_string();
            if status != last_status {
                eprintln!("  {} run {} {}", "◐".cyan(), str_at(run, "id").dimmed(), status);
                last_status.clone_from(&status);
            }
            if is_terminal_run_status(&status) || Instant::now() >= deadline {
                let run_id = str_at(run, "id").to_string();
                let detail = client.get(&run_path(org, id, &run_id)).await.map_err(|e| explain(e, "show run", Some(id)))?;
                if !is_terminal_run_status(&status) {
                    eprintln!(
                        "  {} still {status} after {}s (a wait step can hold a run for days) — check later: smoo workflows run-show {id} {run_id}",
                        "◐".yellow(),
                        timeout.as_secs()
                    );
                }
                return Ok(Some(detail));
            }
        } else if Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// Request building (pure — pinned by tests against the wire contract)
// ---------------------------------------------------------------------------

fn enc(s: &str) -> String {
    urlencoding::encode(s.trim()).into_owned()
}

fn workflow_path(org: &str, id: &str) -> String {
    format!("/organizations/{org}/workflows/{}", enc(id))
}

fn run_path(org: &str, id: &str, run_id: &str) -> String {
    format!("{}/runs/{}", workflow_path(org, id), enc(run_id))
}

fn push_query(path: &mut String, key: &str, value: &str) {
    let sep = if path.contains('?') { '&' } else { '?' };
    let _ = write!(path, "{sep}{key}={}", enc(value));
}

fn list_path(org: &str, status: Option<&str>, limit: u32, offset: u32) -> String {
    let mut path = format!("/organizations/{org}/workflows");
    if let Some(s) = status.map(str::trim).filter(|s| !s.is_empty()) {
        push_query(&mut path, "status", s);
    }
    push_query(&mut path, "limit", &limit.to_string());
    if offset > 0 {
        push_query(&mut path, "offset", &offset.to_string());
    }
    path
}

fn runs_path(org: &str, id: &str, status: Option<&str>, event_id: Option<&str>, temporal_id: Option<&str>, limit: u32, offset: u32) -> String {
    let mut path = format!("{}/runs", workflow_path(org, id));
    for (key, value) in [("status", status), ("eventId", event_id), ("temporalWorkflowId", temporal_id)] {
        if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
            push_query(&mut path, key, v);
        }
    }
    push_query(&mut path, "limit", &limit.to_string());
    if offset > 0 {
        push_query(&mut path, "offset", &offset.to_string());
    }
    path
}

/// `POST /workflows` body: `{ name, description?, definition, concurrencyPolicy? }`.
fn create_body(name: &str, description: Option<&str>, definition: &Value, concurrency: Option<Concurrency>) -> Value {
    let mut body = json!({ "name": name, "definition": definition });
    if let Some(d) = description {
        body["description"] = json!(d);
    }
    if let Some(c) = concurrency {
        body["concurrencyPolicy"] = json!(c.wire());
    }
    body
}

/// `PATCH /workflows/{id}` body. A present key sets the field (`description:
/// null` clears it); an absent key keeps it — so only what was flagged is sent.
fn update_body(
    definition: Option<Value>,
    name: Option<&str>,
    description: Option<&str>,
    clear_description: bool,
    concurrency: Option<Concurrency>,
) -> Result<Value> {
    let mut body = Map::new();
    if let Some(def) = definition {
        body.insert("definition".into(), def);
    }
    if let Some(n) = name {
        body.insert("name".into(), json!(n));
    }
    if let Some(d) = description {
        body.insert("description".into(), json!(d));
    } else if clear_description {
        body.insert("description".into(), Value::Null);
    }
    if let Some(c) = concurrency {
        body.insert("concurrencyPolicy".into(), json!(c.wire()));
    }
    if body.is_empty() {
        bail!("nothing to update — pass at least one of --file / --name / --description / --clear-description / --concurrency");
    }
    Ok(Value::Object(body))
}

/// `POST /publish` body. A user token publishes as itself; an M2M key must
/// name the member it publishes for.
fn publish_body(on_behalf_of: Option<&str>) -> Value {
    on_behalf_of
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| json!({}), |u| json!({ "onBehalfOfUserId": u }))
}

/// `POST /run` body: `{ entity?: { type, id }, input?: {…} }`.
fn run_body(entity: Option<&(String, String)>, input: Map<String, Value>) -> Value {
    let mut body = json!({});
    if let Some((kind, id)) = entity {
        body["entity"] = json!({ "type": kind, "id": id });
    }
    if !input.is_empty() {
        body["input"] = Value::Object(input);
    }
    body
}

/// `--contact` / `--deal` / `--entity type:id` → the run's target entity.
/// Entity types are the domain-event catalog's (`crm_contact`, `crm_deal`,
/// `crm_task`) — a step that defaults to "the triggering contact" only picks
/// up a target of exactly that type.
fn parse_entity(contact: Option<&str>, deal: Option<&str>, entity: Option<&str>) -> Result<Option<(String, String)>> {
    let nonempty = |flag: &str, v: &str| -> Result<String> {
        let v = v.trim();
        if v.is_empty() {
            bail!("--{flag} needs a value");
        }
        Ok(v.to_string())
    };
    if let Some(c) = contact {
        return Ok(Some(("crm_contact".into(), nonempty("contact", c)?)));
    }
    if let Some(d) = deal {
        return Ok(Some(("crm_deal".into(), nonempty("deal", d)?)));
    }
    let Some(raw) = entity else { return Ok(None) };
    match raw.split_once(':') {
        Some((kind, id)) if !kind.trim().is_empty() && !id.trim().is_empty() => Ok(Some((kind.trim().to_string(), id.trim().to_string()))),
        _ => bail!("--entity must be `type:id`, e.g. `crm_contact:3f2c…` (got `{raw}`)"),
    }
}

/// Merge `--input-file` (a JSON object) with `--input key=value` pairs; pairs
/// win. A value that parses as JSON is sent as JSON, otherwise as a string —
/// so `count=3` is a number and `name=Ada` is text.
fn parse_inputs(file: Option<Value>, pairs: &[String]) -> Result<Map<String, Value>> {
    let mut input = match file {
        None => Map::new(),
        Some(Value::Object(m)) => m,
        Some(_) => bail!("--input-file must hold a JSON object"),
    };
    for pair in pairs {
        let Some((key, value)) = pair.split_once('=') else {
            bail!("--input expects key=value (got `{pair}`)");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("--input key is empty in `{pair}`");
        }
        let parsed = serde_json::from_str::<Value>(value).unwrap_or_else(|_| Value::String(value.to_string()));
        input.insert(key.to_string(), parsed);
    }
    Ok(input)
}

/// Read a definition file. A `show --json` dump (a whole Workflow) is
/// accepted too and its `draftDefinition` used, so `show --json > wf.json` →
/// edit → `update --file wf.json` works as well as `export`.
fn definition_from_file(path: &str) -> Result<Value> {
    definition_from_value(read_body(path)?).with_context(|| format!("definition in {path}"))
}

fn definition_from_value(value: Value) -> Result<Value> {
    let Value::Object(map) = value else {
        bail!("a workflow definition must be a JSON object");
    };
    if !map.contains_key("schemaVersion") {
        if let Some(def) = map.get("draftDefinition") {
            return Ok(def.clone());
        }
    }
    Ok(Value::Object(map))
}

/// The definition `export` prints: the draft, or the published version's.
fn export_definition(workflow: &Value, published: bool) -> Result<Value> {
    if published {
        return workflow
            .get("published")
            .and_then(|p| p.get("definition"))
            .cloned()
            .context("this workflow has never been published — drop --published to export the draft");
    }
    workflow.get("draftDefinition").cloned().context("the response carried no draftDefinition")
}

// ---------------------------------------------------------------------------
// Templates — the canvas's "New workflow" starters
// (apps/web/components/workflows/templates.ts), same ids. `layout` is omitted
// and the canvas auto-lays it out. Field names follow the LIVE event catalog
// (`smoo workflows event-types`), not the canvas copy: a tag event carries
// `after.tagName` (the canvas's `after.tag` is always null, so its filter
// never matches) and a deal's name is `title`.
// ---------------------------------------------------------------------------

struct Template {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    definition: fn() -> Value,
}

const TEMPLATES: &[Template] = &[
    Template {
        id: "blank",
        name: "Blank workflow",
        description: "An empty draft — add steps on the canvas or with `update --file`.",
        definition: || json!({ "schemaVersion": 1, "triggers": [], "startStepId": "start", "steps": {} }),
    },
    Template {
        id: "tag-added-send-email",
        name: "Tag added → send email",
        description: "When a contact gets the VIP tag, send them a welcome email.",
        definition: || {
            json!({
                "schemaVersion": 1,
                "triggers": [{ "id": "on_tag_added", "eventType": "crm_contact.tagged", "filter": { "and": [{ "==": [{ "var": "after.tagName" }, "VIP"] }] } }],
                "startStepId": "send_email",
                "steps": {
                    "send_email": {
                        "type": "send_email",
                        "label": "Welcome email",
                        "config": { "to": "{{entity.email}}", "subject": "Welcome, {{entity.firstName}}", "body": "Hi {{entity.firstName}},\n\nGlad to have you with us." },
                        "next": null
                    }
                }
            })
        },
    },
    Template {
        id: "deal-won-task-notify",
        name: "Deal won → task + notify",
        description: "When a deal is won, create a kick-off task and tell the admins.",
        definition: || {
            json!({
                "schemaVersion": 1,
                "triggers": [{ "id": "on_deal_won", "eventType": "crm_deal.stage_changed", "filter": { "and": [{ "==": [{ "var": "after.stage" }, "won"] }] } }],
                "startStepId": "create_task",
                "steps": {
                    "create_task": { "type": "create_task", "label": "Kick-off task", "config": { "title": "Kick off {{entity.title}}", "dueInDays": 2 }, "next": "notify" },
                    "notify": {
                        "type": "notify",
                        "label": "Tell the team",
                        "config": { "title": "Deal won: {{entity.title}}", "body": "A kick-off task has been created.", "recipients": "org_admins" },
                        "next": null
                    }
                }
            })
        },
    },
    Template {
        id: "contact-created-wait-email",
        name: "New contact → wait a day → email",
        description: "A day after a contact is created, send a follow-up.",
        definition: || {
            json!({
                "schemaVersion": 1,
                "triggers": [{ "id": "on_contact_created", "eventType": "crm_contact.created" }],
                "startStepId": "wait",
                "steps": {
                    "wait": { "type": "wait", "label": "Wait a day", "config": { "duration": "P1D" }, "next": "send_email" },
                    "send_email": {
                        "type": "send_email",
                        "label": "Follow-up email",
                        "config": { "to": "{{entity.email}}", "subject": "Nice to meet you, {{entity.firstName}}", "body": "Hi {{entity.firstName}},\n\nJust following up." },
                        "next": null
                    }
                }
            })
        },
    },
];

fn template_definition(id: &str) -> Result<Value> {
    TEMPLATES.iter().find(|t| t.id == id.trim()).map(|t| (t.definition)()).with_context(|| {
        let ids: Vec<&str> = TEMPLATES.iter().map(|t| t.id).collect();
        format!("no template `{id}` — one of: {}", ids.join(", "))
    })
}

fn render_templates() -> String {
    let mut out = String::from("\n");
    let _ = writeln!(out, "  {}\n", "Starter templates".bold());
    for t in TEMPLATES {
        let _ = writeln!(out, "  {}  {}", format!("{:<28}", t.id).cyan(), t.name.bold());
        let _ = writeln!(out, "  {:<28}  {}", "", t.description.dimmed());
    }
    let _ = writeln!(out, "\n  {}\n", "smoo workflows create --name \"…\" --from-template <id>".dimmed());
    out
}

// ---------------------------------------------------------------------------
// Error rendering — CLI-Spec §4: what failed (server words), then what to do
// ---------------------------------------------------------------------------

/// A non-2xx response recovered from the client's
/// `"{method} {path} returned HTTP {status}: {body}"` error.
#[derive(Debug, PartialEq)]
struct HttpFailure {
    status: u16,
    body: Value,
}

fn http_failure(err: &anyhow::Error) -> Option<HttpFailure> {
    err.chain().find_map(|e| parse_http_failure(&e.to_string()))
}

fn parse_http_failure(msg: &str) -> Option<HttpFailure> {
    let rest = &msg[msg.find("returned HTTP ")? + "returned HTTP ".len()..];
    let status: u16 = rest.get(..3)?.parse().ok()?;
    let text = rest.split_once(": ").map_or("", |(_, t)| t).trim();
    let body = serde_json::from_str(text).unwrap_or_else(|_| if text.is_empty() { Value::Null } else { json!({ "message": text }) });
    Some(HttpFailure { status, body })
}

/// The server's own words, whichever key it used.
fn server_message(body: &Value) -> String {
    ["message", "error", "reason"]
        .iter()
        .find_map(|k| body.get(*k).and_then(Value::as_str))
        .map_or_else(|| body.to_string(), str::to_string)
}

/// Turn an API failure into the two-part error the spec asks for. Anything
/// that isn't an HTTP failure passes through untouched.
fn explain(err: anyhow::Error, action: &str, id: Option<&str>) -> anyhow::Error {
    let Some(f) = http_failure(&err) else { return err };
    anyhow::anyhow!("{}", explain_failure(&f, action, id.unwrap_or("<id>")))
}

fn explain_failure(f: &HttpFailure, action: &str, id: &str) -> String {
    let body = &f.body;
    match f.status {
        422 if body.get("error").and_then(Value::as_str) == Some("invalid_workflow") => {
            let n = issue_count(body);
            format!(
                "{action} refused — the draft has {n} problem(s):\n{}\nfix the definition (smoo workflows export {id} > def.json), save it (smoo workflows update {id} --file def.json), then publish again",
                render_issues(body)
            )
        }
        403 => match body.get("missingPermission").and_then(Value::as_str) {
            Some(key) if body.get("reason").and_then(Value::as_str).is_some() => format!(
                "{action} refused — this workflow's steps need `{key}`, which your role does not hold ({})\nask an org admin to grant `{key}`, or remove the steps that need it (smoo workflows step-types lists each step's permission)",
                server_message(body)
            ),
            Some(key) => format!("{action} refused — your role lacks `{key}`\nask an org admin to grant it (workflow.read / .write / .publish / .run gate these verbs)"),
            None => format!(
                "{action} refused (403): {}\nworkflows are gated by the org's `workflows` product feature — check it is enabled for this org (--org)",
                server_message(body)
            ),
        },
        404 => format!("{action}: {}\nids are org-scoped: check the id and the org (--org / `smoo org show`)", server_message(body)),
        409 => format!("{action} refused (409): {}\n{}", server_message(body), conflict_hint(action, id)),
        400 | 413 => format!("{action} rejected ({}): {}\nsee `smoo workflows {} --help` for the accepted shape", f.status, server_message(body), help_verb(action)),
        503 => format!("{action} unavailable (503): {}\nthe workflow engine is unreachable right now — nothing was started; retry shortly", server_message(body)),
        s => format!("{action} failed (HTTP {s}): {}", server_message(body)),
    }
}

fn conflict_hint(action: &str, id: &str) -> String {
    match action {
        "enable" | "pause" | "run" => format!("publish it first: smoo workflows publish {id}"),
        "delete" => format!("a run is still in flight — find it with `smoo workflows runs {id} --status running`, then `smoo workflows cancel {id} <runId>`"),
        "cancel" => "that run has already finished — nothing to cancel".to_string(),
        _ => format!("see `smoo workflows show {id}`"),
    }
}

fn help_verb(action: &str) -> &'static str {
    match action {
        "create workflow" => "create",
        "update workflow" => "update",
        "run" => "run",
        "publish" => "publish",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Rendering (pure)
// ---------------------------------------------------------------------------

fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("—")
}

fn rows(body: &Value) -> Vec<Value> {
    body.get("items").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn issue_count(validation: &Value) -> usize {
    let shape = usize::from(validation.get("shapeError").and_then(Value::as_str).is_some_and(|s| !s.is_empty()));
    shape + validation.get("graphIssues").and_then(Value::as_array).map_or(0, Vec::len)
}

/// Status glyph + word — the glyph carries the meaning when colour is off.
/// Padded to `width` AFTER choosing the glyph — matching on a padded word
/// would miss every status.
fn workflow_status_cell(status: &str, width: usize) -> String {
    let word = pad(status, width);
    match status {
        "active" => format!("{} {word}", "●".green()),
        "paused" => format!("{} {word}", "◐".yellow()),
        _ => format!("{} {word}", "○".dimmed()),
    }
}

fn run_status_cell(status: &str, width: usize) -> String {
    let word = pad(status, width);
    match status {
        "completed" | "succeeded" => format!("{} {word}", "✓".green()),
        "failed" => format!("{} {word}", "✗".red()),
        "running" => format!("{} {word}", "◐".cyan()),
        "waiting" => format!("{} {word}", "◐".yellow()),
        "blocked_permission" => format!("{} {word}", "⊘".magenta()),
        _ => format!("{} {word}", "○".dimmed()),
    }
}

fn is_terminal_run_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "blocked_permission" | "skipped")
}

/// Left-align to `width` chars, cutting with `…` when longer; 0 = as is.
fn pad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if width == 0 {
        s.to_string()
    } else if n > width {
        let cut: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        format!("{s}{}", " ".repeat(width - n))
    }
}

fn parse_ts(v: Option<&Value>) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(v?.as_str()?).ok()
}

/// `2026-09-22 12:03Z` — compact, sortable.
fn short_ts(v: Option<&Value>) -> String {
    parse_ts(v).map_or_else(|| "—".to_string(), |t| t.with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%MZ").to_string())
}

fn duration_between(start: Option<&Value>, end: Option<&Value>) -> Option<String> {
    let ms = (parse_ts(end)? - parse_ts(start)?).num_milliseconds();
    Some(human_ms(ms))
}

fn human_ms(ms: i64) -> String {
    match ms {
        i64::MIN..=999 => format!("{}ms", ms.max(0)),
        1000..=59_999 => format!("{}.{}s", ms / 1000, (ms % 1000) / 100),
        60_000..=3_599_999 => format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
        86_400_000.. => format!("{}d{}h", ms / 86_400_000, (ms % 86_400_000) / 3_600_000),
        _ => format!("{}h{:02}m", ms / 3_600_000, (ms % 3_600_000) / 60_000),
    }
}

fn truncation_note(shown: usize, offset: u32, total: Option<u64>, noun: &str) -> Option<String> {
    let total = total?;
    let end = u64::from(offset) + shown as u64;
    (end < total).then(|| format!("  showing {}–{end} of {total} {noun} — page with --offset {end}\n", u64::from(offset) + 1))
}

fn render_list(body: &Value, offset: u32) -> String {
    let items = rows(body);
    let total = body.get("total").and_then(Value::as_u64);
    let mut out = String::from("\n");
    let _ = writeln!(
        out,
        "  {} {}",
        "Workflows".bold(),
        format!("({})", total.unwrap_or(items.len() as u64)).dimmed()
    );
    if items.is_empty() {
        let _ = writeln!(
            out,
            "\n  {}\n",
            "no workflows matched — a confirmed empty read, not an error. Start one: smoo workflows create --name … --from-template <id>".dimmed()
        );
        return out;
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {}  {}  {}  {}  {}  {}",
        pad("ID", 36).dimmed(),
        pad("STATUS", 10).dimmed(),
        pad("VER", 4).dimmed(),
        pad("NAME", 34).dimmed(),
        pad("LAST RUN", 17).dimmed(),
        "TRIGGERS".dimmed()
    );
    for w in &items {
        let status = str_at(w, "status");
        let ver = w
            .get("publishedVersion")
            .and_then(Value::as_i64)
            .map_or_else(|| "—".to_string(), |v| format!("v{v}"));
        let triggers = w
            .get("triggerEventTypes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "manual only".to_string());
        let _ = writeln!(
            out,
            "  {}  {}  {}  {}  {}  {}",
            str_at(w, "id").cyan(),
            // pad the plain word, then colour — ANSI codes would break the width.
            workflow_status_cell(status, 8),
            pad(&ver, 4),
            pad(str_at(w, "name"), 34).bold(),
            pad(&short_ts(w.get("lastRunAt")), 17).dimmed(),
            triggers.dimmed()
        );
    }
    if let Some(note) = truncation_note(items.len(), offset, total, "workflows") {
        let _ = write!(out, "\n{}", note.dimmed());
    }
    out.push('\n');
    out
}

/// JsonLogic → something a human reads: `after.tagName == "VIP"`. Falls back to
/// compact JSON for anything outside the simple comparison/boolean subset.
fn jsonlogic_words(rule: &Value) -> String {
    match rule {
        Value::Bool(b) => b.to_string(),
        Value::Object(m) if m.len() == 1 => {
            let Some((op, args)) = m.iter().next() else { return rule.to_string() };
            let list: Vec<&Value> = args.as_array().map_or_else(|| vec![args], |a| a.iter().collect());
            match (op.as_str(), list.as_slice()) {
                ("var", [path, ..]) => path.as_str().map_or_else(|| path.to_string(), str::to_string),
                ("and" | "or", parts) if !parts.is_empty() => {
                    let joined = parts.iter().map(|p| jsonlogic_words(p)).collect::<Vec<_>>().join(&format!(" {op} "));
                    if parts.len() > 1 {
                        format!("({joined})")
                    } else {
                        joined
                    }
                }
                ("!", [inner]) => format!("not {}", jsonlogic_words(inner)),
                ("==" | "===" | "!=" | "!==" | ">" | ">=" | "<" | "<=" | "in", [a, b]) => {
                    let op = match op.as_str() {
                        "===" => "==",
                        "!==" => "!=",
                        o => o,
                    };
                    format!("{} {op} {}", jsonlogic_words(a), jsonlogic_words(b))
                }
                _ => rule.to_string(),
            }
        }
        other => other.to_string(),
    }
}

/// One trigger in words: `Contact tagged (crm_contact.tagged) where after.tagName == "VIP"`.
fn describe_trigger(trigger: &Value, catalog: Option<&Value>) -> String {
    let event = str_at(trigger, "eventType");
    let label = catalog
        .and_then(|c| c.get("items"))
        .and_then(Value::as_array)
        .and_then(|items| items.iter().find(|i| i.get("eventType").and_then(Value::as_str) == Some(event)))
        .and_then(|i| i.get("label").and_then(Value::as_str));
    let mut out = label.map_or_else(|| format!("on {event}"), |l| format!("{l} ({event})"));
    match trigger.get("filter") {
        None | Some(Value::Null) => out.push_str(" — every event"),
        Some(f) => {
            let _ = write!(out, " where {}", jsonlogic_words(f));
        }
    }
    out
}

/// Successors of a step, for the ordered walk.
fn successors(step: &Value) -> Vec<String> {
    let cfg = step.get("config");
    let mut out: Vec<String> = Vec::new();
    if step.get("type").and_then(Value::as_str) == Some("branch") {
        if let Some(branches) = cfg.and_then(|c| c.get("branches")).and_then(Value::as_array) {
            out.extend(branches.iter().filter_map(|b| b.get("next").and_then(Value::as_str)).map(str::to_string));
        }
        if let Some(d) = cfg.and_then(|c| c.get("defaultNext")).and_then(Value::as_str) {
            out.push(d.to_string());
        }
    } else if let Some(n) = step.get("next").and_then(Value::as_str) {
        out.push(n.to_string());
    }
    out
}

/// Step ids in walk order from `startStepId`, then any the walk never reached.
fn ordered_steps(def: &Value) -> Vec<(String, bool)> {
    let Some(steps) = def.get("steps").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut seen: Vec<String> = Vec::new();
    let mut queue: std::collections::VecDeque<String> = def.get("startStepId").and_then(Value::as_str).map(str::to_string).into_iter().collect();
    while let Some(id) = queue.pop_front() {
        if seen.contains(&id) {
            continue;
        }
        let Some(step) = steps.get(&id) else { continue };
        seen.push(id);
        queue.extend(successors(step));
    }
    let mut out: Vec<(String, bool)> = seen.iter().map(|id| (id.clone(), true)).collect();
    out.extend(steps.keys().filter(|k| !seen.contains(k)).map(|k| (k.clone(), false)));
    out
}

/// What a step does, in one line.
fn describe_step(step: &Value) -> String {
    let cfg = step.get("config").cloned().unwrap_or(Value::Null);
    let c = |k: &str| cfg.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
    let next = |s: Option<&Value>| s.and_then(Value::as_str).map_or_else(|| "end".to_string(), str::to_string);
    let body = match str_at(step, "type") {
        "send_email" => format!("email {} “{}”", c("to"), c("subject")),
        "wait" => format!("wait {}", c("duration")),
        "branch" => {
            let arms = cfg
                .get("branches")
                .and_then(Value::as_array)
                .map(|bs| {
                    bs.iter()
                        .map(|b| {
                            format!(
                                "{} when {} → {}",
                                str_at(b, "name"),
                                b.get("when").map_or_else(String::new, jsonlogic_words),
                                next(b.get("next"))
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            return format!("branch: {arms}; otherwise → {}", next(cfg.get("defaultNext")));
        }
        "crm_update" => {
            let fields = cfg
                .get("patches")
                .and_then(Value::as_array)
                .map(|ps| ps.iter().map(|p| str_at(p, "field").to_string()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            format!("update {} {fields}", c("entity"))
        }
        "add_tag" => format!("tag contact “{}”", c("tag")),
        "create_task" => format!("task “{}”", c("title")),
        "notify" => format!("notify {} “{}”", c("recipients"), c("title")),
        "webhook_out" => format!("{} {}", cfg.get("method").and_then(Value::as_str).unwrap_or("POST"), c("url")),
        other => other.to_string(),
    };
    format!("{body} → {}", next(step.get("next")))
}

fn render_issues(validation: &Value) -> String {
    let mut out = String::new();
    if let Some(shape) = validation.get("shapeError").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let _ = writeln!(out, "    {} shape: {shape}", "✗".red());
    }
    for issue in validation.get("graphIssues").and_then(Value::as_array).into_iter().flatten() {
        let node = issue.get("nodeId").and_then(Value::as_str).map_or_else(String::new, |n| format!("[{n}] "));
        let _ = writeln!(out, "    {} {node}{}: {}", "✗".red(), str_at(issue, "code"), str_at(issue, "message"));
    }
    out.trim_end().to_string()
}

fn permissions_line(validation: &Value) -> String {
    let perms: Vec<&str> = validation
        .get("requiredPermissions")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if perms.is_empty() {
        "publishing needs no step permissions beyond workflow.publish".to_string()
    } else {
        format!("publishing needs: workflow.publish + {}", perms.join(", "))
    }
}

fn render_validation(validation: Option<&Value>) -> String {
    let Some(v) = validation.filter(|v| v.is_object()) else {
        return format!("  {}", "no validation returned".dimmed());
    };
    let n = issue_count(v);
    if n == 0 {
        format!("  {} draft is publishable\n  {}", "✓".green(), permissions_line(v).dimmed())
    } else {
        format!(
            "  {} draft has {n} problem(s) publish would refuse:\n{}\n  {}",
            "✗".red(),
            render_issues(v),
            permissions_line(v).dimmed()
        )
    }
}

/// Definitions compared without `layout` — the server's version hash ignores
/// it too, so a moved node is not a pending change.
fn same_definition(a: &Value, b: &Value) -> bool {
    let strip = |v: &Value| {
        let mut v = v.clone();
        if let Some(m) = v.as_object_mut() {
            m.remove("layout");
        }
        v
    };
    strip(a) == strip(b)
}

fn render_workflow(w: &Value, catalog: Option<&Value>) -> String {
    let mut out = String::from("\n");
    let ver = w.get("publishedVersion").and_then(Value::as_i64);
    let _ = writeln!(
        out,
        "  {}  {}  {}",
        str_at(w, "name").bold(),
        workflow_status_cell(str_at(w, "status"), 0),
        ver.map_or_else(|| "never published".to_string(), |v| format!("published v{v}")).dimmed()
    );
    let _ = writeln!(
        out,
        "  {}   concurrency {}   last run {}",
        str_at(w, "id").cyan(),
        str_at(w, "concurrencyPolicy"),
        short_ts(w.get("lastRunAt"))
    );
    if let Some(d) = w.get("description").and_then(Value::as_str).filter(|d| !d.is_empty()) {
        let _ = writeln!(out, "  {}", d.dimmed());
    }

    let draft = w.get("draftDefinition").cloned().unwrap_or(Value::Null);
    let triggers = draft.get("triggers").and_then(Value::as_array).cloned().unwrap_or_default();
    let _ = writeln!(out, "\n  {}", "Triggers (draft)".bold());
    if triggers.is_empty() {
        let _ = writeln!(out, "    {} manual only — start it with `smoo workflows run {}`", "◇".dimmed(), str_at(w, "id"));
    }
    for t in &triggers {
        let _ = writeln!(
            out,
            "    {} {} {}",
            "◆".cyan(),
            describe_trigger(t, catalog),
            format!("[{}]", str_at(t, "id")).dimmed()
        );
    }

    let steps = draft.get("steps").and_then(Value::as_object);
    let _ = writeln!(
        out,
        "\n  {} {}",
        "Steps (draft)".bold(),
        format!("start: {}", draft.get("startStepId").and_then(Value::as_str).unwrap_or("—")).dimmed()
    );
    let order = ordered_steps(&draft);
    if order.is_empty() {
        let _ = writeln!(out, "    {}", "no steps yet".dimmed());
    }
    for (i, (id, reachable)) in order.iter().enumerate() {
        let step = steps.and_then(|s| s.get(id)).cloned().unwrap_or(Value::Null);
        let label = step.get("label").and_then(Value::as_str).map_or_else(String::new, |l| format!(" “{l}”"));
        let _ = writeln!(
            out,
            "    {:>2}. {} {}{}  {}{}",
            i + 1,
            pad(id, 16).cyan(),
            pad(str_at(&step, "type"), 12),
            label.dimmed(),
            describe_step(&step),
            if *reachable { String::new() } else { format!("  {}", "(unreachable)".red()) }
        );
    }

    let _ = writeln!(out, "\n  {}", "Validation (draft)".bold());
    let _ = writeln!(out, "{}", render_validation(w.get("validation")));

    let _ = writeln!(out, "\n  {}", "Published".bold());
    match w.get("published").filter(|p| p.is_object()) {
        None => {
            let _ = writeln!(out, "    {}", format!("nothing yet — smoo workflows publish {}", str_at(w, "id")).dimmed());
        }
        Some(p) => {
            let _ = writeln!(
                out,
                "    v{} {} by {}",
                p.get("version").and_then(Value::as_i64).unwrap_or(0),
                short_ts(p.get("publishedAt")),
                str_at(p, "publishedBy").dimmed()
            );
            let perms: Vec<&str> = p
                .get("requiredPermissions")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if !perms.is_empty() {
                let _ = writeln!(out, "    {}", format!("runs as the publisher, who must keep: {}", perms.join(", ")).dimmed());
            }
            let pending = p.get("definition").is_some_and(|d| !same_definition(d, &draft));
            let _ = writeln!(
                out,
                "    {}",
                if pending {
                    "draft has unpublished changes — publish to ship them".yellow().to_string()
                } else {
                    "draft matches the published version".dimmed().to_string()
                }
            );
        }
    }
    out.push('\n');
    out
}

/// What saving a draft changed for the live workflow — nothing, but which
/// "nothing" depends on whether there is a published version at all.
fn draft_saved_note(w: &Value) -> String {
    w.get("publishedVersion").and_then(Value::as_i64).map_or_else(
        || "not published yet — nothing runs until you publish and enable it".to_string(),
        |v| format!("published v{v} is unchanged — it keeps running until you publish again"),
    )
}

fn render_versions(body: &Value) -> String {
    let items = rows(body);
    let mut out = String::from("\n");
    let _ = writeln!(out, "  {} {}", "Versions".bold(), format!("({})", items.len()).dimmed());
    if items.is_empty() {
        let _ = writeln!(out, "\n  {}\n", "never published — a confirmed empty read, not an error".dimmed());
        return out;
    }
    let _ = writeln!(out);
    for v in &items {
        let _ = writeln!(
            out,
            "  {}  {}  {}  {}",
            format!("v{:<3}", v.get("version").and_then(Value::as_i64).unwrap_or(0)).bold(),
            short_ts(v.get("publishedAt")),
            str_at(v, "id").dimmed(),
            format!("by {}", str_at(v, "publishedBy")).dimmed()
        );
    }
    out.push('\n');
    out
}

fn render_publish(body: &Value, id: &str) -> String {
    let version = body.get("version").and_then(Value::as_i64).unwrap_or(0);
    if body.get("unchanged").and_then(Value::as_bool).unwrap_or(false) {
        format!("\n  {} unchanged — the draft equals published v{version}; nothing was written\n", "●".dimmed())
    } else {
        format!(
            "\n  {} published v{version} of {} {}\n",
            "✓".green(),
            id.bold(),
            str_at(body, "versionId").dimmed()
        )
    }
}

fn render_status_change(w: &Value, verb: &str) -> String {
    let triggers: Vec<&str> = w
        .get("triggerEventTypes")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let detail = match (verb, triggers.is_empty()) {
        ("enabled", true) => "manual only — no triggers to listen on; start it with `smoo workflows run`".to_string(),
        ("enabled", false) => format!("listening for: {}", triggers.join(", ")),
        (_, _) => "triggers off — runs already in flight continue".to_string(),
    };
    format!(
        "\n  {} {} {} is now {}\n    {}\n\n",
        "✓".green(),
        str_at(w, "name").bold(),
        str_at(w, "id").dimmed(),
        workflow_status_cell(str_at(w, "status"), 0),
        detail.dimmed()
    )
}

fn render_runs(body: &Value, workflow_id: &str, offset: u32) -> String {
    let items = rows(body);
    let total = body.get("total").and_then(Value::as_u64);
    let mut out = String::from("\n");
    let _ = writeln!(out, "  {} {}", "Runs".bold(), format!("({})", total.unwrap_or(items.len() as u64)).dimmed());
    if items.is_empty() {
        let _ = writeln!(out, "\n  {}\n", "no runs matched — a confirmed empty read, not an error".dimmed());
        return out;
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {}  {}  {}  {}  {}  {}",
        pad("RUN", 36).dimmed(),
        pad("STATUS", 20).dimmed(),
        pad("STARTED", 17).dimmed(),
        pad("TOOK", 8).dimmed(),
        pad("TRIGGER", 24).dimmed(),
        "ENTITY".dimmed()
    );
    for r in &items {
        let took = duration_between(r.get("startedAt"), r.get("completedAt")).unwrap_or_else(|| "—".to_string());
        let entity = match (r.get("entityType").and_then(Value::as_str), r.get("entityId").and_then(Value::as_str)) {
            (Some(t), Some(i)) => format!("{t}:{i}"),
            _ => "—".to_string(),
        };
        let _ = writeln!(
            out,
            "  {}  {}  {}  {}  {}  {}",
            str_at(r, "id").cyan(),
            run_status_cell(str_at(r, "status"), 18),
            pad(&short_ts(r.get("startedAt")), 17),
            pad(&took, 8),
            pad(str_at(r, "triggerEventType"), 24),
            entity.dimmed()
        );
    }
    if let Some(note) = truncation_note(items.len(), offset, total, "runs") {
        let _ = write!(out, "\n{}", note.dimmed());
    }
    let _ = writeln!(out, "\n  {}\n", format!("timeline: smoo workflows run-show {workflow_id} <run>").dimmed());
    out
}

/// A JSON value on one line, cut to the timeline width unless `full`.
fn compact(v: &Value, full: bool) -> (String, bool) {
    let s = v.to_string();
    if full || s.chars().count() <= TIMELINE_JSON_WIDTH {
        (s, false)
    } else {
        (format!("{}…", s.chars().take(TIMELINE_JSON_WIDTH).collect::<String>()), true)
    }
}

fn render_run_detail(run: &Value, full: bool) -> String {
    let mut out = String::from("\n");
    let _ = writeln!(
        out,
        "  {} {}  {}",
        "Run".bold(),
        str_at(run, "id").cyan(),
        run_status_cell(str_at(run, "status"), 0)
    );
    let entity = match (run.get("entityType").and_then(Value::as_str), run.get("entityId").and_then(Value::as_str)) {
        (Some(t), Some(i)) => format!("{t}:{i}"),
        _ => "none".to_string(),
    };
    let _ = writeln!(
        out,
        "  trigger {} {}   entity {}",
        str_at(run, "triggerEventType"),
        format!("(event {})", str_at(run, "triggerEventId")).dimmed(),
        entity
    );
    let took = duration_between(run.get("startedAt"), run.get("completedAt")).map_or_else(|| "still going".to_string(), |d| format!("took {d}"));
    let _ = writeln!(out, "  started {}   {took}", short_ts(run.get("startedAt")));
    if let Some(d) = run.get("statusDetail").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let _ = writeln!(out, "  {} {d}", "detail".dimmed());
    }
    if let Some(e) = run.get("error").filter(|e| !e.is_null()) {
        let _ = writeln!(out, "  {} {}", "error".red(), compact(e, full).0);
    }

    let steps = run.get("steps").and_then(Value::as_array).cloned().unwrap_or_default();
    let _ = writeln!(out, "\n  {} {}", "Timeline".bold(), format!("({} step(s))", steps.len()).dimmed());
    if steps.is_empty() {
        let _ = writeln!(out, "    {}", "no steps executed".dimmed());
    }
    let mut cut = false;
    for s in &steps {
        let took = duration_between(s.get("startedAt"), s.get("completedAt")).unwrap_or_else(|| "—".to_string());
        let _ = writeln!(
            out,
            "   {:>3}  {}  {}  {}  {}",
            s.get("sequence").and_then(Value::as_i64).unwrap_or(0),
            pad(str_at(s, "stepId"), 16).cyan(),
            pad(str_at(s, "stepType"), 12),
            run_status_cell(str_at(s, "status"), 10),
            took.dimmed()
        );
        for key in ["input", "output", "error"] {
            if let Some(v) = s.get(key).filter(|v| !v.is_null()) {
                let (text, was_cut) = compact(v, full);
                cut |= was_cut;
                let label = format!("{key:<6}");
                let label = if key == "error" {
                    label.red().to_string()
                } else {
                    label.dimmed().to_string()
                };
                let _ = writeln!(out, "         {label} {text}");
            }
        }
    }
    if cut {
        let _ = writeln!(
            out,
            "\n  {}",
            "some values were cut at 240 chars — --full shows them whole, --json raw".dimmed()
        );
    }
    out.push('\n');
    out
}

fn render_run_preview(w: &Value, req: &Value) -> String {
    let mut out = String::from("\n");
    let _ = writeln!(
        out,
        "  {} {} {}",
        "preview — nothing was started".yellow().bold(),
        str_at(w, "name").bold(),
        str_at(w, "id").dimmed()
    );
    match w.get("published").filter(|p| p.is_object()) {
        None => {
            let _ = writeln!(
                out,
                "\n  {} never published — there is no version to run. Publish first: smoo workflows publish {}\n",
                "✗".red(),
                str_at(w, "id")
            );
            return out;
        }
        Some(p) => {
            let def = p.get("definition").cloned().unwrap_or(Value::Null);
            let steps = def.get("steps").and_then(Value::as_object);
            let _ = writeln!(
                out,
                "\n  would run published v{} (not the draft) — steps:",
                p.get("version").and_then(Value::as_i64).unwrap_or(0)
            );
            for (i, (id, _)) in ordered_steps(&def).iter().enumerate() {
                let step = steps.and_then(|s| s.get(id)).cloned().unwrap_or(Value::Null);
                let _ = writeln!(out, "    {:>2}. {} {}", i + 1, pad(id, 16).cyan(), describe_step(&step));
            }
        }
    }
    let _ = writeln!(out, "\n  request body: {req}");
    let _ = writeln!(
        out,
        "\n  {}\n",
        "these steps have real effects (email, CRM writes, webhooks). Re-run with --confirm to start it (add --wait to follow it).".dimmed()
    );
    out
}

fn render_event_types(body: &Value) -> String {
    let items = rows(body);
    let mut out = String::from("\n");
    let _ = writeln!(out, "  {} {}\n", "Trigger events".bold(), format!("({})", items.len()).dimmed());
    for e in &items {
        let fields: Vec<&str> = e
            .get("fields")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|f| f.get("name").and_then(Value::as_str)).collect())
            .unwrap_or_default();
        let _ = writeln!(out, "  {}  {}", pad(str_at(e, "eventType"), 26).cyan(), str_at(e, "label").bold());
        let _ = writeln!(out, "  {:<26}  {}", "", str_at(e, "description").dimmed());
        if !fields.is_empty() {
            let _ = writeln!(out, "  {:<26}  {}", "", format!("fields: {}", fields.join(", ")).dimmed());
        }
    }
    out.push('\n');
    out
}

fn render_step_types(body: &Value) -> String {
    let items = rows(body);
    let mut out = String::from("\n");
    let _ = writeln!(out, "  {} {}\n", "Step types".bold(), format!("({})", items.len()).dimmed());
    let _ = writeln!(
        out,
        "  {}  {}  {}",
        pad("TYPE", 12).dimmed(),
        pad("LABEL", 18).dimmed(),
        "PERMISSION TO PUBLISH".dimmed()
    );
    for s in &items {
        let perm = match (str_at(s, "type"), s.get("requiredPermission").and_then(Value::as_str)) {
            (_, Some(p)) => p.to_string(),
            ("crm_update", None) => "crm.contacts.write or crm.deals.write (per entity)".to_string(),
            (_, None) => "none".to_string(),
        };
        let _ = writeln!(out, "  {}  {}  {}", pad(str_at(s, "type"), 12).cyan(), pad(str_at(s, "label"), 18).bold(), perm);
        let _ = writeln!(out, "  {:<12}  {}", "", str_at(s, "description").dimmed());
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::json;

    use super::*;

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    fn parse(args: &[&str]) -> Cmd {
        let mut argv = vec!["t"];
        argv.extend_from_slice(args);
        Wrap::try_parse_from(argv).unwrap_or_else(|e| panic!("{args:?}: {e}")).cmd
    }

    /// The contract samples the api-prime contract test pins
    /// (`packages/schemas/src/workflows/__fixtures__/api-responses.json` in
    /// SmooAI/smooai), copied verbatim.
    fn fixture(schema: &str) -> Value {
        let all: Value = serde_json::from_str(include_str!("workflows_fixtures/api-responses.json")).expect("fixture json");
        all[schema][0].clone()
    }

    /// Strip ANSI so assertions read the words, not the colours.
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    const ORG: &str = "8be5f5fd-cf71-43ba-9df9-01e15acdaf8e";
    const WF: &str = "00000000-0000-4000-8000-000000000001";

    // ── Parsing ────────────────────────────────────────────────────────────

    #[test]
    fn every_verb_parses() {
        assert!(matches!(parse(&["list"]), Cmd::List { limit: 50, offset: 0, .. }));
        assert!(matches!(parse(&["show", WF]), Cmd::Show { .. }));
        assert!(matches!(parse(&["get", WF]), Cmd::Show { .. }), "get aliases show");
        assert!(matches!(parse(&["versions", WF]), Cmd::Versions { .. }));
        assert!(matches!(parse(&["export", WF, "--published"]), Cmd::Export { published: true, .. }));
        assert!(matches!(parse(&["validate", WF]), Cmd::Validate { .. }));
        assert!(matches!(parse(&["publish", WF, "--enable"]), Cmd::Publish { enable: true, .. }));
        assert!(matches!(parse(&["enable", WF]), Cmd::Enable { .. }));
        assert!(matches!(parse(&["pause", WF]), Cmd::Pause { .. }));
        assert!(matches!(parse(&["rm", WF, "--yes"]), Cmd::Rm { .. }));
        assert!(matches!(parse(&["delete", WF, "--dry-run"]), Cmd::Rm { .. }), "delete aliases rm");
        assert!(matches!(parse(&["runs", WF, "--status", "failed"]), Cmd::Runs { .. }));
        assert!(matches!(parse(&["run-show", WF, "r1"]), Cmd::RunShow { .. }));
        assert!(
            matches!(parse(&["run-get", WF, "r1", "--full"]), Cmd::RunShow { full: true, .. }),
            "run-get aliases run-show"
        );
        assert!(matches!(parse(&["cancel", WF, "r1", "--yes"]), Cmd::Cancel { .. }));
        assert!(matches!(parse(&["event-types", "--json"]), Cmd::EventTypes { out: Out { json: true, .. } }));
        assert!(matches!(parse(&["step-types"]), Cmd::StepTypes { .. }));
        assert!(matches!(parse(&["templates"]), Cmd::Templates { json: false }));
    }

    #[test]
    fn every_read_verb_takes_json_and_org() {
        for args in [
            vec!["list"],
            vec!["show", WF],
            vec!["versions", WF],
            vec!["validate", WF],
            vec!["runs", WF],
            vec!["run-show", WF, "r1"],
            vec!["event-types"],
            vec!["step-types"],
        ] {
            let mut a = args.clone();
            a.extend(["--json", "--org", ORG]);
            let mut argv = vec!["t"];
            argv.extend(a);
            assert!(Wrap::try_parse_from(&argv).is_ok(), "{argv:?}");
        }
    }

    #[test]
    fn create_needs_a_source_and_takes_only_one() {
        assert!(Wrap::try_parse_from(["t", "create", "--name", "x"]).is_err(), "no --file / --from-template");
        assert!(
            Wrap::try_parse_from(["t", "create", "--name", "x", "--file", "a.json", "--from-template", "blank"]).is_err(),
            "both sources"
        );
        match parse(&["create", "--name", "x", "--from-template", "blank", "--concurrency", "skip_if_active"]) {
            Cmd::Create {
                from_template, concurrency, ..
            } => {
                assert_eq!(from_template.as_deref(), Some("blank"));
                assert_eq!(concurrency, Some(Concurrency::SkipIfActive));
            }
            _ => panic!("expected Create"),
        }
        assert!(matches!(
            parse(&["create", "--name", "x", "--file", "-", "--concurrency", "skip-if-active"]),
            Cmd::Create {
                concurrency: Some(Concurrency::SkipIfActive),
                ..
            }
        ));
    }

    #[test]
    fn run_entity_flags_are_mutually_exclusive() {
        assert!(Wrap::try_parse_from(["t", "run", WF, "--contact", "c", "--deal", "d"]).is_err());
        assert!(Wrap::try_parse_from(["t", "run", WF, "--deal", "d", "--entity", "x:y"]).is_err());
        match parse(&[
            "run",
            WF,
            "--contact",
            "c1",
            "--input",
            "a=1",
            "--input",
            "b=hi",
            "--confirm",
            "--wait",
            "--timeout",
            "30",
        ]) {
            Cmd::Run {
                contact,
                input,
                confirm,
                wait,
                timeout,
                ..
            } => {
                assert_eq!(contact.as_deref(), Some("c1"));
                assert_eq!(input, vec!["a=1", "b=hi"]);
                assert!(confirm && wait);
                assert_eq!(timeout, 30);
            }
            _ => panic!("expected Run"),
        }
    }

    /// The invariant that keeps `run` safe: no `--confirm` ⇒ preview.
    #[test]
    fn run_without_confirm_is_a_preview() {
        assert!(matches!(parse(&["run", WF]), Cmd::Run { confirm: false, .. }));
    }

    #[test]
    fn update_description_and_clear_conflict() {
        assert!(Wrap::try_parse_from(["t", "update", WF, "--description", "x", "--clear-description"]).is_err());
    }

    // ── Request building ───────────────────────────────────────────────────

    #[test]
    fn paths_match_the_route_manifest() {
        assert_eq!(list_path(ORG, None, 50, 0), format!("/organizations/{ORG}/workflows?limit=50"));
        assert_eq!(
            list_path(ORG, Some("active"), 10, 20),
            format!("/organizations/{ORG}/workflows?status=active&limit=10&offset=20")
        );
        assert_eq!(workflow_path(ORG, WF), format!("/organizations/{ORG}/workflows/{WF}"));
        assert_eq!(run_path(ORG, WF, "r1"), format!("/organizations/{ORG}/workflows/{WF}/runs/r1"));
        assert_eq!(
            runs_path(ORG, WF, Some("failed"), Some("evt 1"), Some("wf:a:b"), 25, 0),
            format!("/organizations/{ORG}/workflows/{WF}/runs?status=failed&eventId=evt%201&temporalWorkflowId=wf%3Aa%3Ab&limit=25")
        );
        assert_eq!(
            runs_path(ORG, WF, Some("  "), None, None, 1, 5),
            format!("/organizations/{ORG}/workflows/{WF}/runs?limit=1&offset=5")
        );
    }

    #[test]
    fn ids_are_trimmed_and_encoded() {
        assert_eq!(workflow_path(ORG, " a/b "), format!("/organizations/{ORG}/workflows/a%2Fb"));
    }

    #[test]
    fn create_body_matches_the_contract() {
        let def = json!({ "schemaVersion": 1, "triggers": [], "startStepId": "a", "steps": {} });
        assert_eq!(create_body("W", None, &def, None), json!({ "name": "W", "definition": def }));
        assert_eq!(
            create_body("W", Some("d"), &def, Some(Concurrency::SkipIfActive)),
            json!({ "name": "W", "description": "d", "definition": def, "concurrencyPolicy": "skip_if_active" })
        );
    }

    #[test]
    fn update_body_sends_only_what_was_flagged() {
        assert!(update_body(None, None, None, false, None).is_err(), "empty PATCH refused");
        assert_eq!(update_body(None, Some("N"), None, false, None).expect("name"), json!({ "name": "N" }));
        assert_eq!(
            update_body(None, None, None, true, None).expect("clear"),
            json!({ "description": null }),
            "clear sends an explicit null"
        );
        assert_eq!(
            update_body(Some(json!({ "x": 1 })), None, Some("d"), false, Some(Concurrency::Allow)).expect("all"),
            json!({ "definition": { "x": 1 }, "description": "d", "concurrencyPolicy": "allow" })
        );
    }

    #[test]
    fn publish_body_names_the_member_only_when_asked() {
        assert_eq!(publish_body(None), json!({}));
        assert_eq!(publish_body(Some(" ")), json!({}));
        assert_eq!(publish_body(Some("u-1")), json!({ "onBehalfOfUserId": "u-1" }));
    }

    #[test]
    fn run_body_matches_the_contract() {
        assert_eq!(run_body(None, Map::new()), json!({}));
        let mut input = Map::new();
        input.insert("n".into(), json!(3));
        assert_eq!(
            run_body(Some(&("crm_contact".into(), "c1".into())), input),
            json!({ "entity": { "type": "crm_contact", "id": "c1" }, "input": { "n": 3 } })
        );
    }

    #[test]
    fn entity_flags_map_to_catalog_entity_types() {
        assert_eq!(
            parse_entity(Some("c1"), None, None).expect("contact"),
            Some(("crm_contact".into(), "c1".into()))
        );
        assert_eq!(parse_entity(None, Some(" d1 "), None).expect("deal"), Some(("crm_deal".into(), "d1".into())));
        assert_eq!(
            parse_entity(None, None, Some("crm_task:t1")).expect("entity"),
            Some(("crm_task".into(), "t1".into()))
        );
        assert_eq!(parse_entity(None, None, None).expect("none"), None);
        assert!(parse_entity(None, None, Some("nocolon")).is_err());
        assert!(parse_entity(None, None, Some(":id")).is_err());
        assert!(parse_entity(Some(" "), None, None).is_err());
    }

    #[test]
    fn inputs_parse_json_values_and_pairs_override_the_file() {
        let got = parse_inputs(
            Some(json!({ "a": "file", "keep": true })),
            &["a=1".into(), "s=hello world".into(), "o={\"k\":[1]}".into(), "eq=x=y".into()],
        )
        .expect("inputs");
        assert_eq!(got["a"], json!(1), "pair wins, parsed as JSON");
        assert_eq!(got["keep"], json!(true));
        assert_eq!(got["s"], json!("hello world"), "non-JSON stays a string");
        assert_eq!(got["o"], json!({ "k": [1] }));
        assert_eq!(got["eq"], json!("x=y"), "only the first = splits");
        assert!(parse_inputs(None, &["novalue".into()]).is_err());
        assert!(parse_inputs(None, &["=v".into()]).is_err());
        assert!(parse_inputs(Some(json!([1])), &[]).is_err(), "file must be an object");
    }

    #[test]
    fn definition_files_accept_a_bare_definition_or_a_show_dump() {
        let def = json!({ "schemaVersion": 1, "triggers": [], "startStepId": "a", "steps": {} });
        assert_eq!(definition_from_value(def.clone()).expect("bare"), def);
        let dump = fixture("WorkflowSchema");
        assert_eq!(definition_from_value(dump.clone()).expect("dump"), dump["draftDefinition"]);
        assert!(definition_from_value(json!([1])).is_err());
    }

    #[test]
    fn export_round_trips_the_draft_and_can_take_the_published_version() {
        let w = fixture("WorkflowSchema");
        assert_eq!(export_definition(&w, false).expect("draft"), w["draftDefinition"]);
        assert_eq!(export_definition(&w, true).expect("published"), w["published"]["definition"]);
        let unpublished = json!({ "draftDefinition": {}, "published": null });
        let err = export_definition(&unpublished, true).expect_err("never published");
        assert!(format!("{err}").contains("never been published"), "{err}");
    }

    /// Fields of the events the templates listen on, as the live catalog
    /// (`GET /workflow-event-types`) lists them.
    fn catalog_fields(event: &str) -> Vec<&'static str> {
        match event {
            "crm_contact.tagged" | "crm_contact.untagged" => vec!["tagId", "tagName"],
            "crm_deal.stage_changed" => vec!["stage"],
            "crm_contact.created" => vec!["firstName", "lastName", "email", "phone", "lifecycleStage", "source"],
            other => panic!("add {other}'s catalog fields"),
        }
    }

    /// Every `{"var": …}` path in a JsonLogic rule.
    fn vars(rule: &Value) -> Vec<String> {
        match rule {
            Value::Object(m) => m
                .iter()
                .flat_map(|(k, v)| {
                    if k == "var" {
                        v.as_str().map(str::to_string).into_iter().collect()
                    } else {
                        vars(v)
                    }
                })
                .collect(),
            Value::Array(a) => a.iter().flat_map(vars).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn templates_are_internally_consistent() {
        let known_events = [
            "crm_contact.created",
            "crm_contact.updated",
            "crm_contact.deleted",
            "crm_contact.tagged",
            "crm_contact.untagged",
            "crm_deal.created",
            "crm_deal.updated",
            "crm_deal.stage_changed",
            "crm_task.created",
            "crm_task.completed",
        ];
        for t in TEMPLATES {
            let def = (t.definition)();
            assert_eq!(def["schemaVersion"], json!(1), "{}", t.id);
            for trig in def["triggers"].as_array().expect("triggers") {
                assert!(known_events.contains(&trig["eventType"].as_str().expect("eventType")), "{}", t.id);
            }
            let steps = def["steps"].as_object().expect("steps");
            if t.id == "blank" {
                continue;
            }
            assert!(steps.contains_key(def["startStepId"].as_str().expect("start")), "{}: start exists", t.id);
            for step in steps.values() {
                for next in successors(step) {
                    assert!(steps.contains_key(&next), "{}: dangling next {next}", t.id);
                }
            }
            assert!(ordered_steps(&def).iter().all(|(_, reachable)| *reachable), "{}: every step reachable", t.id);
            for trig in def["triggers"].as_array().expect("triggers") {
                let fields = catalog_fields(trig["eventType"].as_str().expect("eventType"));
                for path in vars(&trig["filter"]) {
                    let field = path.strip_prefix("after.").unwrap_or(&path);
                    assert!(fields.contains(&field), "{}: filter reads `{path}`, not a field of its event", t.id);
                }
            }
            let text = def.to_string();
            if def["triggers"][0]["eventType"].as_str().is_some_and(|e| e.starts_with("crm_deal.")) {
                assert!(!text.contains("{{entity.name}}"), "{}: a deal's name is `title`", t.id);
            }
        }
        assert!(template_definition("tag-added-send-email").is_ok());
        let err = template_definition("nope").expect_err("unknown");
        assert!(format!("{err}").contains("deal-won-task-notify"), "lists the choices: {err}");
    }

    // ── Error rendering ────────────────────────────────────────────────────

    fn api_error(status: &str, body: &Value) -> anyhow::Error {
        // Exactly the smooth-api-client `decode` format, wrapped once as the
        // call sites do.
        anyhow::anyhow!("POST /organizations/o/workflows/w/publish returned HTTP {status}: {body}").context("outer")
    }

    #[test]
    fn a_422_lists_every_issue_pinned_to_its_node() {
        let body = fixture("InvalidWorkflowErrorSchema");
        let msg = plain(&explain(api_error("422 Unprocessable Entity", &body), "publish", Some(WF)).to_string());
        assert!(msg.starts_with("publish refused — the draft has 2 problem(s):"), "{msg}");
        assert!(msg.contains("✗ [a] unknown_next: Step \"a\" points to \"b\", which does not exist."), "{msg}");
        assert!(msg.contains("✗ [t] unknown_event_type:"), "{msg}");
        assert!(
            msg.contains(&format!("smoo workflows update {WF} --file def.json")),
            "says what to do next: {msg}"
        );
    }

    #[test]
    fn a_422_shape_error_is_its_own_line() {
        let body = json!({ "error": "invalid_workflow", "shapeError": "$.triggers: is required", "graphIssues": [] });
        let msg = plain(&explain(api_error("422 Unprocessable Entity", &body), "publish", Some(WF)).to_string());
        assert!(msg.contains("1 problem(s)"), "{msg}");
        assert!(msg.contains("✗ shape: $.triggers: is required"), "{msg}");
    }

    #[test]
    fn a_403_step_permission_names_the_key_and_the_fix() {
        let body = fixture("WorkflowForbiddenErrorSchema");
        let msg = explain(api_error("403 Forbidden", &body), "publish", Some(WF)).to_string();
        assert!(msg.contains("this workflow's steps need `communications.email.send`"), "{msg}");
        assert!(msg.contains("ask an org admin to grant `communications.email.send`"), "{msg}");
    }

    #[test]
    fn a_403_route_permission_and_a_feature_gate_read_differently() {
        let role = explain(
            api_error("403 Forbidden", &json!({ "error": "Forbidden", "missingPermission": "workflow.publish" })),
            "enable",
            Some(WF),
        )
        .to_string();
        assert!(role.starts_with("enable refused — your role lacks `workflow.publish`"), "{role}");
        let gate = explain(api_error("403 Forbidden", &json!({ "message": "feature not enabled" })), "list workflows", None).to_string();
        assert!(gate.contains("feature not enabled") && gate.contains("`workflows` product feature"), "{gate}");
    }

    #[test]
    fn conflicts_say_what_to_do_next() {
        let body = json!({ "message": "workflow has no published version" });
        let enable = explain(api_error("409 Conflict", &body), "enable", Some(WF)).to_string();
        assert!(
            enable.contains("workflow has no published version") && enable.contains(&format!("smoo workflows publish {WF}")),
            "{enable}"
        );
        let delete = explain(api_error("409 Conflict", &json!({ "message": "run in flight" })), "delete", Some(WF)).to_string();
        assert!(delete.contains("--status running"), "{delete}");
    }

    #[test]
    fn non_http_errors_and_odd_bodies_pass_through() {
        let net = explain(anyhow::anyhow!("connection refused"), "list workflows", None);
        assert_eq!(net.to_string(), "connection refused");
        assert_eq!(
            parse_http_failure("GET /x returned HTTP 502 Bad Gateway: "),
            Some(HttpFailure {
                status: 502,
                body: Value::Null
            })
        );
        assert_eq!(
            parse_http_failure("GET /x returned HTTP 500 Internal Server Error: boom"),
            Some(HttpFailure {
                status: 500,
                body: json!({ "message": "boom" })
            })
        );
        assert_eq!(parse_http_failure("no status here"), None);
        let other = explain(anyhow::anyhow!("GET /x returned HTTP 500 Internal Server Error: boom"), "list workflows", None).to_string();
        assert_eq!(other, "list workflows failed (HTTP 500): boom");
    }

    // ── Rendering ──────────────────────────────────────────────────────────

    #[test]
    fn list_renders_every_row_and_reports_truncation() {
        let body = fixture("WorkflowListResponseSchema");
        let out = plain(&render_list(&body, 0));
        assert!(out.contains(WF) && out.contains("Welcome VIPs") && out.contains("crm_contact.tagged"), "{out}");
        let mut paged = body.clone();
        paged["total"] = json!(3);
        let out = plain(&render_list(&paged, 0));
        assert!(out.contains("showing 1–1 of 3 workflows — page with --offset 1"), "{out}");
        let empty = plain(&render_list(&json!({ "items": [], "total": 0 }), 0));
        assert!(empty.contains("confirmed empty read"), "{empty}");
    }

    #[test]
    fn show_puts_triggers_in_words_and_flags_pending_changes() {
        let mut w = fixture("WorkflowSchema");
        let catalog = fixture("WorkflowEventTypeListResponseSchema");
        w["draftDefinition"]["triggers"][0]["filter"] = json!({ "and": [{ "==": [{ "var": "after.tagName" }, "VIP"] }] });
        let out = plain(&render_workflow(&w, Some(&catalog)));
        assert!(
            out.contains("Tag added to contact (crm_contact.tagged) where after.tagName == \"VIP\""),
            "{out}"
        );
        assert!(out.contains("email") && out.contains("send_email") && out.contains("→ end"), "{out}");
        assert!(out.contains("draft is publishable"), "{out}");
        assert!(out.contains("publishing needs: workflow.publish + communications.email.send"), "{out}");
        assert!(out.contains("v3"), "{out}");
        assert!(out.contains("draft has unpublished changes"), "filter added ⇒ draft ≠ published: {out}");

        // Moving a node only is NOT a pending change (the server's hash ignores layout).
        let mut moved = fixture("WorkflowSchema");
        moved["draftDefinition"]["layout"] = json!({ "nodes": { "email": { "x": 99, "y": 1 } } });
        assert!(plain(&render_workflow(&moved, None)).contains("draft matches the published version"));
    }

    #[test]
    fn show_pins_draft_issues_and_handles_an_unpublished_manual_workflow() {
        let w = fixture("WorkflowSchema");
        let invalid = json!({
            "id": WF, "name": "Half-built", "status": "draft", "concurrencyPolicy": "allow",
            "publishedVersion": null, "lastRunAt": null,
            "draftDefinition": { "schemaVersion": 1, "triggers": [], "startStepId": "a",
                "steps": { "a": { "type": "wait", "config": { "duration": "PT5M" }, "next": "b" }, "orphan": { "type": "notify", "config": {} } } },
            "published": null,
            "validation": { "shapeError": null, "graphIssues": [
                { "code": "unknown_next", "message": "Step \"a\" points to \"b\", which does not exist.", "nodeId": "a" },
                { "code": "unreachable_step", "message": "Step \"orphan\" can never run", "nodeId": "orphan" }
            ], "requiredPermissions": [] }
        });
        let out = plain(&render_workflow(&invalid, None));
        assert!(out.contains("manual only"), "{out}");
        assert!(out.contains("draft has 2 problem(s)"), "{out}");
        assert!(out.contains("[orphan] unreachable_step"), "{out}");
        assert!(out.contains("(unreachable)"), "{out}");
        assert!(out.contains("nothing yet — smoo workflows publish"), "{out}");
        // The second fixture sample is shape-invalid.
        let shape = &fixture_all("WorkflowSchema")[1];
        assert!(plain(&render_workflow(shape, None)).contains("shape: $.triggers: is required"));
        let _ = w;
    }

    fn fixture_all(schema: &str) -> Vec<Value> {
        let all: Value = serde_json::from_str(include_str!("workflows_fixtures/api-responses.json")).expect("fixture json");
        all[schema].as_array().cloned().unwrap_or_default()
    }

    #[test]
    fn run_timeline_shows_each_step_with_resolved_io() {
        let out = plain(&render_run_detail(&fixture("WorkflowRunDetailSchema"), false));
        assert!(out.contains("✗ failed"), "{out}");
        assert!(out.contains("trigger crm_contact.tagged (event evt-1)   entity crm_contact:c-1"), "{out}");
        assert!(out.contains("took 1m00s"), "{out}");
        assert!(out.contains("detail send_email failed"), "{out}");
        assert!(out.contains("1  email"), "{out}");
        assert!(out.contains("input  {\"to\":\"a@b.co\"}"), "{out}");
        assert!(out.contains("error  {\"code\":\"NO_SENDGRID_INTEGRATION\"}"), "{out}");
        assert!(!out.contains("output"), "null output is omitted: {out}");
    }

    #[test]
    fn long_step_values_are_cut_and_the_cut_is_reported() {
        let mut run = fixture("WorkflowRunDetailSchema");
        run["steps"][0]["output"] = json!({ "blob": "x".repeat(600) });
        let cut = plain(&render_run_detail(&run, false));
        assert!(cut.contains("some values were cut"), "{cut}");
        let full = plain(&render_run_detail(&run, true));
        assert!(
            !full.contains("some values were cut") && full.contains(&"x".repeat(600)),
            "--full shows it whole"
        );
    }

    #[test]
    fn runs_list_renders_and_points_at_the_timeline() {
        let body = fixture("WorkflowRunListResponseSchema");
        let out = plain(&render_runs(&body, WF, 0));
        assert!(out.contains("00000000-0000-4000-8000-000000000005"), "{out}");
        assert!(out.contains(&format!("smoo workflows run-show {WF} <run>")), "{out}");
        assert!(plain(&render_runs(&json!({ "items": [], "total": 0 }), WF, 0)).contains("confirmed empty read"));
    }

    #[test]
    fn publish_and_status_changes_render() {
        let unchanged = plain(&render_publish(&fixture("PublishWorkflowResponseSchema"), WF));
        assert!(unchanged.contains("unchanged — the draft equals published v3"), "{unchanged}");
        let fresh = plain(&render_publish(&json!({ "versionId": "v", "version": 4, "unchanged": false }), WF));
        assert!(fresh.contains("published v4"), "{fresh}");
        let enabled = plain(&render_status_change(&fixture("WorkflowSchema"), "enabled"));
        assert!(
            enabled.contains("● active") && enabled.contains("listening for: crm_contact.tagged"),
            "{enabled}"
        );
        let paused = plain(&render_status_change(&json!({ "name": "W", "id": WF, "status": "paused" }), "paused"));
        assert!(paused.contains("◐ paused") && paused.contains("in flight continue"), "{paused}");
    }

    #[test]
    fn catalogs_render_with_permissions() {
        let steps = plain(&render_step_types(&fixture("WorkflowStepTypeListResponseSchema")));
        assert!(steps.contains("send_email") && steps.contains("communications.email.send"), "{steps}");
        assert!(
            steps.contains("crm.contacts.write or crm.deals.write (per entity)"),
            "crm_update is per entity: {steps}"
        );
        let events = plain(&render_event_types(&fixture("WorkflowEventTypeListResponseSchema")));
        assert!(
            events.contains("crm_contact.created") && events.contains("Contact created") && events.contains("fields: firstName"),
            "{events}"
        );
    }

    #[test]
    fn run_preview_refuses_an_unpublished_workflow_and_shows_the_pinned_steps() {
        let never = plain(&render_run_preview(&json!({ "id": WF, "name": "W", "published": null }), &json!({})));
        assert!(never.contains("never published"), "{never}");
        let req = run_body(Some(&("crm_contact".into(), "c1".into())), Map::new());
        let out = plain(&render_run_preview(&fixture("WorkflowSchema"), &req));
        assert!(out.contains("preview — nothing was started"), "{out}");
        assert!(out.contains("published v3 (not the draft)"), "{out}");
        assert!(out.contains("request body: {\"entity\":{\"type\":\"crm_contact\",\"id\":\"c1\"}}"), "{out}");
        assert!(out.contains("--confirm"), "{out}");
    }

    #[test]
    fn jsonlogic_reads_as_words() {
        assert_eq!(jsonlogic_words(&json!({ "==": [{ "var": "after.stage" }, "won"] })), "after.stage == \"won\"");
        assert_eq!(
            jsonlogic_words(&json!({ "or": [{ ">": [{ "var": "after.value" }, 100] }, { "!": { "var": "before.value" } }] })),
            "(after.value > 100 or not before.value)"
        );
        assert_eq!(jsonlogic_words(&json!(true)), "true");
        // Anything outside the subset stays exact rather than guessed at.
        assert_eq!(jsonlogic_words(&json!({ "some": [1, 2] })), "{\"some\":[1,2]}");
    }

    #[test]
    fn steps_describe_every_type() {
        let branch = json!({ "type": "branch", "config": { "branches": [{ "name": "vip", "when": { "==": [{ "var": "entity.tier" }, "vip"] }, "next": "x" }], "defaultNext": null } });
        assert_eq!(describe_step(&branch), "branch: vip when entity.tier == \"vip\" → x; otherwise → end");
        assert_eq!(
            describe_step(&json!({ "type": "wait", "config": { "duration": "P1D" }, "next": "e" })),
            "wait P1D → e"
        );
        assert_eq!(
            describe_step(&json!({ "type": "crm_update", "config": { "entity": "deal", "patches": [{ "field": "stage", "value": "won" }] } })),
            "update deal stage → end"
        );
        assert_eq!(
            describe_step(&json!({ "type": "webhook_out", "config": { "url": "https://x.io/h" } })),
            "POST https://x.io/h → end"
        );
    }

    #[test]
    fn status_cells_keep_their_glyph_when_padded() {
        assert!(plain(&workflow_status_cell("active", 8)).starts_with("● active  "));
        assert!(plain(&workflow_status_cell("paused", 0)).starts_with("◐ paused"));
        assert!(plain(&run_status_cell("failed", 18)).starts_with("✗ failed"));
        assert!(plain(&run_status_cell("succeeded", 10)).starts_with("✓ succeeded"));
        assert!(plain(&run_status_cell("blocked_permission", 18)).starts_with("⊘ "));
    }

    #[test]
    fn a_404_quotes_the_server_once() {
        let msg = explain(
            anyhow::anyhow!("GET /x returned HTTP 404 Not Found: {}", json!({ "message": "not found: Workflow not found" })),
            "show workflow",
            Some(WF),
        )
        .to_string();
        assert!(msg.starts_with("show workflow: not found: Workflow not found\n"), "{msg}");
        assert!(msg.contains("ids are org-scoped"), "{msg}");
    }

    #[test]
    fn saving_a_draft_says_what_is_live() {
        assert!(draft_saved_note(&json!({ "publishedVersion": 3 })).contains("published v3 is unchanged"));
        assert!(draft_saved_note(&json!({ "publishedVersion": null })).contains("not published yet"));
    }

    #[test]
    fn durations_and_terminal_statuses() {
        assert_eq!(human_ms(250), "250ms");
        assert_eq!(human_ms(1500), "1.5s");
        assert_eq!(human_ms(61_000), "1m01s");
        assert_eq!(human_ms(3_700_000), "1h01m");
        assert_eq!(human_ms(90_000_000), "1d1h");
        for s in ["completed", "failed", "cancelled", "blocked_permission", "skipped"] {
            assert!(is_terminal_run_status(s), "{s}");
        }
        assert!(!is_terminal_run_status("running"));
    }
}
