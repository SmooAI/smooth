//! `smoo work …` — the Smoo Projects work surface (SMOODEV-3038): projects,
//! work items, sprints, releases, work-item links, and the Jira work-items
//! sync. CLI twin of the api-prime work domain (SMOODEV-2994, ADR-119).
//!
//! Authenticated as the logged-in *user* (`smoo auth login`) via [`UserClient`],
//! like the CRM commands — the work routes RBAC-gate on `projects.read` /
//! `projects.write` and writes are attributed to a real person.
//!
//! PATCH semantics (mirrors the server): a present key SETS the field (JSON
//! null clears a nullable one), an absent key KEEPS it. So `update` sends only
//! the fields you flagged, and every nullable field has a `--clear-*` twin
//! that sends an explicit null.

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::print_json;
use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// Projects — the containers work items hang off (list / show / create /
    /// update / archive). A project's KEY (e.g. SMOODEV) names it everywhere.
    #[command(visible_alias = "project")]
    Projects {
        #[command(subcommand)]
        cmd: ProjectsCmd,
    },
    /// Work items — tasks, issues, bugs, features, incidents.
    #[command(visible_alias = "item")]
    Items {
        #[command(subcommand)]
        cmd: ItemsCmd,
    },
    /// Sprints — time-boxed iterations inside a project (at most one active
    /// per project; complete rolls unfinished items forward).
    #[command(visible_alias = "sprint")]
    Sprints {
        #[command(subcommand)]
        cmd: SprintsCmd,
    },
    /// Releases — named ship targets inside a project.
    #[command(visible_alias = "release")]
    Releases {
        #[command(subcommand)]
        cmd: ReleasesCmd,
    },
    /// Links — attach files, URLs, or other work items to a work item.
    #[command(visible_alias = "link")]
    Links {
        #[command(subcommand)]
        cmd: LinksCmd,
    },
    /// Jira work-items sync — trigger an import, read the sync state.
    Jira {
        #[command(subcommand)]
        cmd: JiraCmd,
    },
}

#[derive(Subcommand)]
pub enum ProjectsCmd {
    /// List the org's projects.
    List {
        /// Filter by status (`active` or `archived`).
        #[arg(long)]
        status: Option<String>,
        /// Maximum number of projects to return.
        #[arg(long, default_value = "50")]
        limit: u32,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show one project (by id or KEY).
    Show {
        /// The project — uuid or KEY (e.g. SMOODEV).
        project: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a project. The KEY (1-10 chars, A-Z0-9, letter-first) is
    /// immutable after create — references are minted from it.
    Create {
        /// Project name.
        name: String,
        /// Short uppercase reference prefix, unique per org (e.g. SUPPORT).
        #[arg(long)]
        key: String,
        /// Project description.
        #[arg(long)]
        description: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update a project — only the flags you pass change.
    Update {
        /// The project — uuid or KEY.
        project: String,
        /// New project name.
        #[arg(long)]
        name: Option<String>,
        /// New description.
        #[arg(long, conflicts_with = "clear_description")]
        description: Option<String>,
        /// Clear the description.
        #[arg(long)]
        clear_description: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Archive a project (work items keep their history; nothing is deleted).
    Archive {
        /// The project — uuid or KEY.
        project: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

/// The shared filter flags for `items list`. Split out so the flag → query
/// param mapping is a pure, testable function ([`items_query`]).
#[derive(Args, Debug, Default)]
pub struct ItemFilters {
    /// Filter to one project — uuid or KEY.
    #[arg(long)]
    pub project: Option<String>,
    /// Filter by status (open, in_progress, blocked, in_review, done, cancelled).
    #[arg(long)]
    pub status: Option<String>,
    /// Filter by type (task, issue, bug, feature, incident).
    #[arg(long = "type")]
    pub item_type: Option<String>,
    /// Filter by assignee — `me`, an email, or a user-id uuid.
    #[arg(long)]
    pub assignee: Option<String>,
    /// Filter to items escalated from a support ticket.
    #[arg(long = "support-ticket")]
    pub support_ticket: Option<String>,
    /// Filter to one sprint (uuid).
    #[arg(long)]
    pub sprint: Option<String>,
    /// Filter to one release (uuid).
    #[arg(long)]
    pub release: Option<String>,
    /// Maximum number of items to return.
    #[arg(long, default_value = "50")]
    pub limit: u32,
}

#[derive(Subcommand)]
pub enum ItemsCmd {
    /// List work items (`--project SMOODEV --status open --assignee me`).
    List {
        #[command(flatten)]
        filters: ItemFilters,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show one work item.
    Show {
        /// The work item id from `smoo work items list`.
        item_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a work item.
    Create {
        /// Work item title.
        title: String,
        /// File it into a project — uuid or KEY.
        #[arg(long)]
        project: Option<String>,
        /// Type: task (default), issue, bug, feature, incident.
        #[arg(long = "type")]
        item_type: Option<String>,
        /// Priority 0 (lowest) – 4 (highest); default 2.
        #[arg(long, value_parser = clap::value_parser!(i64).range(0..=4))]
        priority: Option<i64>,
        /// Description.
        #[arg(long)]
        description: Option<String>,
        /// Assignee — `me`, an email, or a user-id uuid.
        #[arg(long)]
        assignee: Option<String>,
        /// Due date — RFC3339 or `YYYY-MM-DD`.
        #[arg(long)]
        due: Option<String>,
        /// Parent work item id.
        #[arg(long)]
        parent: Option<String>,
        /// Support ticket this item was escalated from.
        #[arg(long = "support-ticket")]
        support_ticket: Option<String>,
        /// Plan into a sprint (uuid, must belong to the item's project).
        #[arg(long)]
        sprint: Option<String>,
        /// Plan into a release (uuid, must belong to the item's project).
        #[arg(long)]
        release: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update a work item — only the flags you pass change; `--clear-*`
    /// sends an explicit null (PATCH is set-vs-clear, absent keeps).
    Update {
        /// The work item id from `smoo work items list`.
        item_id: String,
        #[command(flatten)]
        patch: ItemPatch,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Move a work item to a new status (`transition <id> done`). Landing on
    /// `done` stamps completedAt; leaving it clears it (server-owned).
    Transition {
        /// The work item id from `smoo work items list`.
        item_id: String,
        /// New status: open, in_progress, blocked, in_review, done, cancelled.
        status: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a work item permanently (its Jira sync mapping goes with it).
    #[command(visible_alias = "delete")]
    Rm {
        /// The work item id from `smoo work items list`.
        item_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the target and exit without deleting.
        #[arg(long)]
        dry_run: bool,
        /// Skip the interactive confirmation. Required in scripts/CI.
        #[arg(long)]
        yes: bool,
    },
}

/// The `items update` field flags. Set-vs-clear pairs `conflicts_with` each
/// other; [`item_patch`] turns the whole struct into the PATCH body.
#[derive(Args, Debug, Default)]
pub struct ItemPatch {
    /// New title.
    #[arg(long)]
    pub title: Option<String>,
    /// New description.
    #[arg(long, conflicts_with = "clear_description")]
    pub description: Option<String>,
    /// Clear the description.
    #[arg(long)]
    pub clear_description: bool,
    /// New type (task, issue, bug, feature, incident).
    #[arg(long = "type")]
    pub item_type: Option<String>,
    /// New priority 0 (lowest) – 4 (highest).
    #[arg(long, value_parser = clap::value_parser!(i64).range(0..=4))]
    pub priority: Option<i64>,
    /// Move to a project — uuid or KEY (resolved before the PATCH).
    #[arg(long, conflicts_with = "clear_project")]
    pub project: Option<String>,
    /// Detach from its project.
    #[arg(long)]
    pub clear_project: bool,
    /// New assignee — `me`, an email, or a user-id uuid.
    #[arg(long, conflicts_with = "clear_assignee")]
    pub assignee: Option<String>,
    /// Unassign the item.
    #[arg(long)]
    pub clear_assignee: bool,
    /// New due date — RFC3339 or `YYYY-MM-DD`.
    #[arg(long, conflicts_with = "clear_due")]
    pub due: Option<String>,
    /// Clear the due date.
    #[arg(long)]
    pub clear_due: bool,
    /// New parent work item id.
    #[arg(long, conflicts_with = "clear_parent")]
    pub parent: Option<String>,
    /// Detach from its parent.
    #[arg(long)]
    pub clear_parent: bool,
    /// Link to a support ticket.
    #[arg(long = "support-ticket", conflicts_with = "clear_support_ticket")]
    pub support_ticket: Option<String>,
    /// Detach from its support ticket.
    #[arg(long)]
    pub clear_support_ticket: bool,
    /// Plan into a sprint (uuid).
    #[arg(long, conflicts_with = "clear_sprint")]
    pub sprint: Option<String>,
    /// Move to the backlog (out of its sprint).
    #[arg(long)]
    pub clear_sprint: bool,
    /// Plan into a release (uuid).
    #[arg(long, conflicts_with = "clear_release")]
    pub release: Option<String>,
    /// Take out of its release.
    #[arg(long)]
    pub clear_release: bool,
}

#[derive(Subcommand)]
pub enum SprintsCmd {
    /// List sprints.
    List {
        /// Filter to one project — uuid or KEY.
        #[arg(long)]
        project: Option<String>,
        /// Filter by state (planned, active, completed).
        #[arg(long)]
        state: Option<String>,
        /// Maximum number of sprints to return.
        #[arg(long, default_value = "50")]
        limit: u32,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show one sprint (includes work-item counts by status).
    Show {
        /// The sprint id from `smoo work sprints list`.
        sprint_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a sprint (starts life `planned`).
    Create {
        /// Sprint name (e.g. "Sprint 12").
        name: String,
        /// The project it belongs to — uuid or KEY.
        #[arg(long)]
        project: String,
        /// Sprint goal.
        #[arg(long)]
        goal: Option<String>,
        /// Start date — RFC3339 or `YYYY-MM-DD`.
        #[arg(long)]
        starts: Option<String>,
        /// End date — RFC3339 or `YYYY-MM-DD`.
        #[arg(long)]
        ends: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Start a sprint (at most one active per project — a second start 409s).
    Start {
        /// The sprint id from `smoo work sprints list`.
        sprint_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Complete a sprint — unfinished items roll to `--rollover-to` or the backlog.
    Complete {
        /// The sprint id from `smoo work sprints list`.
        sprint_id: String,
        /// Sprint to roll unfinished items into (default: the backlog).
        #[arg(long = "rollover-to")]
        rollover_to: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ReleasesCmd {
    /// List releases.
    List {
        /// Filter to one project — uuid or KEY.
        #[arg(long)]
        project: Option<String>,
        /// Filter by state (planned, released).
        #[arg(long)]
        state: Option<String>,
        /// Maximum number of releases to return.
        #[arg(long, default_value = "50")]
        limit: u32,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a release (starts life `planned`).
    Create {
        /// Release name (e.g. "v2.1").
        name: String,
        /// The project it belongs to — uuid or KEY.
        #[arg(long)]
        project: String,
        /// Release description.
        #[arg(long)]
        description: Option<String>,
        /// Target ship date — RFC3339 or `YYYY-MM-DD`.
        #[arg(long = "target-date")]
        target_date: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Mark a release released (stamps releasedAt; re-releasing 409s).
    Release {
        /// The release id from `smoo work releases list`.
        release_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum LinksCmd {
    /// List a work item's links.
    List {
        /// The work item id from `smoo work items list`.
        item_id: String,
        /// Print raw JSON instead of the list.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Link a file, URL, or another work item to a work item. Exactly one of
    /// `--file` / `--url` / `--item`.
    Link {
        /// The work item id from `smoo work items list`.
        item_id: String,
        /// A managed file / authored doc id (from `smoo files ls`).
        #[arg(long, conflicts_with_all = ["url", "item"])]
        file: Option<String>,
        /// An external URL.
        #[arg(long, conflicts_with = "item")]
        url: Option<String>,
        /// Another work item id (typed issue link).
        #[arg(long)]
        item: Option<String>,
        /// Link type: file/url → attachment | reference (default reference);
        /// item → relates (default) | blocks | duplicates.
        #[arg(long = "type")]
        link_type: Option<String>,
        /// Display title for the link.
        #[arg(long)]
        title: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove a link from a work item.
    Unlink {
        /// The work item id from `smoo work items list`.
        item_id: String,
        /// The link id from `smoo work links list`.
        link_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Reverse lookup: which work items reference this file.
    ForFile {
        /// The managed file id (from `smoo files ls`).
        file_id: String,
        /// Print raw JSON instead of the list.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum JiraCmd {
    /// Start a Jira work-items import (Temporal pull for the org). Delta by
    /// default; `--full` resets the watermark and re-imports everything.
    Import {
        /// Reset the watermark and re-import from the beginning.
        #[arg(long)]
        full: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show the Jira work-items sync state (watermark, state, last error).
    Status {
        /// Print the raw integration JSON instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Projects { cmd } => projects(cmd).await,
        Cmd::Items { cmd } => items(cmd).await,
        Cmd::Sprints { cmd } => sprints(cmd).await,
        Cmd::Releases { cmd } => releases(cmd).await,
        Cmd::Links { cmd } => links(cmd).await,
        Cmd::Jira { cmd } => jira(cmd).await,
    }
}

/// Resolve the org for these user-authenticated calls: `--org` flag →
/// `SMOOAI_ORG_ID` → the active org persisted by `smoo org switch`.
fn resolve_org(override_org: Option<String>) -> Result<String> {
    crate::active_org::resolve(override_org)
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

async fn projects(cmd: ProjectsCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        ProjectsCmd::List { status, limit, json, org } => {
            let org = resolve_org(org)?;
            let mut path = format!("/organizations/{org}/projects?limit={limit}");
            if let Some(s) = status.filter(|s| !s.trim().is_empty()) {
                path.push_str(&format!("&status={}", urlencoding::encode(&s)));
            }
            let body = client.get(&path).await.context("GET projects")?;
            if json {
                print_json(&body);
            } else {
                print!("{}", render_projects(&body, limit));
            }
        }
        ProjectsCmd::Show { project, org } => {
            let org = resolve_org(org)?;
            let id = resolve_project_id(&client, &org, &project).await?;
            print_json(&client.get(&format!("/organizations/{org}/projects/{id}")).await.context("GET project")?);
        }
        ProjectsCmd::Create { name, key, description, org } => {
            let org = resolve_org(org)?;
            let mut body = json!({ "name": name, "key": key });
            if let Some(d) = description.filter(|s| !s.trim().is_empty()) {
                body["description"] = json!(d);
            }
            let r = client.post(&format!("/organizations/{org}/projects"), &body).await.context("POST project")?;
            let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
            let key = r.get("key").and_then(Value::as_str).unwrap_or("?");
            println!("  {} project {} {} created {}", "✚".green(), key.bold(), name.bold(), id.dimmed());
        }
        ProjectsCmd::Update {
            project,
            name,
            description,
            clear_description,
            org,
        } => {
            let org = resolve_org(org)?;
            let id = resolve_project_id(&client, &org, &project).await?;
            let mut body = serde_json::Map::new();
            if let Some(n) = name {
                body.insert("name".into(), json!(n));
            }
            if let Some(d) = description {
                body.insert("description".into(), json!(d));
            } else if clear_description {
                body.insert("description".into(), Value::Null);
            }
            if body.is_empty() {
                bail!("nothing to update — pass at least one of --name / --description / --clear-description");
            }
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/projects/{id}"), &Value::Object(body))
                    .await
                    .context("PATCH project")?,
            );
        }
        ProjectsCmd::Archive { project, org } => {
            let org = resolve_org(org)?;
            let id = resolve_project_id(&client, &org, &project).await?;
            client.delete(&format!("/organizations/{org}/projects/{id}")).await.context("DELETE project")?;
            println!("  {} project {} archived (history kept — nothing deleted)", "✓".green(), project.bold());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Work items
// ---------------------------------------------------------------------------

async fn items(cmd: ItemsCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        ItemsCmd::List { filters, json, org } => {
            let org = resolve_org(org)?;
            let project_id = match &filters.project {
                Some(p) => Some(resolve_project_id(&client, &org, p).await?),
                None => None,
            };
            let assignee_id = match &filters.assignee {
                Some(a) => Some(resolve_assignee_id(&client, &org, a).await?),
                None => None,
            };
            let path = format!(
                "/organizations/{org}/work-items{}",
                items_query(&filters, project_id.as_deref(), assignee_id.as_deref())
            );
            let body = client.get(&path).await.context("GET work-items")?;
            if json {
                print_json(&body);
            } else {
                print!("{}", render_items(&body, filters.limit));
            }
        }
        ItemsCmd::Show { item_id, org } => {
            let org = resolve_org(org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{org}/work-items/{item_id}"))
                    .await
                    .context("GET work item")?,
            );
        }
        ItemsCmd::Create {
            title,
            project,
            item_type,
            priority,
            description,
            assignee,
            due,
            parent,
            support_ticket,
            sprint,
            release,
            org,
        } => {
            let org = resolve_org(org)?;
            let mut body = json!({ "title": title });
            if let Some(p) = project {
                body["projectId"] = json!(resolve_project_id(&client, &org, &p).await?);
            }
            if let Some(t) = item_type {
                body["type"] = json!(t);
            }
            if let Some(p) = priority {
                body["priority"] = json!(p);
            }
            if let Some(d) = description {
                body["description"] = json!(d);
            }
            if let Some(a) = assignee {
                body["assigneeUserId"] = json!(resolve_assignee_id(&client, &org, &a).await?);
            }
            if let Some(d) = due {
                body["dueAt"] = json!(parse_ts(&d)?);
            }
            if let Some(p) = parent {
                body["parentWorkItemId"] = json!(p);
            }
            if let Some(s) = support_ticket {
                body["supportTicketId"] = json!(s);
            }
            if let Some(s) = sprint {
                body["sprintId"] = json!(s);
            }
            if let Some(r) = release {
                body["releaseId"] = json!(r);
            }
            let r = client
                .post(&format!("/organizations/{org}/work-items"), &body)
                .await
                .context("POST work item")?;
            let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
            let title = r.get("title").and_then(Value::as_str).unwrap_or("?");
            println!("  {} work item {} {} created", "✚".green(), short_id(id).dimmed(), title.bold());
        }
        ItemsCmd::Update { item_id, patch, org } => {
            let org = resolve_org(org)?;
            // Resolve human refs (project KEY, assignee me/email) to ids first,
            // so item_patch stays a pure flags → body mapping.
            let project_id = match &patch.project {
                Some(p) => Some(resolve_project_id(&client, &org, p).await?),
                None => None,
            };
            let assignee_id = match &patch.assignee {
                Some(a) => Some(resolve_assignee_id(&client, &org, a).await?),
                None => None,
            };
            let body = item_patch(&patch, project_id.as_deref(), assignee_id.as_deref())?;
            if body.as_object().is_some_and(serde_json::Map::is_empty) {
                bail!("nothing to update — pass at least one field flag (see `smoo work items update --help`)");
            }
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/work-items/{item_id}"), &body)
                    .await
                    .context("PATCH work item")?,
            );
        }
        ItemsCmd::Transition { item_id, status, org } => {
            let org = resolve_org(org)?;
            let r = client
                .patch(&format!("/organizations/{org}/work-items/{item_id}"), &json!({ "status": status }))
                .await
                .context("PATCH work item status")?;
            let title = r.get("title").and_then(Value::as_str).unwrap_or("?");
            println!("  {} {} → {}", "✓".green(), title.bold(), status_cell(&status));
        }
        ItemsCmd::Rm { item_id, org, dry_run, yes } => {
            let org = resolve_org(org)?;
            let proceed = crate::destructive::gate(
                &crate::destructive::Target {
                    verb: "delete",
                    noun: "work item",
                    id: &item_id,
                    org: &org,
                    severity: crate::destructive::Severity::Standard,
                },
                dry_run,
                yes,
            )?;
            if proceed {
                client
                    .delete(&format!("/organizations/{org}/work-items/{item_id}"))
                    .await
                    .context("DELETE work item")?;
                println!("  {} work item {} deleted", "✓".green(), short_id(&item_id).dimmed());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sprints
// ---------------------------------------------------------------------------

async fn sprints(cmd: SprintsCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        SprintsCmd::List {
            project,
            state,
            limit,
            json,
            org,
        } => {
            let org = resolve_org(org)?;
            let mut path = format!("/organizations/{org}/sprints?limit={limit}");
            if let Some(p) = project {
                path.push_str(&format!("&projectId={}", resolve_project_id(&client, &org, &p).await?));
            }
            if let Some(s) = state.filter(|s| !s.trim().is_empty()) {
                path.push_str(&format!("&state={}", urlencoding::encode(&s)));
            }
            let body = client.get(&path).await.context("GET sprints")?;
            if json {
                print_json(&body);
            } else {
                print!("{}", render_sprints(&body, limit));
            }
        }
        SprintsCmd::Show { sprint_id, org } => {
            let org = resolve_org(org)?;
            print_json(&client.get(&format!("/organizations/{org}/sprints/{sprint_id}")).await.context("GET sprint")?);
        }
        SprintsCmd::Create {
            name,
            project,
            goal,
            starts,
            ends,
            org,
        } => {
            let org = resolve_org(org)?;
            let project_id = resolve_project_id(&client, &org, &project).await?;
            let mut body = json!({ "name": name, "projectId": project_id });
            if let Some(g) = goal {
                body["goal"] = json!(g);
            }
            if let Some(s) = starts {
                body["startsAt"] = json!(parse_ts(&s)?);
            }
            if let Some(e) = ends {
                body["endsAt"] = json!(parse_ts(&e)?);
            }
            let r = client.post(&format!("/organizations/{org}/sprints"), &body).await.context("POST sprint")?;
            let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
            println!("  {} sprint {} {} created (planned)", "✚".green(), short_id(id).dimmed(), name.bold());
        }
        SprintsCmd::Start { sprint_id, org } => {
            let org = resolve_org(org)?;
            let r = client
                .post(&format!("/organizations/{org}/sprints/{sprint_id}/start"), &json!({}))
                .await
                .context("POST sprint start")?;
            let name = r.get("name").and_then(Value::as_str).unwrap_or("?");
            println!("  {} sprint {} is now {}", "✓".green(), name.bold(), state_cell("active"));
        }
        SprintsCmd::Complete { sprint_id, rollover_to, org } => {
            let org = resolve_org(org)?;
            let body = match rollover_to {
                Some(target) => json!({ "rolloverToSprintId": target }),
                None => json!({}),
            };
            let r = client
                .post(&format!("/organizations/{org}/sprints/{sprint_id}/complete"), &body)
                .await
                .context("POST sprint complete")?;
            let moved = r.get("movedCount").and_then(Value::as_i64).unwrap_or(0);
            let kept = r.get("keptCount").and_then(Value::as_i64).unwrap_or(0);
            println!(
                "  {} sprint completed — {moved} unfinished item(s) rolled forward, {kept} finished item(s) kept for history",
                "✓".green()
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Releases
// ---------------------------------------------------------------------------

async fn releases(cmd: ReleasesCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        ReleasesCmd::List {
            project,
            state,
            limit,
            json,
            org,
        } => {
            let org = resolve_org(org)?;
            let mut path = format!("/organizations/{org}/releases?limit={limit}");
            if let Some(p) = project {
                path.push_str(&format!("&projectId={}", resolve_project_id(&client, &org, &p).await?));
            }
            if let Some(s) = state.filter(|s| !s.trim().is_empty()) {
                path.push_str(&format!("&state={}", urlencoding::encode(&s)));
            }
            let body = client.get(&path).await.context("GET releases")?;
            if json {
                print_json(&body);
            } else {
                print!("{}", render_releases(&body, limit));
            }
        }
        ReleasesCmd::Create {
            name,
            project,
            description,
            target_date,
            org,
        } => {
            let org = resolve_org(org)?;
            let project_id = resolve_project_id(&client, &org, &project).await?;
            let mut body = json!({ "name": name, "projectId": project_id });
            if let Some(d) = description {
                body["description"] = json!(d);
            }
            if let Some(t) = target_date {
                body["targetDate"] = json!(parse_ts(&t)?);
            }
            let r = client.post(&format!("/organizations/{org}/releases"), &body).await.context("POST release")?;
            let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
            println!("  {} release {} {} created (planned)", "✚".green(), short_id(id).dimmed(), name.bold());
        }
        ReleasesCmd::Release { release_id, org } => {
            let org = resolve_org(org)?;
            let r = client
                .post(&format!("/organizations/{org}/releases/{release_id}/release"), &json!({}))
                .await
                .context("POST release release")?;
            let name = r.get("name").and_then(Value::as_str).unwrap_or("?");
            let at = r.get("releasedAt").and_then(Value::as_str).unwrap_or("");
            println!("  {} release {} shipped {}", "✓".green(), name.bold(), at.dimmed());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

async fn links(cmd: LinksCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        LinksCmd::List { item_id, json, org } => {
            let org = resolve_org(org)?;
            let body = client
                .get(&format!("/organizations/{org}/work-items/{item_id}/links"))
                .await
                .context("GET work-item links")?;
            if json {
                print_json(&body);
            } else {
                print!("{}", render_links(&body));
            }
        }
        LinksCmd::Link {
            item_id,
            file,
            url,
            item,
            link_type,
            title,
            org,
        } => {
            let org = resolve_org(org)?;
            let body = link_body(file.as_deref(), url.as_deref(), item.as_deref(), link_type.as_deref(), title.as_deref())?;
            let r = client
                .post(&format!("/organizations/{org}/work-items/{item_id}/links"), &body)
                .await
                .context("POST work-item link")?;
            let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
            let lt = r.get("linkType").and_then(Value::as_str).unwrap_or("?");
            println!("  {} link {} added ({lt})", "✚".green(), short_id(id).dimmed());
        }
        LinksCmd::Unlink { item_id, link_id, org } => {
            let org = resolve_org(org)?;
            client
                .delete(&format!("/organizations/{org}/work-items/{item_id}/links/{link_id}"))
                .await
                .context("DELETE work-item link")?;
            println!("  {} link {} removed", "✓".green(), short_id(&link_id).dimmed());
        }
        LinksCmd::ForFile { file_id, json, org } => {
            let org = resolve_org(org)?;
            let body = client
                .get(&format!("/organizations/{org}/files/{file_id}/work-item-links"))
                .await
                .context("GET file work-item links")?;
            if json {
                print_json(&body);
            } else {
                print!("{}", render_file_links(&body));
            }
        }
    }
    Ok(())
}

/// Build the POST body for `links link` — exactly one target among
/// file / url / item, with a per-kind default `linkType`.
fn link_body(file: Option<&str>, url: Option<&str>, item: Option<&str>, link_type: Option<&str>, title: Option<&str>) -> Result<Value> {
    let mut body = match (file, url, item) {
        (Some(f), None, None) => json!({ "targetKind": "managed_file", "targetId": f, "linkType": link_type.unwrap_or("reference") }),
        (None, Some(u), None) => json!({ "targetKind": "url", "url": u, "linkType": link_type.unwrap_or("reference") }),
        (None, None, Some(i)) => json!({ "targetKind": "work_item", "targetId": i, "linkType": link_type.unwrap_or("relates") }),
        _ => bail!("pass exactly one of --file / --url / --item"),
    };
    if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
        body["title"] = json!(t);
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// Jira sync
// ---------------------------------------------------------------------------

async fn jira(cmd: JiraCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        JiraCmd::Import { full, org } => {
            let org = resolve_org(org)?;
            let body = if full { json!({ "full": true }) } else { json!({}) };
            let r = client
                .post(&format!("/organizations/{org}/integrations/jira/import-work-items"), &body)
                .await
                .context("POST jira import-work-items")?;
            let wf = r.get("workflowId").and_then(Value::as_str).unwrap_or("?");
            let already = r.get("alreadyRunning").and_then(Value::as_bool).unwrap_or(false);
            if already {
                println!("  {} an import is already running ({})", "◐".cyan(), wf.dimmed());
            } else {
                let mode = if full { "full re-import" } else { "delta import" };
                println!("  {} {mode} started ({})", "✚".green(), wf.dimmed());
            }
            println!("  {}", "watch progress with `smoo work jira status`".dimmed());
        }
        JiraCmd::Status { json, org } => {
            let org = resolve_org(org)?;
            let body = client
                .get(&format!("/organizations/{org}/integrations/jira"))
                .await
                .context("GET jira integration")?;
            if json {
                print_json(&body);
            } else {
                print!("{}", render_jira_status(&body));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Resolution helpers
// ---------------------------------------------------------------------------

/// True for a 36-char hyphenated UUID (loose shape check, like the org switcher's).
fn looks_like_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, &b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                (b as char).is_ascii_hexdigit()
            }
        })
}

/// Resolve a project argument — a uuid passes through; anything else matches
/// the org's projects by KEY (case-insensitive) or exact name.
async fn resolve_project_id(client: &UserClient, org: &str, s: &str) -> Result<String> {
    if looks_like_uuid(s) {
        return Ok(s.to_string());
    }
    let body = client.get(&format!("/organizations/{org}/projects?limit=200")).await.context("GET projects")?;
    let rows = body.as_array().cloned().unwrap_or_default();
    let needle = s.trim();
    rows.iter()
        .find(|p| {
            p.get("key").and_then(Value::as_str).is_some_and(|k| k.eq_ignore_ascii_case(needle))
                || p.get("name").and_then(Value::as_str).is_some_and(|n| n.eq_ignore_ascii_case(needle))
        })
        .and_then(|p| p.get("id").and_then(Value::as_str).map(str::to_string))
        .with_context(|| format!("no project matches '{s}' — run `smoo work projects list`"))
}

/// Resolve `--assignee`: `me` → the logged-in user's org membership; a uuid
/// passes through; anything else is matched against member emails.
async fn resolve_assignee_id(client: &UserClient, org: &str, s: &str) -> Result<String> {
    let needle = if s.trim().eq_ignore_ascii_case("me") {
        UserClient::user_label().context("can't resolve `me` — no user email in the session; run `smoo auth login` again")?
    } else {
        s.trim().to_string()
    };
    if looks_like_uuid(&needle) {
        return Ok(needle);
    }
    let body = client.get(&format!("/organizations/{org}/members")).await.context("GET members")?;
    let needle = needle.to_lowercase();
    body.get("members")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter().find_map(|m| {
                let email = m.get("userEmail").and_then(Value::as_str)?.trim().to_lowercase();
                (email == needle).then(|| m.get("userId").and_then(Value::as_str).map(str::to_string)).flatten()
            })
        })
        .with_context(|| format!("no org member matches '{s}' — run `smoo api members list`"))
}

/// A timestamp flag: RFC3339 passes through; a bare `YYYY-MM-DD` becomes
/// midnight UTC that day.
fn parse_ts(s: &str) -> Result<String> {
    let s = s.trim();
    if chrono::DateTime::parse_from_rfc3339(s).is_ok() {
        return Ok(s.to_string());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(format!("{d}T00:00:00Z"));
    }
    bail!("'{s}' is not a date — pass RFC3339 (2026-08-25T14:00:00Z) or YYYY-MM-DD")
}

// ---------------------------------------------------------------------------
// Pure flag → wire mappings (unit-tested)
// ---------------------------------------------------------------------------

/// The `items list` filter flags as a query string. `project_id` /
/// `assignee_id` are the RESOLVED ids (KEY / `me` / email already looked up).
fn items_query(f: &ItemFilters, project_id: Option<&str>, assignee_id: Option<&str>) -> String {
    let mut params = vec![format!("limit={}", f.limit)];
    if let Some(p) = project_id {
        params.push(format!("projectId={}", urlencoding::encode(p)));
    }
    if let Some(s) = f.status.as_deref().filter(|s| !s.trim().is_empty()) {
        params.push(format!("status={}", urlencoding::encode(s)));
    }
    if let Some(t) = f.item_type.as_deref().filter(|s| !s.trim().is_empty()) {
        params.push(format!("type={}", urlencoding::encode(t)));
    }
    if let Some(a) = assignee_id {
        params.push(format!("assigneeUserId={}", urlencoding::encode(a)));
    }
    if let Some(t) = f.support_ticket.as_deref().filter(|s| !s.trim().is_empty()) {
        params.push(format!("supportTicketId={}", urlencoding::encode(t)));
    }
    if let Some(s) = f.sprint.as_deref().filter(|s| !s.trim().is_empty()) {
        params.push(format!("sprintId={}", urlencoding::encode(s)));
    }
    if let Some(r) = f.release.as_deref().filter(|s| !s.trim().is_empty()) {
        params.push(format!("releaseId={}", urlencoding::encode(r)));
    }
    format!("?{}", params.join("&"))
}

/// The `items update` flags as a PATCH body: a set flag puts the value, a
/// `--clear-*` flag puts an explicit null, everything else stays absent —
/// mirroring the server's present-sets / null-clears / absent-keeps contract.
fn item_patch(p: &ItemPatch, project_id: Option<&str>, assignee_id: Option<&str>) -> Result<Value> {
    let mut body = serde_json::Map::new();
    let mut field = |key: &str, set: Option<Value>, clear: bool| {
        if let Some(v) = set {
            body.insert(key.to_string(), v);
        } else if clear {
            body.insert(key.to_string(), Value::Null);
        }
    };
    field("title", p.title.as_deref().map(|v| json!(v)), false);
    field("description", p.description.as_deref().map(|v| json!(v)), p.clear_description);
    field("type", p.item_type.as_deref().map(|v| json!(v)), false);
    field("priority", p.priority.map(|v| json!(v)), false);
    field("projectId", project_id.map(|v| json!(v)), p.clear_project);
    field("assigneeUserId", assignee_id.map(|v| json!(v)), p.clear_assignee);
    field("parentWorkItemId", p.parent.as_deref().map(|v| json!(v)), p.clear_parent);
    field("supportTicketId", p.support_ticket.as_deref().map(|v| json!(v)), p.clear_support_ticket);
    field("sprintId", p.sprint.as_deref().map(|v| json!(v)), p.clear_sprint);
    field("releaseId", p.release.as_deref().map(|v| json!(v)), p.clear_release);
    let due = p.due.as_deref().map(parse_ts).transpose()?;
    if let Some(d) = due {
        body.insert("dueAt".to_string(), json!(d));
    } else if p.clear_due {
        body.insert("dueAt".to_string(), Value::Null);
    }
    Ok(Value::Object(body))
}

// ---------------------------------------------------------------------------
// Rendering — each returns the full block as a String so tests can pin it.
// ---------------------------------------------------------------------------

/// Priority label for the 0-4 scale (0 lowest – 4 highest, server default 2).
fn priority_label(p: i64) -> &'static str {
    match p {
        0 => "lowest",
        1 => "low",
        2 => "medium",
        3 => "high",
        4 => "highest",
        _ => "?",
    }
}

/// `3 high` — the number plus its label, padded to a fixed 9-char cell.
fn priority_cell(p: i64) -> String {
    format!("{:<9}", format!("{p} {}", priority_label(p)))
}

/// Work-item status as glyph + word, colored, padded to 14 visible chars.
/// Glyph carries the meaning so it survives `NO_COLOR` (CLI-Spec §4).
fn status_cell(status: &str) -> String {
    let padded = format!("{status:<12}");
    match status {
        "open" => format!("{} {}", "○", padded),
        "in_progress" => format!("{} {}", "◐".cyan(), padded.cyan()),
        "blocked" => format!("{} {}", "●".red(), padded.red()),
        "in_review" => format!("{} {}", "◐".yellow(), padded.yellow()),
        "done" => format!("{} {}", "●".green(), padded.green()),
        "cancelled" => format!("{} {}", "○".dimmed(), padded.dimmed()),
        _ => format!("○ {padded}"),
    }
}

/// Sprint / release state as glyph + word, colored, padded like [`status_cell`].
fn state_cell(state: &str) -> String {
    let padded = format!("{state:<10}");
    match state {
        "planned" => format!("{} {}", "○", padded),
        "active" => format!("{} {}", "◐".cyan(), padded.cyan()),
        "completed" | "released" => format!("{} {}", "●".green(), padded.green()),
        _ => format!("○ {padded}"),
    }
}

/// First 8 chars of an id — enough to paste back into `show`.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn short_date(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .map(|s| s.chars().take(10).collect::<String>())
        .unwrap_or_else(|| "—".into())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// Rows from a bare-array or `{data:[…]}` body.
fn rows(body: &Value) -> Vec<Value> {
    body.as_array()
        .cloned()
        .or_else(|| body.get("data").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

/// "showing first N — pass --limit for more" when the page came back full
/// (the routes return no total, so a full page is the truncation signal).
fn truncation_note(shown: usize, limit: u32) -> Option<String> {
    (shown as u32 >= limit).then(|| format!("  showing first {shown} — pass --limit for more\n"))
}

fn render_projects(body: &Value, limit: u32) -> String {
    use std::fmt::Write;
    let items = rows(body);
    let mut out = String::new();
    let _ = writeln!(out);
    let _ = writeln!(out, "  {} {}", "Projects".bold(), format!("({})", items.len()).dimmed());
    if items.is_empty() {
        let _ = writeln!(out, "\n  {}\n", "no projects yet — `smoo work projects create <name> --key KEY`".dimmed());
        return out;
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {}  {}  {}  {}",
        format!("{:<10}", "KEY").dimmed(),
        format!("{:<32}", "NAME").dimmed(),
        format!("{:<12}", "STATUS").dimmed(),
        "ID".dimmed()
    );
    for p in &items {
        let key = format!("{:<10}", p.get("key").and_then(Value::as_str).unwrap_or("—"));
        let name = format!("{:<32}", truncate(p.get("name").and_then(Value::as_str).unwrap_or("—"), 32));
        let status = p.get("status").and_then(Value::as_str).unwrap_or("—");
        let id = p.get("id").and_then(Value::as_str).unwrap_or("?");
        let _ = writeln!(out, "  {}  {}  {}  {}", key.bold(), name, state_cell(status), short_id(id).dimmed());
    }
    if let Some(note) = truncation_note(items.len(), limit) {
        let _ = write!(out, "{}", note.dimmed());
    }
    let _ = writeln!(out);
    out
}

fn render_items(body: &Value, limit: u32) -> String {
    use std::fmt::Write;
    let items = rows(body);
    let mut out = String::new();
    let _ = writeln!(out);
    let _ = writeln!(out, "  {} {}", "Work items".bold(), format!("({})", items.len()).dimmed());
    if items.is_empty() {
        let _ = writeln!(out, "\n  {}\n", "no work items match — this is a confirmed empty result, not an error".dimmed());
        return out;
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {}  {}  {}  {}  {}  {}",
        format!("{:<8}", "ID").dimmed(),
        format!("{:<14}", "STATUS").dimmed(),
        format!("{:<9}", "PRI").dimmed(),
        format!("{:<8}", "TYPE").dimmed(),
        format!("{:<42}", "TITLE").dimmed(),
        "DUE".dimmed()
    );
    for i in &items {
        let id = short_id(i.get("id").and_then(Value::as_str).unwrap_or("?"));
        let status = i.get("status").and_then(Value::as_str).unwrap_or("—");
        let pri = i.get("priority").and_then(Value::as_i64).unwrap_or(2);
        let ty = format!("{:<8}", i.get("type").and_then(Value::as_str).unwrap_or("—"));
        let title = format!("{:<42}", truncate(i.get("title").and_then(Value::as_str).unwrap_or("—"), 42));
        let due = short_date(i.get("dueAt"));
        let _ = writeln!(
            out,
            "  {}  {}  {}  {}  {}  {}",
            format!("{id:<8}").dimmed(),
            status_cell(status),
            priority_cell(pri),
            ty.dimmed(),
            title,
            due.dimmed()
        );
    }
    if let Some(note) = truncation_note(items.len(), limit) {
        let _ = write!(out, "{}", note.dimmed());
    }
    let _ = writeln!(out);
    out
}

fn render_sprints(body: &Value, limit: u32) -> String {
    use std::fmt::Write;
    let items = rows(body);
    let mut out = String::new();
    let _ = writeln!(out);
    let _ = writeln!(out, "  {} {}", "Sprints".bold(), format!("({})", items.len()).dimmed());
    if items.is_empty() {
        let _ = writeln!(out, "\n  {}\n", "no sprints match".dimmed());
        return out;
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {}  {}  {}  {}  {}",
        format!("{:<12}", "STATE").dimmed(),
        format!("{:<28}", "NAME").dimmed(),
        format!("{:<10}", "STARTS").dimmed(),
        format!("{:<10}", "ENDS").dimmed(),
        "ID".dimmed()
    );
    for s in &items {
        let state = s.get("state").and_then(Value::as_str).unwrap_or("—");
        let name = format!("{:<28}", truncate(s.get("name").and_then(Value::as_str).unwrap_or("—"), 28));
        let starts = format!("{:<10}", short_date(s.get("startsAt")));
        let ends = format!("{:<10}", short_date(s.get("endsAt")));
        let id = short_id(s.get("id").and_then(Value::as_str).unwrap_or("?"));
        let _ = writeln!(
            out,
            "  {}  {}  {}  {}  {}",
            state_cell(state),
            name.bold(),
            starts.dimmed(),
            ends.dimmed(),
            id.dimmed()
        );
    }
    if let Some(note) = truncation_note(items.len(), limit) {
        let _ = write!(out, "{}", note.dimmed());
    }
    let _ = writeln!(out);
    out
}

fn render_releases(body: &Value, limit: u32) -> String {
    use std::fmt::Write;
    let items = rows(body);
    let mut out = String::new();
    let _ = writeln!(out);
    let _ = writeln!(out, "  {} {}", "Releases".bold(), format!("({})", items.len()).dimmed());
    if items.is_empty() {
        let _ = writeln!(out, "\n  {}\n", "no releases match".dimmed());
        return out;
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {}  {}  {}  {}  {}",
        format!("{:<12}", "STATE").dimmed(),
        format!("{:<28}", "NAME").dimmed(),
        format!("{:<10}", "TARGET").dimmed(),
        format!("{:<10}", "RELEASED").dimmed(),
        "ID".dimmed()
    );
    for r in &items {
        let state = r.get("state").and_then(Value::as_str).unwrap_or("—");
        let name = format!("{:<28}", truncate(r.get("name").and_then(Value::as_str).unwrap_or("—"), 28));
        let target = format!("{:<10}", short_date(r.get("targetDate")));
        let released = format!("{:<10}", short_date(r.get("releasedAt")));
        let id = short_id(r.get("id").and_then(Value::as_str).unwrap_or("?"));
        let _ = writeln!(
            out,
            "  {}  {}  {}  {}  {}",
            state_cell(state),
            name.bold(),
            target.dimmed(),
            released.dimmed(),
            id.dimmed()
        );
    }
    if let Some(note) = truncation_note(items.len(), limit) {
        let _ = write!(out, "{}", note.dimmed());
    }
    let _ = writeln!(out);
    out
}

/// One line per link: `linkType → target` where target is the URL, the title,
/// or the target id depending on kind.
fn render_links(body: &Value) -> String {
    use std::fmt::Write;
    let items = rows(body);
    let mut out = String::new();
    let _ = writeln!(out);
    let _ = writeln!(out, "  {} {}", "Links".bold(), format!("({})", items.len()).dimmed());
    if items.is_empty() {
        let _ = writeln!(out, "\n  {}\n", "no links on this work item".dimmed());
        return out;
    }
    let _ = writeln!(out);
    for l in &items {
        let kind = l.get("targetKind").and_then(Value::as_str).unwrap_or("?");
        let lt = format!("{:<10}", l.get("linkType").and_then(Value::as_str).unwrap_or("?"));
        let target = l
            .get("url")
            .and_then(Value::as_str)
            .or_else(|| l.get("title").and_then(Value::as_str))
            .or_else(|| l.get("targetId").and_then(Value::as_str))
            .unwrap_or("—");
        let id = short_id(l.get("id").and_then(Value::as_str).unwrap_or("?"));
        let glyph = match kind {
            "url" => "🔗",
            "work_item" => "◆",
            _ => "📄",
        };
        let _ = writeln!(
            out,
            "  {} {}  {} {}  {}",
            glyph,
            lt.cyan(),
            target.bold(),
            format!("[{kind}]").dimmed(),
            id.dimmed()
        );
    }
    let _ = writeln!(out);
    out
}

/// Reverse listing: the work items whose links reference a file.
fn render_file_links(body: &Value) -> String {
    use std::fmt::Write;
    let items = rows(body);
    let mut out = String::new();
    let _ = writeln!(out);
    let _ = writeln!(out, "  {} {}", "Work items referencing this file".bold(), format!("({})", items.len()).dimmed());
    if items.is_empty() {
        let _ = writeln!(out, "\n  {}\n", "no work items reference this file".dimmed());
        return out;
    }
    let _ = writeln!(out);
    for l in &items {
        // Rows are link rows; the reverse listing may enrich them with the
        // item's title/status — render those when present, ids otherwise.
        let item_id = short_id(l.get("workItemId").and_then(Value::as_str).unwrap_or("?"));
        let lt = l.get("linkType").and_then(Value::as_str).unwrap_or("?");
        let title = l
            .get("workItemTitle")
            .and_then(Value::as_str)
            .or_else(|| l.get("title").and_then(Value::as_str))
            .unwrap_or("");
        let _ = writeln!(
            out,
            "  {} {}  {} {}",
            "◆".cyan(),
            format!("{item_id:<8}").bold(),
            title,
            format!("({lt})").dimmed()
        );
    }
    let _ = writeln!(out);
    out
}

/// Human summary of `metadata.workItemsSync` off the Jira integration row —
/// the shape `write_sync_state` in the temporal worker maintains:
/// `{ state, watermark, error, updatedAt }`.
fn render_jira_status(body: &Value) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out);
    let _ = writeln!(out, "  {}", "Jira work-items sync".bold());
    let _ = writeln!(out);

    let integration_status = body.get("status").and_then(Value::as_str).unwrap_or("?");
    let keys = body
        .get("projectKeys")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(none configured)".to_string());
    let _ = writeln!(out, "  {}  {}", format!("{:<12}", "integration").dimmed(), state_cell(integration_status));
    let _ = writeln!(out, "  {}  {}", format!("{:<12}", "projectKeys").dimmed(), keys);

    let sync = body.pointer("/metadata/workItemsSync");
    match sync {
        None | Some(Value::Null) => {
            let _ = writeln!(
                out,
                "  {}  {}",
                format!("{:<12}", "sync").dimmed(),
                "never imported — run `smoo work jira import`".dimmed()
            );
        }
        Some(sync) => {
            let state = sync.get("state").and_then(Value::as_str).unwrap_or("?");
            let glyph = match state {
                "running" => "◐".cyan().to_string(),
                "idle" => "●".green().to_string(),
                _ => "●".red().to_string(),
            };
            let _ = writeln!(out, "  {}  {} {}", format!("{:<12}", "sync").dimmed(), glyph, state);
            let watermark = sync.get("watermark").and_then(Value::as_str).unwrap_or("—");
            let _ = writeln!(out, "  {}  {}", format!("{:<12}", "watermark").dimmed(), watermark);
            if let Some(err) = sync.get("error").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                let _ = writeln!(out, "  {}  {}", format!("{:<12}", "error").dimmed(), err.red());
            }
            if let Some(at) = sync.get("updatedAt").and_then(Value::as_str) {
                let _ = writeln!(out, "  {}  {}", format!("{:<12}", "updated").dimmed(), at.dimmed());
            }
        }
    }
    let _ = writeln!(out);
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
        let argv: Vec<&str> = std::iter::once("t").chain(args.iter().copied()).collect();
        Wrap::try_parse_from(argv).unwrap_or_else(|e| panic!("{args:?} must parse: {e}")).cmd
    }

    // ---- clap parse coverage (CLI-Spec §8.5: a parse test for every verb) ----

    #[test]
    fn every_verb_parses() {
        parse(&["projects", "list"]);
        parse(&["projects", "show", "SMOODEV"]);
        parse(&["projects", "create", "Support", "--key", "SUP"]);
        parse(&["projects", "update", "SUP", "--name", "Support 2"]);
        parse(&["projects", "archive", "SUP"]);
        parse(&["items", "list"]);
        parse(&["items", "show", "abc"]);
        parse(&["items", "create", "Fix login", "--type", "bug", "--priority", "3"]);
        parse(&["items", "update", "abc", "--title", "New"]);
        parse(&["items", "transition", "abc", "done"]);
        parse(&["items", "rm", "abc"]);
        parse(&["sprints", "list"]);
        parse(&["sprints", "show", "abc"]);
        parse(&["sprints", "create", "Sprint 1", "--project", "SMOODEV"]);
        parse(&["sprints", "start", "abc"]);
        parse(&["sprints", "complete", "abc", "--rollover-to", "def"]);
        parse(&["releases", "list"]);
        parse(&["releases", "create", "v2.1", "--project", "SMOODEV"]);
        parse(&["releases", "release", "abc"]);
        parse(&["links", "list", "abc"]);
        parse(&["links", "link", "abc", "--url", "https://x.com"]);
        parse(&["links", "unlink", "abc", "def"]);
        parse(&["links", "for-file", "abc"]);
        parse(&["jira", "import", "--full"]);
        parse(&["jira", "status"]);
    }

    /// CLI-Spec §flags: every platform `list` verb offers `--json`.
    #[test]
    fn lists_accept_json_flag_and_default_to_off() {
        assert!(matches!(
            parse(&["projects", "list", "--json"]),
            Cmd::Projects {
                cmd: ProjectsCmd::List { json: true, .. }
            }
        ));
        assert!(matches!(
            parse(&["projects", "list"]),
            Cmd::Projects {
                cmd: ProjectsCmd::List { json: false, .. }
            }
        ));
        assert!(matches!(
            parse(&["items", "list", "--json"]),
            Cmd::Items {
                cmd: ItemsCmd::List { json: true, .. }
            }
        ));
        assert!(matches!(
            parse(&["sprints", "list", "--json"]),
            Cmd::Sprints {
                cmd: SprintsCmd::List { json: true, .. }
            }
        ));
        assert!(matches!(
            parse(&["releases", "list", "--json"]),
            Cmd::Releases {
                cmd: ReleasesCmd::List { json: true, .. }
            }
        ));
        assert!(matches!(
            parse(&["links", "list", "x", "--json"]),
            Cmd::Links {
                cmd: LinksCmd::List { json: true, .. }
            }
        ));
        assert!(matches!(
            parse(&["jira", "status", "--json"]),
            Cmd::Jira {
                cmd: JiraCmd::Status { json: true, .. }
            }
        ));
    }

    #[test]
    fn priority_flag_rejects_out_of_range() {
        let argv = ["t", "items", "create", "X", "--priority", "5"];
        assert!(Wrap::try_parse_from(argv).is_err(), "priority 5 must be rejected at parse time");
    }

    #[test]
    fn set_and_clear_flags_conflict() {
        let argv = ["t", "items", "update", "abc", "--assignee", "me", "--clear-assignee"];
        assert!(Wrap::try_parse_from(argv).is_err(), "--assignee and --clear-assignee must conflict");
        let argv = ["t", "items", "update", "abc", "--due", "2026-01-01", "--clear-due"];
        assert!(Wrap::try_parse_from(argv).is_err(), "--due and --clear-due must conflict");
    }

    #[test]
    fn link_targets_conflict() {
        let argv = ["t", "links", "link", "abc", "--file", "f1", "--url", "https://x.com"];
        assert!(Wrap::try_parse_from(argv).is_err(), "--file and --url must conflict");
    }

    // ---- filter flag → query param mapping ----

    #[test]
    fn items_query_maps_every_filter() {
        let f = ItemFilters {
            status: Some("open".into()),
            item_type: Some("bug".into()),
            support_ticket: Some("t-1".into()),
            sprint: Some("s-1".into()),
            release: Some("r-1".into()),
            limit: 25,
            ..Default::default()
        };
        let q = items_query(&f, Some("p-1"), Some("u-1"));
        assert_eq!(
            q,
            "?limit=25&projectId=p-1&status=open&type=bug&assigneeUserId=u-1&supportTicketId=t-1&sprintId=s-1&releaseId=r-1"
        );
    }

    #[test]
    fn items_query_omits_absent_filters() {
        let f = ItemFilters {
            limit: 50,
            ..Default::default()
        };
        assert_eq!(items_query(&f, None, None), "?limit=50");
        // Whitespace-only values are treated as absent, not sent as empties.
        let f = ItemFilters {
            status: Some("  ".into()),
            limit: 50,
            ..Default::default()
        };
        assert_eq!(items_query(&f, None, None), "?limit=50");
    }

    // ---- set-vs-clear PATCH bodies ----

    #[test]
    fn item_patch_sends_only_changed_fields() {
        let p = ItemPatch {
            title: Some("New title".into()),
            priority: Some(4),
            ..Default::default()
        };
        let body = item_patch(&p, None, None).expect("patch");
        assert_eq!(body, json!({ "title": "New title", "priority": 4 }));
    }

    #[test]
    fn item_patch_clear_flags_send_explicit_null() {
        let p = ItemPatch {
            clear_assignee: true,
            clear_due: true,
            clear_sprint: true,
            ..Default::default()
        };
        let body = item_patch(&p, None, None).expect("patch");
        assert_eq!(body, json!({ "assigneeUserId": null, "dueAt": null, "sprintId": null }));
        // The null keys must be PRESENT (absent would mean "keep").
        assert!(body.as_object().unwrap().contains_key("assigneeUserId"));
    }

    #[test]
    fn item_patch_set_wins_over_absent_and_resolved_ids_flow_through() {
        let p = ItemPatch {
            due: Some("2026-09-01".into()),
            ..Default::default()
        };
        let body = item_patch(&p, Some("proj-1"), Some("user-1")).expect("patch");
        assert_eq!(
            body,
            json!({ "projectId": "proj-1", "assigneeUserId": "user-1", "dueAt": "2026-09-01T00:00:00Z" })
        );
    }

    #[test]
    fn item_patch_rejects_malformed_due() {
        let p = ItemPatch {
            due: Some("tomorrow".into()),
            ..Default::default()
        };
        assert!(item_patch(&p, None, None).is_err(), "a non-date --due must error, not silently drop");
    }

    // ---- link body ----

    #[test]
    fn link_body_per_kind_defaults() {
        assert_eq!(
            link_body(Some("f-1"), None, None, None, None).unwrap(),
            json!({ "targetKind": "managed_file", "targetId": "f-1", "linkType": "reference" })
        );
        assert_eq!(
            link_body(None, Some("https://x.com"), None, None, Some("Docs")).unwrap(),
            json!({ "targetKind": "url", "url": "https://x.com", "linkType": "reference", "title": "Docs" })
        );
        assert_eq!(
            link_body(None, None, Some("wi-1"), Some("blocks"), None).unwrap(),
            json!({ "targetKind": "work_item", "targetId": "wi-1", "linkType": "blocks" })
        );
        assert!(link_body(None, None, None, None, None).is_err(), "no target must error");
    }

    // ---- rendering ----

    fn item_fixture() -> serde_json::Value {
        json!([
            {
                "id": "11111111-2222-4333-8444-555555555555",
                "title": "Fix the login redirect loop on Safari",
                "type": "bug",
                "status": "in_progress",
                "priority": 3,
                "dueAt": "2026-09-01T00:00:00Z"
            },
            {
                "id": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
                "title": "Ship the onboarding email sequence",
                "type": "task",
                "status": "done",
                "priority": 2,
                "dueAt": null
            }
        ])
    }

    #[test]
    fn items_table_renders_fixture_columns() {
        let out = render_items(&item_fixture(), 50);
        assert!(out.contains("Work items"), "{out}");
        assert!(out.contains("(2)"), "{out}");
        // Short ids, statuses with glyphs, priorities as number + label.
        assert!(out.contains("11111111"), "{out}");
        assert!(out.contains("◐"), "in_progress glyph: {out}");
        assert!(out.contains("●"), "done glyph: {out}");
        assert!(out.contains("3 high"), "priority number + label: {out}");
        assert!(out.contains("2 medium"), "{out}");
        assert!(out.contains("Fix the login redirect loop on Safari"), "{out}");
        assert!(out.contains("2026-09-01"), "due date, date part only: {out}");
        // A null dueAt renders as an em dash, not "null".
        assert!(out.contains('—'), "{out}");
        assert!(!out.contains("null"), "{out}");
        // Not a full page → no truncation note.
        assert!(!out.contains("showing first"), "{out}");
    }

    #[test]
    fn items_table_empty_is_an_answer() {
        let out = render_items(&json!([]), 50);
        assert!(out.contains("(0)"), "{out}");
        assert!(out.contains("confirmed empty result"), "{out}");
    }

    #[test]
    fn full_page_reports_truncation() {
        let many: Vec<serde_json::Value> = (0..3)
            .map(|i| json!({ "id": format!("{i}"), "title": "x", "type": "task", "status": "open", "priority": 2 }))
            .collect();
        let out = render_items(&json!(many), 3);
        assert!(out.contains("showing first 3"), "a full page must report truncation: {out}");
    }

    #[test]
    fn projects_table_renders_key_and_status() {
        let body = json!([{ "id": "11111111-2222-4333-8444-555555555555", "key": "SMOODEV", "name": "Smoo Dev", "status": "active" }]);
        let out = render_projects(&body, 50);
        assert!(out.contains("SMOODEV"), "{out}");
        assert!(out.contains("Smoo Dev"), "{out}");
        assert!(out.contains("active"), "{out}");
    }

    #[test]
    fn priority_labels_cover_the_scale() {
        assert_eq!(priority_label(0), "lowest");
        assert_eq!(priority_label(2), "medium");
        assert_eq!(priority_label(4), "highest");
        assert_eq!(priority_label(9), "?");
    }

    // ---- jira status rendering ----

    #[test]
    fn jira_status_renders_sync_state() {
        let body = json!({
            "status": "active",
            "projectKeys": ["SMOODEV", "OPS"],
            "metadata": {
                "workItemsSync": {
                    "state": "idle",
                    "watermark": "2026-08-20T10:00:00.000Z",
                    "error": null,
                    "updatedAt": "2026-08-20T10:05:00Z"
                }
            }
        });
        let out = render_jira_status(&body);
        assert!(out.contains("SMOODEV, OPS"), "{out}");
        assert!(out.contains("idle"), "{out}");
        assert!(out.contains("2026-08-20T10:00:00.000Z"), "{out}");
        assert!(out.contains("2026-08-20T10:05:00Z"), "{out}");
        assert!(!out.contains("error"), "null error must not render a row: {out}");
    }

    #[test]
    fn jira_status_shows_error_and_never_imported() {
        let body = json!({
            "status": "active",
            "projectKeys": ["SMOODEV"],
            "metadata": { "workItemsSync": { "state": "idle", "watermark": null, "error": "no projectKeys configured", "updatedAt": "2026-08-20T10:05:00Z" } }
        });
        let out = render_jira_status(&body);
        assert!(out.contains("no projectKeys configured"), "{out}");

        let never = render_jira_status(&json!({ "status": "active", "projectKeys": [] }));
        assert!(never.contains("never imported"), "{never}");
        assert!(never.contains("(none configured)"), "{never}");
    }

    // ---- misc helpers ----

    #[test]
    fn parse_ts_accepts_rfc3339_and_bare_dates() {
        assert_eq!(parse_ts("2026-09-01").unwrap(), "2026-09-01T00:00:00Z");
        assert_eq!(parse_ts("2026-09-01T14:30:00Z").unwrap(), "2026-09-01T14:30:00Z");
        assert!(parse_ts("next tuesday").is_err());
    }

    #[test]
    fn uuid_shape_detection() {
        assert!(looks_like_uuid("11111111-2222-4333-8444-555555555555"));
        assert!(!looks_like_uuid("SMOODEV"));
        assert!(!looks_like_uuid("11111111-2222-4333-8444-55555555555")); // 35 chars
    }
}
