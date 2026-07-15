//! `th api crm contacts …` — CRM contacts via the platform API,
//! authenticated as the logged-in *user* (`smooai-user.json`), so
//! writes are attributed to a real person (e.g. tara@offsetwell.com)
//! rather than an M2M client. SMOODEV-1735.
//!
//! `import` is an idempotent bulk upsert from a JSON array file. The
//! dedup key is the lowercased email, falling back to the last 10
//! digits of the phone. Re-running adds zero new rows. `--dry-run`
//! resolves the org, parses the file, fetches the existing contacts,
//! and prints what it WOULD do without writing.

use std::collections::HashMap;

use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::{print_json, read_body};
use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// Contact records (list / get / create / update / import).
    #[command(visible_alias = "contact")]
    Contacts {
        #[command(subcommand)]
        cmd: ContactsCmd,
    },
    /// Company / account records (list / show / upsert).
    #[command(visible_alias = "company")]
    Companies {
        #[command(subcommand)]
        cmd: CompaniesCmd,
    },
    /// Deals — your sales pipeline (list / show / create / move).
    #[command(visible_alias = "deal")]
    Deals {
        #[command(subcommand)]
        cmd: DealsCmd,
    },
    /// Pipeline board — weighted forecast by stage (the money view).
    Pipeline {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw forecast JSON instead of the board.
        #[arg(long)]
        json: bool,
    },
    /// Pipeline stage catalog (list / show / create / update / reorder / init).
    #[command(visible_alias = "stage")]
    Stages {
        #[command(subcommand)]
        cmd: StagesCmd,
    },
    /// Tasks — next actions on deals & contacts.
    #[command(visible_alias = "task")]
    Tasks {
        #[command(subcommand)]
        cmd: TasksCmd,
    },
    /// Conversations — email threads with contacts.
    #[command(visible_alias = "conversation")]
    Conversations {
        #[command(subcommand)]
        cmd: ConversationsCmd,
    },
    /// Timeline — unified, date-sorted history for a deal.
    Timeline {
        /// The deal id from `th api crm deals list`.
        deal_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw timeline JSON instead of the rendered view.
        #[arg(long)]
        json: bool,
    },
    /// Invoices — revenue actuals (read-only; Stripe-backed).
    #[command(visible_alias = "invoice")]
    Invoices {
        #[command(subcommand)]
        cmd: InvoicesCmd,
    },
    /// Associations — link entities (contacts, companies, deals, …) to each other.
    #[command(visible_aliases = ["associations", "association", "links"])]
    Assoc {
        #[command(subcommand)]
        cmd: AssocCmd,
    },
    /// Set a reminder on a CRM entity: `remind contact:jane@acme.com --at tomorrow`.
    /// Use `remind cancel <id>` to cancel one.
    Remind {
        /// Entity ref `TYPE:REF` (e.g. `contact:jane@acme.com`, `deal:"Acme renewal"`,
        /// `company:<uuid>`) — or the literal `cancel` to cancel a reminder by id.
        target: String,
        /// With `cancel`: the reminder id to cancel.
        id: Option<String>,
        /// When to fire — `tomorrow`, `"next week"`, `"in 3 days"`, `2026-08-01`, or full RFC3339.
        #[arg(long)]
        at: Option<String>,
        /// Optional note shown with the reminder.
        #[arg(long)]
        note: Option<String>,
        /// Assign to a member (email or user-id uuid). Defaults to you.
        #[arg(long)]
        assignee: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// CRM reminders — list your due reminders, or a single entity's.
    #[command(visible_alias = "reminder")]
    Reminders {
        #[command(subcommand)]
        cmd: RemindersCmd,
    },
}

#[derive(Subcommand)]
pub enum RemindersCmd {
    /// List reminders — your own pending ones (`--mine`) or a single entity's (`--entity`).
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Your own pending reminders, soonest-due first.
        #[arg(long)]
        mine: bool,
        /// A single entity's reminders — `TYPE:REF` (e.g. `contact:jane@acme.com`).
        #[arg(long)]
        entity: Option<String>,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Cancel (soft-delete) a reminder by id.
    Cancel {
        /// The reminder id from `th api crm reminders list`.
        id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum CompaniesCmd {
    /// List companies for the org.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Filter to companies matching this search term.
        #[arg(long)]
        search: Option<String>,
        /// Maximum number of companies to return.
        #[arg(long, default_value = "50")]
        limit: u32,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Show a single company by id.
    Show {
        /// The company id from `th api crm companies list`.
        company_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create or update a company, matched by name (case-insensitive) or
    /// domain. Safe to re-run — a second call patches the same row.
    Upsert {
        /// Company name (the match key).
        name: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Website domain (e.g. rpmpizza.com) — also a match key.
        #[arg(long)]
        domain: Option<String>,
        /// Industry (free text).
        #[arg(long)]
        industry: Option<String>,
        /// Website URL.
        #[arg(long)]
        website: Option<String>,
    },
    /// Upload a local image and make it the company's logo.
    SetImage {
        /// The company id from `th api crm companies list`.
        company_id: String,
        /// Path to a local image (png, jpg, jpeg, gif or webp).
        path: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Resolve the company's logo server-side from its domain (no local file).
    ResolveLogo {
        /// The company id from `th api crm companies list`.
        company_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove the company's logo (falls back to initials).
    RemoveImage {
        /// The company id from `th api crm companies list`.
        company_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Set (or clear) the company's parent (a `parent` association to another
    /// company). PARENT is a uuid, name, or `none`/`-` to clear.
    SetParent {
        /// The child company — id or name.
        company: String,
        /// The parent company — id or name, or `none`/`-` to clear.
        parent: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum DealsCmd {
    /// List deals as a pipeline view (totals + table).
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Filter to a single stage.
        #[arg(long)]
        stage: Option<String>,
        /// Maximum number of deals to return.
        #[arg(long, default_value = "50")]
        limit: u32,
        /// Print raw JSON instead of the pipeline view.
        #[arg(long)]
        json: bool,
    },
    /// Show a single deal by id.
    Show {
        /// The deal id from `th api crm deals list`.
        deal_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a deal, matched by title (case-insensitive). Safe to
    /// re-run — an existing deal with the same title is left untouched
    /// (use `move` to change its stage).
    Create {
        /// Deal title (the match key).
        title: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Deal value in dollars (e.g. 5500).
        #[arg(long)]
        value: Option<f64>,
        /// Pipeline stage (free text, e.g. "closed_won", "discovery").
        #[arg(long)]
        stage: Option<String>,
        /// Link to a company id.
        #[arg(long)]
        company: Option<String>,
        /// Link to a contact id.
        #[arg(long)]
        contact: Option<String>,
        /// Close date (ISO-8601, e.g. 2026-07-02).
        #[arg(long = "close-date")]
        close_date: Option<String>,
    },
    /// Move a deal to a new stage.
    Move {
        /// The deal id from `th api crm deals list`.
        deal_id: String,
        /// New stage (free text).
        stage: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Upload a local image and make it the deal's image.
    SetImage {
        /// The deal id from `th api crm deals list`.
        deal_id: String,
        /// Path to a local image (png, jpg, jpeg, gif or webp).
        path: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove the deal's own image (its displayed image then falls back to the
    /// linked company logo / contact avatar). No-op when the image is inherited.
    RemoveImage {
        /// The deal id from `th api crm deals list`.
        deal_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Set (or clear) the deal's primary contact. Writes the legacy `contactId` FK.
    SetContact {
        /// The deal — id or title.
        deal: String,
        /// The contact — id, email, or name, or `none`/`-` to clear.
        contact: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Set (or clear) the deal's company. Writes the legacy `companyId` FK.
    SetCompany {
        /// The deal — id or title.
        deal: String,
        /// The company — id or name, or `none`/`-` to clear.
        company: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum StagesCmd {
    /// List pipeline stages in board order.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Show a single stage by id.
    Show {
        /// The stage id from `th api crm stages list`.
        stage_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a pipeline stage.
    Create {
        /// Stage name (e.g. "Discovery").
        name: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Board position (lower = earlier).
        #[arg(long)]
        position: Option<i64>,
        /// Win probability, 0–100.
        #[arg(long)]
        probability: Option<f64>,
        /// Mark this stage as a won (closed-won) stage.
        #[arg(long)]
        won: bool,
        /// Mark this stage as a lost (closed-lost) stage.
        #[arg(long)]
        lost: bool,
        /// Hex color for the stage chip (e.g. #22c55e).
        #[arg(long)]
        color: Option<String>,
    },
    /// Update a pipeline stage.
    Update {
        /// The stage id from `th api crm stages list`.
        stage_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// New stage name.
        #[arg(long)]
        name: Option<String>,
        /// New board position.
        #[arg(long)]
        position: Option<i64>,
        /// New win probability, 0–100.
        #[arg(long)]
        probability: Option<f64>,
        /// Mark as a won (closed-won) stage.
        #[arg(long)]
        won: bool,
        /// Mark as a lost (closed-lost) stage.
        #[arg(long)]
        lost: bool,
        /// New hex color.
        #[arg(long)]
        color: Option<String>,
    },
    /// Delete a pipeline stage.
    Delete {
        /// The stage id from `th api crm stages list`.
        stage_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Reorder stages — pass ids in the desired board order.
    Reorder {
        /// Stage ids, in the new order.
        ids: Vec<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Seed the default pipeline stages (idempotent).
    Init {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum TasksCmd {
    /// List tasks (open by default).
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Filter to a deal (id, or title to resolve).
        #[arg(long)]
        deal: Option<String>,
        /// Filter to a contact (id, email, or name to resolve).
        #[arg(long)]
        contact: Option<String>,
        /// Filter to an assignee user id.
        #[arg(long)]
        assignee: Option<String>,
        /// Only show tasks past their due date.
        #[arg(long)]
        overdue: bool,
        /// Include completed tasks too.
        #[arg(long)]
        all: bool,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Show a single task by id.
    Show {
        /// The task id from `th api crm tasks list`.
        task_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Add a task.
    Add {
        /// Task title.
        title: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Link to a deal (id, or title to resolve).
        #[arg(long)]
        deal: Option<String>,
        /// Link to a contact (id, email, or name to resolve).
        #[arg(long)]
        contact: Option<String>,
        /// Link to a company (id, or name to resolve).
        #[arg(long)]
        company: Option<String>,
        /// Due date (ISO-8601, e.g. 2026-07-10).
        #[arg(long)]
        due: Option<String>,
        /// Free-text description.
        #[arg(long)]
        description: Option<String>,
        /// Assignee user id.
        #[arg(long)]
        assignee: Option<String>,
    },
    /// Update a task.
    Update {
        /// The task id from `th api crm tasks list`.
        task_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// New title.
        #[arg(long)]
        title: Option<String>,
        /// New due date (ISO-8601).
        #[arg(long)]
        due: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// New assignee user id.
        #[arg(long)]
        assignee: Option<String>,
    },
    /// Mark a task complete.
    Done {
        /// The task id from `th api crm tasks list`.
        task_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a task.
    Rm {
        /// The task id from `th api crm tasks list`.
        task_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ConversationsCmd {
    /// List conversations (optionally by contact or deal).
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Filter to a contact (id, email, or name to resolve).
        #[arg(long)]
        contact: Option<String>,
        /// Filter to a deal id.
        #[arg(long)]
        deal: Option<String>,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Show a conversation thread as a timeline.
    Show {
        /// The conversation id from `th api crm conversations list`.
        conversation_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the thread view.
        #[arg(long)]
        json: bool,
    },
    /// Start a conversation, optionally with a first message.
    Create {
        /// Conversation name / subject line.
        name: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Platform (default: email).
        #[arg(long, default_value = "email")]
        platform: String,
        /// Link to a contact (id, email, or name to resolve).
        #[arg(long)]
        contact: Option<String>,
        /// First message subject.
        #[arg(long)]
        subject: Option<String>,
        /// First message body.
        #[arg(long)]
        body: Option<String>,
    },
    /// Append an email message to a conversation.
    AddEmail {
        /// The conversation id from `th api crm conversations list`.
        conversation_id: String,
        /// Message direction.
        #[arg(value_parser = ["inbound", "outbound"])]
        direction: String,
        /// Message body.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Message subject.
        #[arg(long)]
        subject: Option<String>,
        /// When it occurred (ISO-8601; defaults to now server-side).
        #[arg(long)]
        occurred: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum InvoicesCmd {
    /// List invoices (optionally by deal or contact).
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Filter to a deal id.
        #[arg(long)]
        deal: Option<String>,
        /// Filter to a contact id.
        #[arg(long)]
        contact: Option<String>,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Show a single invoice by id.
    Show {
        /// The invoice id from `th api crm invoices list`.
        invoice_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ContactsCmd {
    /// List contacts for the org.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Filter to contacts matching this search term.
        #[arg(long)]
        search: Option<String>,
        /// Maximum number of contacts to return.
        #[arg(long, default_value = "50")]
        limit: u32,
    },
    /// Get a single contact by id.
    Get {
        /// The contact id from `th api crm contacts list`.
        contact_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a contact from a JSON body (file path, or `-` for stdin).
    Create {
        /// JSON body (file path, or `-` for stdin) with the new contact's fields.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update a contact from a JSON body (file path, or `-` for stdin).
    Update {
        /// The contact id from `th api crm contacts list`.
        contact_id: String,
        /// JSON body (file path, or `-` for stdin) with the fields to patch.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Idempotent bulk upsert from a JSON array file. Dedup key:
    /// lowercased email, else last-10 phone digits. `--dry-run` to
    /// preview without writing.
    Import {
        /// Path to a JSON file containing an array of contact objects.
        file: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Preview what would be created/updated without writing.
        #[arg(long)]
        dry_run: bool,
        /// Minimum delay between API writes, in ms. The contacts API rate
        /// limits at 100 requests / 60s per auth token, so the default
        /// (700ms ≈ 85/min) stays safely under it. On a rate-limit error the
        /// import also waits 61s and retries.
        #[arg(long, default_value = "700")]
        rate_ms: u64,
    },
    /// Upload a local image and make it the contact's primary avatar.
    SetImage {
        /// The contact id from `th api crm contacts list`.
        contact_id: String,
        /// Path to a local image (png, jpg, jpeg, gif or webp).
        path: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove the contact's avatar (falls back to initials).
    RemoveImage {
        /// The contact id from `th api crm contacts list`.
        contact_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Set (or clear) the contact's company. COMPANY is a uuid, name, or
    /// `none`/`-` to unlink. Writes through the legacy `companyId` FK.
    SetCompany {
        /// The contact — id, email, or name.
        contact: String,
        /// The company — id or name, or `none`/`-` to clear.
        company: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum AssocCmd {
    /// Link two entities. FROM/TO are `TYPE:REF` (e.g. `contact:jane@x.com`,
    /// `company:Acme`, `deal:"Big Deal"`, or `task:<uuid>`).
    Link {
        /// Source entity as `TYPE:REF`.
        from: String,
        /// Target entity as `TYPE:REF`.
        to: String,
        /// Association type (free text; system values: primary, parent, related). Defaults to related.
        #[arg(long = "as")]
        association_type: Option<String>,
        /// Human-readable label for the association.
        #[arg(long)]
        label: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove the link between two entities (both as `TYPE:REF`).
    Unlink {
        /// Source entity as `TYPE:REF`.
        from: String,
        /// Target entity as `TYPE:REF`.
        to: String,
        /// Only unlink this association type (required when several types link the pair).
        #[arg(long = "as")]
        association_type: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// List an entity's associations. ENTITY is `TYPE:REF`.
    List {
        /// The entity as `TYPE:REF`.
        entity: String,
        /// Filter to associations whose other entity is this type.
        #[arg(long = "type")]
        other_type: Option<String>,
        /// Filter to this association type.
        #[arg(long = "as")]
        association_type: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Change an association's type, by association id.
    SetType {
        /// The association id from `th api crm assoc list`.
        association_id: String,
        /// The new association type (free text).
        association_type: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Change an association's label, by association id.
    SetLabel {
        /// The association id from `th api crm assoc list`.
        association_id: String,
        /// The new label.
        label: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Contacts { cmd } => contacts(cmd).await,
        Cmd::Companies { cmd } => companies(cmd).await,
        Cmd::Deals { cmd } => deals(cmd).await,
        Cmd::Pipeline { org, json } => pipeline(org, json).await,
        Cmd::Stages { cmd } => stages(cmd).await,
        Cmd::Tasks { cmd } => tasks(cmd).await,
        Cmd::Conversations { cmd } => conversations(cmd).await,
        Cmd::Timeline { deal_id, org, json } => timeline(deal_id, org, json).await,
        Cmd::Invoices { cmd } => invoices(cmd).await,
        Cmd::Assoc { cmd } => associations(cmd).await,
        Cmd::Remind {
            target,
            id,
            at,
            note,
            assignee,
            org,
        } => remind(target, id, at, note, assignee, org).await,
        Cmd::Reminders { cmd } => reminders(cmd).await,
    }
}

/// Resolve the org id for user-authenticated calls. The user session
/// doesn't persist an active org, so this is `--org` flag → `SMOOAI_ORG_ID`.
pub fn resolve_org(override_org: Option<String>) -> Result<String> {
    if let Some(o) = override_org.filter(|s| !s.trim().is_empty()) {
        return Ok(o);
    }
    if let Ok(o) = std::env::var("SMOOAI_ORG_ID") {
        if !o.trim().is_empty() {
            return Ok(o);
        }
    }
    anyhow::bail!("no org specified — pass `--org <id>` or set SMOOAI_ORG_ID")
}

async fn contacts(cmd: ContactsCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        ContactsCmd::List { org, search, limit } => {
            let org = resolve_org(org)?;
            let mut path = format!("/organizations/{org}/crm/contacts?limit={limit}");
            if let Some(s) = search.filter(|s| !s.trim().is_empty()) {
                path.push_str(&format!("&search={}", urlencoding::encode(&s)));
            }
            print_json(&client.get(&path).await.context("GET contacts")?);
        }
        ContactsCmd::Get { contact_id, org } => {
            let org = resolve_org(org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{org}/crm/contacts/{contact_id}"))
                    .await
                    .context("GET contact")?,
            );
        }
        ContactsCmd::Create { body, org } => {
            let org = resolve_org(org)?;
            let body = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{org}/crm/contacts"), &body)
                    .await
                    .context("POST contact")?,
            );
        }
        ContactsCmd::Update { contact_id, body, org } => {
            let org = resolve_org(org)?;
            let body = read_body(&body)?;
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/crm/contacts/{contact_id}"), &body)
                    .await
                    .context("PATCH contact")?,
            );
        }
        ContactsCmd::Import { file, org, dry_run, rate_ms } => {
            let org = resolve_org(org)?;
            import(&client, &org, &file, dry_run, rate_ms).await?;
        }
        ContactsCmd::SetImage { contact_id, path, org } => {
            let org = resolve_org(org)?;
            set_image(&client, &org, ImageEntity::Contact, &contact_id, &path).await?;
        }
        ContactsCmd::RemoveImage { contact_id, org } => {
            let org = resolve_org(org)?;
            remove_image(&client, &org, ImageEntity::Contact, &contact_id).await?;
        }
        ContactsCmd::SetCompany { contact, company, org } => {
            let org = resolve_org(org)?;
            let contact_id = resolve_contact_id(&client, &org, &contact).await?;
            let body = if is_clear_token(&company) {
                json!({ "companyId": Value::Null })
            } else {
                json!({ "companyId": resolve_company_id(&client, &org, &company).await? })
            };
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/crm/contacts/{contact_id}"), &body)
                    .await
                    .context("PATCH contact company")?,
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Companies
// ---------------------------------------------------------------------------

async fn companies(cmd: CompaniesCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        CompaniesCmd::List { org, search, limit, json } => {
            let org = resolve_org(org)?;
            let mut path = format!("/organizations/{org}/crm/companies?limit={limit}");
            if let Some(s) = search.filter(|s| !s.trim().is_empty()) {
                path.push_str(&format!("&search={}", urlencoding::encode(&s)));
            }
            let body = client.get(&path).await.context("GET companies")?;
            if json {
                print_json(&body);
            } else {
                render_companies(&body);
            }
        }
        CompaniesCmd::Show { company_id, org } => {
            let org = resolve_org(org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{org}/crm/companies/{company_id}"))
                    .await
                    .context("GET company")?,
            );
        }
        CompaniesCmd::Upsert {
            name,
            org,
            domain,
            industry,
            website,
        } => {
            let org = resolve_org(org)?;
            upsert_company(&client, &org, &name, domain, industry, website).await?;
        }
        CompaniesCmd::SetImage { company_id, path, org } => {
            let org = resolve_org(org)?;
            set_image(&client, &org, ImageEntity::Company, &company_id, &path).await?;
        }
        CompaniesCmd::ResolveLogo { company_id, org } => {
            let org = resolve_org(org)?;
            resolve_logo(&client, &org, &company_id).await?;
        }
        CompaniesCmd::RemoveImage { company_id, org } => {
            let org = resolve_org(org)?;
            remove_image(&client, &org, ImageEntity::Company, &company_id).await?;
        }
        CompaniesCmd::SetParent { company, parent, org } => {
            let org = resolve_org(org)?;
            let company_id = resolve_company_id(&client, &org, &company).await?;
            if is_clear_token(&parent) {
                clear_company_parent(&client, &org, &company_id).await?;
            } else {
                let parent_id = resolve_company_id(&client, &org, &parent).await?;
                let body = json!({
                    "sourceType": "company",
                    "sourceId": company_id,
                    "targetType": "company",
                    "targetId": parent_id,
                    "associationType": "parent",
                });
                let row = client
                    .post(&format!("/organizations/{org}/crm/associations"), &body)
                    .await
                    .context("POST parent association")?;
                print_json(&row);
            }
        }
    }
    Ok(())
}

/// Find an existing company by case-insensitive name, or by domain.
async fn find_company(client: &UserClient, org: &str, name: &str, domain: Option<&str>) -> Result<Option<Value>> {
    let list = client
        .get(&format!("/organizations/{org}/crm/companies?limit=200"))
        .await
        .context("GET companies for match")?;
    let name_l = name.trim().to_lowercase();
    let dom_l = domain.map(|d| d.trim().to_lowercase()).filter(|d| !d.is_empty());
    Ok(list.as_array().and_then(|arr| {
        arr.iter()
            .find(|c| {
                let cn = c.get("name").and_then(Value::as_str).unwrap_or_default().trim().to_lowercase();
                let cd = c.get("domain").and_then(Value::as_str).map(|s| s.trim().to_lowercase());
                cn == name_l || (dom_l.is_some() && cd == dom_l)
            })
            .cloned()
    }))
}

async fn upsert_company(client: &UserClient, org: &str, name: &str, domain: Option<String>, industry: Option<String>, website: Option<String>) -> Result<()> {
    let mut body = json!({ "name": name });
    if let Some(d) = domain.filter(|s| !s.trim().is_empty()) {
        body["domain"] = json!(d);
    }
    if let Some(i) = industry.filter(|s| !s.trim().is_empty()) {
        body["industry"] = json!(i);
    }
    if let Some(w) = website.filter(|s| !s.trim().is_empty()) {
        body["website"] = json!(w);
    }

    let existing = find_company(client, org, name, body.get("domain").and_then(Value::as_str)).await?;
    if let Some(c) = existing {
        let id = c.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        client
            .patch(&format!("/organizations/{org}/crm/companies/{id}"), &body)
            .await
            .context("PATCH company")?;
        println!("  {} updated company {} {}", "↻".yellow(), id.dimmed(), name.bold());
    } else {
        let r = client
            .post(&format!("/organizations/{org}/crm/companies"), &body)
            .await
            .context("POST company")?;
        let id = r.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
        println!("  {} created company {} {}", "✚".green(), id.dimmed(), name.bold());
    }
    Ok(())
}

fn render_companies(body: &Value) {
    let cos = body.as_array().cloned().unwrap_or_default();
    let count = format!("({})", cos.len());
    println!();
    println!("  {} {}", "Companies".bold(), count.dimmed());
    if cos.is_empty() {
        println!("\n  {}\n", "none".dimmed());
        return;
    }
    let (h_name, h_dom, h_ind) = (format!("{:<28}", "NAME"), format!("{:<26}", "DOMAIN"), format!("{:<20}", "INDUSTRY"));
    println!();
    println!("  {}  {}  {}", h_name.dimmed(), h_dom.dimmed(), h_ind.dimmed());
    for c in &cos {
        println!(
            "  {:<28}  {:<26}  {:<20}",
            truncate(c.get("name").and_then(Value::as_str).unwrap_or("—"), 28),
            truncate(c.get("domain").and_then(Value::as_str).unwrap_or("—"), 26),
            truncate(c.get("industry").and_then(Value::as_str).unwrap_or("—"), 20),
        );
    }
    println!();
}

// ---------------------------------------------------------------------------
// Deals (pipeline)
// ---------------------------------------------------------------------------

async fn deals(cmd: DealsCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        DealsCmd::List { org, stage, limit, json } => {
            let org = resolve_org(org)?;
            let mut path = format!("/organizations/{org}/crm/deals?limit={limit}");
            if let Some(s) = stage.filter(|s| !s.trim().is_empty()) {
                path.push_str(&format!("&stage={}", urlencoding::encode(&s)));
            }
            let body = client.get(&path).await.context("GET deals")?;
            if json {
                print_json(&body);
            } else {
                render_deals(&body);
            }
        }
        DealsCmd::Show { deal_id, org } => {
            let org = resolve_org(org)?;
            show_deal(&client, &org, &deal_id).await?;
        }
        DealsCmd::Create {
            title,
            org,
            value,
            stage,
            company,
            contact,
            close_date,
        } => {
            let org = resolve_org(org)?;
            create_deal(&client, &org, &title, value, stage, company, contact, close_date).await?;
        }
        DealsCmd::Move { deal_id, stage, org } => {
            let org = resolve_org(org)?;
            client
                .patch(&format!("/organizations/{org}/crm/deals/{deal_id}"), &json!({ "stage": stage }))
                .await
                .context("PATCH deal stage")?;
            println!("  {} moved deal {} → {}", "↻".yellow(), deal_id.dimmed(), stage_color(&stage));
        }
        DealsCmd::SetImage { deal_id, path, org } => {
            let org = resolve_org(org)?;
            set_image(&client, &org, ImageEntity::Deal, &deal_id, &path).await?;
        }
        DealsCmd::RemoveImage { deal_id, org } => {
            let org = resolve_org(org)?;
            remove_image(&client, &org, ImageEntity::Deal, &deal_id).await?;
        }
        DealsCmd::SetContact { deal, contact, org } => {
            let org = resolve_org(org)?;
            let deal_id = resolve_deal_id(&client, &org, &deal).await?;
            let body = if is_clear_token(&contact) {
                json!({ "contactId": Value::Null })
            } else {
                json!({ "contactId": resolve_contact_id(&client, &org, &contact).await? })
            };
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/crm/deals/{deal_id}"), &body)
                    .await
                    .context("PATCH deal contact")?,
            );
        }
        DealsCmd::SetCompany { deal, company, org } => {
            let org = resolve_org(org)?;
            let deal_id = resolve_deal_id(&client, &org, &deal).await?;
            let body = if is_clear_token(&company) {
                json!({ "companyId": Value::Null })
            } else {
                json!({ "companyId": resolve_company_id(&client, &org, &company).await? })
            };
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/crm/deals/{deal_id}"), &body)
                    .await
                    .context("PATCH deal company")?,
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Images (contact avatar / company logo / deal image) — SMOODEV-2605.
//
// One polymorphic `crm_images` table backs all three (SMOODEV-2589). Uploads
// mirror `th files upload`: presign via `/media/upload-url`, PUT the bytes to
// S3 with a bearer-less client, then link the DURABLE `mediaUrl` under
// `/crm/images`. Removal needs the image UUID, which the client does not hold —
// it reads the id off the entity's own read payload.
// ---------------------------------------------------------------------------

/// The three CRM entities that can carry an image, and the wire vocabulary each
/// uses across the API.
#[derive(Clone, Copy)]
enum ImageEntity {
    Contact,
    Company,
    Deal,
}

impl ImageEntity {
    /// `entityType` value in the `POST /crm/images` body.
    fn wire(self) -> &'static str {
        match self {
            Self::Contact => "contact",
            Self::Company => "company",
            Self::Deal => "deal",
        }
    }

    /// URL path segment for the entity's read endpoint (`/crm/<seg>/<id>`).
    fn seg(self) -> &'static str {
        match self {
            Self::Contact => "contacts",
            Self::Company => "companies",
            Self::Deal => "deals",
        }
    }

    /// Field on the entity read payload holding its OWN primary image id — what
    /// `DELETE /crm/images/{id}` needs. Null when the entity has no own image
    /// (for a deal, when the displayed picture is inherited from company/contact).
    fn image_id_field(self) -> &'static str {
        match self {
            Self::Contact => "avatarImageId",
            Self::Company => "logoImageId",
            Self::Deal => "imageId",
        }
    }
}

/// Map an image file name to the MIME type `/media/upload-url` accepts, or bail
/// naming the allowed types. That endpoint only takes png/jpeg/jpg/gif/webp;
/// SVG and everything else is rejected (an SVG served back is a script vector).
fn image_content_type(name: &str) -> Result<&'static str> {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Ok("image/png"),
        "jpg" | "jpeg" => Ok("image/jpeg"),
        "gif" => Ok("image/gif"),
        "webp" => Ok("image/webp"),
        other => anyhow::bail!(
            "unsupported image type {:?} — allowed: png, jpg, jpeg, gif, webp",
            if other.is_empty() { name } else { other }
        ),
    }
}

/// The entity's OWN primary image id from its read payload, or None when it has
/// none (field absent or JSON null). For a deal, None ⇒ the displayed image is
/// inherited and there is nothing this entity can delete.
fn own_image_id(entity: ImageEntity, read: &Value) -> Option<String> {
    read.get(entity.image_id_field()).and_then(Value::as_str).map(str::to_string)
}

/// Upload a local image (presigned PUT) and link it as the entity's primary
/// image. Prints the created `crm_images` row.
async fn set_image(client: &UserClient, org: &str, entity: ImageEntity, entity_id: &str, path: &str) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("read {path}"))?;
    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("cannot derive a file name from {path}"))?;
    let content_type = image_content_type(file_name)?;
    let size = bytes.len();

    // 1. Presigned S3 PUT URL + the durable, non-expiring object URL we persist.
    let upload_req = json!({ "fileName": file_name, "contentType": content_type, "fileSizeBytes": size });
    let resp = client
        .post(&format!("/organizations/{org}/media/upload-url"), &upload_req)
        .await
        .context("POST media/upload-url")?;
    let upload_url = resp
        .get("uploadUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("upload-url response missing uploadUrl"))?;
    let media_url = resp
        .get("mediaUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("upload-url response missing mediaUrl"))?
        .to_string();

    // 2. PUT the bytes with a bearer-less client — the presigned URL is
    //    self-authorizing; an extra Authorization header makes S3 400.
    let put = reqwest::Client::new()
        .put(upload_url)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(bytes)
        .send()
        .await
        .context("PUT to presigned URL")?;
    if !put.status().is_success() {
        let status = put.status();
        let text = put.text().await.unwrap_or_default();
        anyhow::bail!("upload PUT failed: HTTP {status}: {text}");
    }

    // 3. Link the durable URL as the entity's primary image.
    let body = json!({
        "entityType": entity.wire(),
        "entityId": entity_id,
        "url": media_url,
        "mimeType": content_type,
        "size": size,
    });
    print_json(
        &client
            .post(&format!("/organizations/{org}/crm/images"), &body)
            .await
            .context("POST crm/images")?,
    );
    Ok(())
}

/// Remove the entity's own image. Resolves the image UUID from the entity read
/// (there is no other way to get it), then DELETEs by that id. When the entity
/// has no own image it exits 0 with a clear message rather than erroring.
async fn remove_image(client: &UserClient, org: &str, entity: ImageEntity, entity_id: &str) -> Result<()> {
    let read = client
        .get(&format!("/organizations/{org}/crm/{}/{entity_id}", entity.seg()))
        .await
        .with_context(|| format!("GET {}", entity.seg()))?;
    let Some(image_id) = own_image_id(entity, &read) else {
        let msg = match entity {
            ImageEntity::Deal => {
                "deal has no image of its own — its displayed image is inherited from the linked company/contact, so there is nothing to remove"
            }
            _ => "no image set — nothing to remove",
        };
        println!();
        println!("  {} {}", "●".dimmed(), msg.dimmed());
        println!();
        return Ok(());
    };
    print_json(
        &client
            .delete(&format!("/organizations/{org}/crm/images/{image_id}"))
            .await
            .context("DELETE crm/images")?,
    );
    Ok(())
}

/// Resolve a company's logo server-side from its own domain (no local file).
/// Prints the durable URL, or a clear message when nothing was resolved.
async fn resolve_logo(client: &UserClient, org: &str, company_id: &str) -> Result<()> {
    let resp = client
        .post(&format!("/organizations/{org}/crm/companies/{company_id}/resolve-logo"), &json!({}))
        .await
        .context("POST resolve-logo")?;
    println!();
    match resp.get("logoUrl").and_then(Value::as_str) {
        Some(url) => println!("  {} {}", "Logo resolved →".dimmed(), url.cyan().bold()),
        None => println!(
            "  {} {}",
            "●".dimmed(),
            "no logo resolved — the company has no domain, or nothing was found".dimmed()
        ),
    }
    println!();
    Ok(())
}

async fn find_deal(client: &UserClient, org: &str, title: &str) -> Result<Option<Value>> {
    let list = client
        .get(&format!("/organizations/{org}/crm/deals?limit=200"))
        .await
        .context("GET deals for match")?;
    let t = title.trim().to_lowercase();
    Ok(list.as_array().and_then(|arr| {
        arr.iter()
            .find(|d| d.get("title").and_then(Value::as_str).unwrap_or_default().trim().to_lowercase() == t)
            .cloned()
    }))
}

#[allow(clippy::too_many_arguments)]
async fn create_deal(
    client: &UserClient,
    org: &str,
    title: &str,
    value: Option<f64>,
    stage: Option<String>,
    company: Option<String>,
    contact: Option<String>,
    close_date: Option<String>,
) -> Result<()> {
    if let Some(existing) = find_deal(client, org, title).await? {
        let id = existing.get("id").and_then(Value::as_str).unwrap_or("?");
        println!(
            "  {} deal {} already exists {} — left as-is (use `deals move` to change stage)",
            "•".cyan(),
            id.dimmed(),
            title.bold()
        );
        return Ok(());
    }
    let stage = stage.filter(|s| !s.trim().is_empty());
    let mut body = json!({ "title": title });
    if let Some(v) = value {
        body["value"] = json!(v);
    }
    if let Some(s) = &stage {
        body["stage"] = json!(s);
    }
    if let Some(c) = company.filter(|s| !s.trim().is_empty()) {
        body["companyId"] = json!(resolve_company_id(client, org, &c).await?);
    }
    if let Some(c) = contact.filter(|s| !s.trim().is_empty()) {
        body["contactId"] = json!(resolve_contact_id(client, org, &c).await?);
    }
    if let Some(d) = close_date.filter(|s| !s.trim().is_empty()) {
        body["closeDate"] = json!(d);
    }
    let r = client.post(&format!("/organizations/{org}/crm/deals"), &body).await.context("POST deal")?;
    let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
    println!(
        "  {} created deal {} {}  {}  {}",
        "✚".green(),
        id.dimmed(),
        title.bold(),
        stage_color(stage.as_deref().unwrap_or("")),
        fmt_money(value),
    );
    Ok(())
}

/// Resolve a company id from a uuid (used as-is) or a name (looked up).
async fn resolve_company_id(client: &UserClient, org: &str, s: &str) -> Result<String> {
    if looks_like_uuid(s) {
        return Ok(s.to_string());
    }
    match find_company(client, org, s, None).await? {
        Some(c) => c.get("id").and_then(Value::as_str).map(str::to_string).context("matched company has no id"),
        None => anyhow::bail!("no company matches '{s}' — create it first: `th api crm companies upsert \"{s}\"`"),
    }
}

/// Resolve a contact id from a uuid (used as-is) or an email/name (looked up).
async fn resolve_contact_id(client: &UserClient, org: &str, s: &str) -> Result<String> {
    if looks_like_uuid(s) {
        return Ok(s.to_string());
    }
    // Match locally against every contact — the `?search=` param doesn't
    // reliably hit a full email address, so reuse the same paged fetch as
    // `import` and compare exact email / "first last" here.
    let contacts = fetch_all(client, org, std::time::Duration::from_millis(0)).await?;
    let sl = s.trim().to_lowercase();
    let found = contacts.iter().find(|c| {
        let email = c.get("email").and_then(Value::as_str).unwrap_or_default().to_lowercase();
        let name = format!(
            "{} {}",
            c.get("firstName").and_then(Value::as_str).unwrap_or(""),
            c.get("lastName").and_then(Value::as_str).unwrap_or("")
        )
        .trim()
        .to_lowercase();
        email == sl || name == sl
    });
    match found {
        Some(c) => c.get("id").and_then(Value::as_str).map(str::to_string).context("matched contact has no id"),
        None => anyhow::bail!("no contact matches '{s}' — create it first: `th api crm contacts create`"),
    }
}

/// Resolve a deal id from a uuid (used as-is) or a title (looked up).
async fn resolve_deal_id(client: &UserClient, org: &str, s: &str) -> Result<String> {
    if looks_like_uuid(s) {
        return Ok(s.to_string());
    }
    match find_deal(client, org, s).await? {
        Some(d) => d.get("id").and_then(Value::as_str).map(str::to_string).context("matched deal has no id"),
        None => anyhow::bail!("no deal matches '{s}' — create it first: `th api crm deals create \"{s}\"`"),
    }
}

async fn show_deal(client: &UserClient, org: &str, deal_id: &str) -> Result<()> {
    let d = client.get(&format!("/organizations/{org}/crm/deals/{deal_id}")).await.context("GET deal")?;
    println!();
    println!("  {}", d.get("title").and_then(Value::as_str).unwrap_or("—").bold());
    println!("  {} {}", "Stage  ".dimmed(), stage_color(d.get("stage").and_then(Value::as_str).unwrap_or("")));
    println!("  {} {}", "Value  ".dimmed(), fmt_money(as_money(d.get("value"))).bold());
    println!("  {} {}", "Close  ".dimmed(), short_date(d.get("closeDate")));
    if let Some(cid) = d.get("companyId").and_then(Value::as_str) {
        let name = client
            .get(&format!("/organizations/{org}/crm/companies/{cid}"))
            .await
            .ok()
            .and_then(|c| c.get("name").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| cid.to_string());
        println!("  {} {}", "Company".dimmed(), name);
    }
    if let Some(cid) = d.get("contactId").and_then(Value::as_str) {
        let label = client
            .get(&format!("/organizations/{org}/crm/contacts/{cid}"))
            .await
            .ok()
            .map(|c| contact_label(&c))
            .unwrap_or_else(|| cid.to_string());
        println!("  {} {}", "Contact".dimmed(), label);
    }
    println!("  {} {}", "id     ".dimmed(), deal_id.dimmed());
    // The deal's merged timeline, when the endpoint is available.
    if let Ok(tl) = client.get(&format!("/organizations/{org}/crm/deals/{deal_id}/timeline")).await {
        render_timeline(&tl, false);
    } else {
        println!();
    }
    Ok(())
}

fn contact_label(c: &Value) -> String {
    let name = format!(
        "{} {}",
        c.get("firstName").and_then(Value::as_str).unwrap_or(""),
        c.get("lastName").and_then(Value::as_str).unwrap_or("")
    )
    .trim()
    .to_string();
    let email = c.get("email").and_then(Value::as_str).unwrap_or("");
    match (name.is_empty(), email.is_empty()) {
        (false, false) => format!("{name} <{email}>"),
        (false, true) => name,
        _ => email.to_string(),
    }
}

fn render_deals(body: &Value) {
    let deals = body.as_array().cloned().unwrap_or_default();
    let pipeline: f64 = deals.iter().filter_map(|d| as_money(d.get("value"))).sum();
    println!();
    println!("  {}", "Deals".bold());
    println!(
        "  {} {}     {} {}",
        "Total".dimmed(),
        deals.len().to_string().bold(),
        "Pipeline".dimmed(),
        fmt_money(Some(pipeline)).bold()
    );
    if deals.is_empty() {
        println!("\n  {}\n", "no deals yet".dimmed());
        return;
    }
    let (h_title, h_stage, h_value, h_close) = (
        format!("{:<38}", "TITLE"),
        format!("{:<14}", "STAGE"),
        format!("{:>12}", "VALUE"),
        format!("{:<10}", "CLOSE"),
    );
    println!();
    println!("  {}  {}  {}  {}", h_title.dimmed(), h_stage.dimmed(), h_value.dimmed(), h_close.dimmed());
    for d in &deals {
        let title = truncate(d.get("title").and_then(Value::as_str).unwrap_or("—"), 38);
        let value = format!("{:>12}", fmt_money(as_money(d.get("value"))));
        println!(
            "  {:<38}  {}  {}  {}",
            title,
            stage_cell(d.get("stage").and_then(Value::as_str).unwrap_or(""), 14),
            value,
            short_date(d.get("closeDate")),
        );
    }
    println!();
}

// ---------------------------------------------------------------------------
// Pipeline (forecast board)
// ---------------------------------------------------------------------------

async fn pipeline(org: Option<String>, json: bool) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    let org = resolve_org(org)?;
    let body = client
        .get(&format!("/organizations/{org}/crm/deals/forecast"))
        .await
        .context("GET deal forecast")?;
    if json {
        print_json(&body);
    } else {
        render_pipeline(&body);
    }
    Ok(())
}

fn render_pipeline(body: &Value) {
    let mut stages = body.get("stages").and_then(Value::as_array).cloned().unwrap_or_default();
    stages.sort_by_key(|s| s.get("position").and_then(Value::as_i64).unwrap_or(i64::MAX));
    let totals = body.get("totals").cloned().unwrap_or(Value::Null);

    println!();
    println!("  {}", "Pipeline".bold());
    if stages.is_empty() {
        println!("\n  {}\n", "no stages — seed defaults with `th api crm stages init`".dimmed());
        return;
    }

    let (h_stage, h_open, h_val, h_wt, h_prob) = (
        format!("{:<16}", "STAGE"),
        format!("{:>5}", "OPEN"),
        format!("{:>13}", "OPEN VALUE"),
        format!("{:>13}", "WEIGHTED"),
        format!("{:<14}", "PROB"),
    );
    println!();
    println!(
        "  {}  {}  {}  {}  {}",
        h_stage.dimmed(),
        h_open.dimmed(),
        h_val.dimmed(),
        h_wt.dimmed(),
        h_prob.dimmed()
    );
    for s in &stages {
        let name = s.get("stage").and_then(Value::as_str).unwrap_or("—");
        let open = s.get("openCount").and_then(Value::as_i64).unwrap_or(0);
        let openv = fmt_money(as_money(s.get("openValue")));
        let wt = fmt_money(as_money(s.get("weightedValue")));
        let prob = as_money(s.get("probability")).unwrap_or(0.0);
        let open_cell = format!("{open:>5}");
        let openv_cell = format!("{openv:>13}");
        let wt_cell = format!("{wt:>13}");
        let prob_cell = prob_bar(prob, 10);
        println!("  {}  {}  {}  {}  {}", stage_cell(name, 16), open_cell, openv_cell, wt_cell.bold(), prob_cell,);
    }

    // Totals line — the number that matters.
    let open_value = fmt_money(as_money(totals.get("openValue")));
    let weighted = fmt_money(as_money(totals.get("weightedValue")));
    let won_value = fmt_money(as_money(totals.get("wonValue")));
    let won_count = totals.get("wonCount").and_then(Value::as_i64).unwrap_or(0);
    println!();
    let open_lbl = format!("Open {}", open_value);
    let fc_lbl = format!("Forecast {}", weighted);
    let won_lbl = format!("Won {} ({})", won_value, won_count);
    println!("  {}   {}   {}", open_lbl.dimmed(), fc_lbl.green().bold(), won_lbl.cyan());

    // By-source mini table.
    if let Some(sources) = body.get("bySource").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        println!();
        println!("  {}", "By source".dimmed());
        for src in sources {
            let name = src.get("source").and_then(Value::as_str).unwrap_or("—");
            let count = src.get("count").and_then(Value::as_i64).unwrap_or(0);
            let value = fmt_money(as_money(src.get("value")));
            let name_cell = format!("{:<20}", truncate(name, 20));
            let count_cell = format!("{count:>4}");
            let value_cell = format!("{value:>13}");
            println!("    {}  {}  {}", name_cell, count_cell.dimmed(), value_cell);
        }
    }
    println!();
}

// ---------------------------------------------------------------------------
// Stages
// ---------------------------------------------------------------------------

async fn stages(cmd: StagesCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        StagesCmd::List { org, json } => {
            let org = resolve_org(org)?;
            let body = client.get(&format!("/organizations/{org}/crm/stages")).await.context("GET stages")?;
            if json {
                print_json(&body);
            } else {
                render_stages(&body);
            }
        }
        StagesCmd::Show { stage_id, org } => {
            let org = resolve_org(org)?;
            print_json(&client.get(&format!("/organizations/{org}/crm/stages/{stage_id}")).await.context("GET stage")?);
        }
        StagesCmd::Create {
            name,
            org,
            position,
            probability,
            won,
            lost,
            color,
        } => {
            let org = resolve_org(org)?;
            let mut b = json!({ "name": name });
            stage_fields(&mut b, position, probability, won, lost, color);
            let r = client.post(&format!("/organizations/{org}/crm/stages"), &b).await.context("POST stage")?;
            let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
            println!("  {} created stage {} {}", "✚".green(), id.dimmed(), name.bold());
        }
        StagesCmd::Update {
            stage_id,
            org,
            name,
            position,
            probability,
            won,
            lost,
            color,
        } => {
            let org = resolve_org(org)?;
            let mut b = json!({});
            if let Some(n) = name.filter(|s| !s.trim().is_empty()) {
                b["name"] = json!(n);
            }
            stage_fields(&mut b, position, probability, won, lost, color);
            client
                .patch(&format!("/organizations/{org}/crm/stages/{stage_id}"), &b)
                .await
                .context("PATCH stage")?;
            println!("  {} updated stage {}", "↻".yellow(), stage_id.dimmed());
        }
        StagesCmd::Delete { stage_id, org } => {
            let org = resolve_org(org)?;
            client
                .delete(&format!("/organizations/{org}/crm/stages/{stage_id}"))
                .await
                .context("DELETE stage")?;
            println!("  {} deleted stage {}", "✗".red(), stage_id.dimmed());
        }
        StagesCmd::Reorder { ids, org } => {
            let org = resolve_org(org)?;
            client
                .post(&format!("/organizations/{org}/crm/stages/reorder"), &json!({ "orderedIds": ids }))
                .await
                .context("POST stages reorder")?;
            let n = ids.len();
            println!("  {} reordered {} stages", "↻".yellow(), n.to_string().bold());
        }
        StagesCmd::Init { org } => {
            let org = resolve_org(org)?;
            client
                .post(&format!("/organizations/{org}/crm/stages/ensure-defaults"), &json!({}))
                .await
                .context("POST ensure-defaults")?;
            println!("  {} default pipeline stages ensured", "✓".green().bold());
        }
    }
    Ok(())
}

/// Fold the shared stage flags/options into a JSON body. `won`/`lost` are
/// only set when the flag is present (a bare `false` isn't meaningful on update).
fn stage_fields(b: &mut Value, position: Option<i64>, probability: Option<f64>, won: bool, lost: bool, color: Option<String>) {
    if let Some(p) = position {
        b["position"] = json!(p);
    }
    if let Some(p) = probability {
        b["probability"] = json!(p);
    }
    if won {
        b["isWon"] = json!(true);
    }
    if lost {
        b["isLost"] = json!(true);
    }
    if let Some(c) = color.filter(|s| !s.trim().is_empty()) {
        b["color"] = json!(c);
    }
}

fn render_stages(body: &Value) {
    let mut stages = body.as_array().cloned().unwrap_or_default();
    stages.sort_by_key(|s| s.get("position").and_then(Value::as_i64).unwrap_or(i64::MAX));
    let count = format!("({})", stages.len());
    println!();
    println!("  {} {}", "Stages".bold(), count.dimmed());
    if stages.is_empty() {
        println!("\n  {}\n", "none — seed defaults with `th api crm stages init`".dimmed());
        return;
    }
    let (h_pos, h_name, h_prob, h_won, h_lost) = (
        format!("{:>3}", "#"),
        format!("{:<22}", "NAME"),
        format!("{:>5}", "PROB"),
        format!("{:<4}", "WON"),
        format!("{:<4}", "LOST"),
    );
    println!();
    println!(
        "  {}  {}  {}  {}  {}",
        h_pos.dimmed(),
        h_name.dimmed(),
        h_prob.dimmed(),
        h_won.dimmed(),
        h_lost.dimmed()
    );
    for s in &stages {
        let pos = s.get("position").and_then(Value::as_i64).unwrap_or(0);
        let prob = as_money(s.get("probability")).unwrap_or(0.0);
        let name = s.get("stage").or_else(|| s.get("name")).and_then(Value::as_str).unwrap_or("—");
        let pos_cell = format!("{pos:>3}");
        let prob_cell = format!("{:>4}%", prob.round() as i64);
        let won_cell = yes_no(s.get("isWon").and_then(Value::as_bool).unwrap_or(false));
        let lost_cell = yes_no(s.get("isLost").and_then(Value::as_bool).unwrap_or(false));
        println!(
            "  {}  {}  {}  {:<4}  {:<4}",
            pos_cell.dimmed(),
            stage_cell(name, 22),
            prob_cell,
            won_cell,
            lost_cell,
        );
    }
    println!();
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

async fn tasks(cmd: TasksCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        TasksCmd::List {
            org,
            deal,
            contact,
            assignee,
            overdue,
            all,
            json,
        } => {
            let org = resolve_org(org)?;
            let mut path = format!("/organizations/{org}/crm/tasks?");
            let mut params: Vec<String> = Vec::new();
            if let Some(d) = deal.filter(|s| !s.trim().is_empty()) {
                params.push(format!("dealId={}", resolve_deal_id(&client, &org, &d).await?));
            }
            if let Some(c) = contact.filter(|s| !s.trim().is_empty()) {
                params.push(format!("contactId={}", resolve_contact_id(&client, &org, &c).await?));
            }
            if let Some(a) = assignee.filter(|s| !s.trim().is_empty()) {
                params.push(format!("assigneeUserId={a}"));
            }
            if overdue {
                params.push("overdue=true".into());
            }
            if all {
                params.push("includeCompleted=true".into());
            }
            path.push_str(&params.join("&"));
            let body = client.get(&path).await.context("GET tasks")?;
            if json {
                print_json(&body);
            } else {
                render_tasks(&body);
            }
        }
        TasksCmd::Show { task_id, org } => {
            let org = resolve_org(org)?;
            print_json(&client.get(&format!("/organizations/{org}/crm/tasks/{task_id}")).await.context("GET task")?);
        }
        TasksCmd::Add {
            title,
            org,
            deal,
            contact,
            company,
            due,
            description,
            assignee,
        } => {
            let org = resolve_org(org)?;
            let mut b = json!({ "title": title });
            if let Some(d) = deal.filter(|s| !s.trim().is_empty()) {
                b["dealId"] = json!(resolve_deal_id(&client, &org, &d).await?);
            }
            if let Some(c) = contact.filter(|s| !s.trim().is_empty()) {
                b["contactId"] = json!(resolve_contact_id(&client, &org, &c).await?);
            }
            if let Some(c) = company.filter(|s| !s.trim().is_empty()) {
                b["companyId"] = json!(resolve_company_id(&client, &org, &c).await?);
            }
            if let Some(d) = due.filter(|s| !s.trim().is_empty()) {
                b["dueAt"] = json!(d);
            }
            if let Some(d) = description.filter(|s| !s.trim().is_empty()) {
                b["description"] = json!(d);
            }
            if let Some(a) = assignee.filter(|s| !s.trim().is_empty()) {
                b["assigneeUserId"] = json!(a);
            }
            let r = client.post(&format!("/organizations/{org}/crm/tasks"), &b).await.context("POST task")?;
            let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
            println!("  {} added task {} {}", "✚".green(), id.dimmed(), title.bold());
        }
        TasksCmd::Update {
            task_id,
            org,
            title,
            due,
            description,
            assignee,
        } => {
            let org = resolve_org(org)?;
            let mut b = json!({});
            if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
                b["title"] = json!(t);
            }
            if let Some(d) = due.filter(|s| !s.trim().is_empty()) {
                b["dueAt"] = json!(d);
            }
            if let Some(d) = description.filter(|s| !s.trim().is_empty()) {
                b["description"] = json!(d);
            }
            if let Some(a) = assignee.filter(|s| !s.trim().is_empty()) {
                b["assigneeUserId"] = json!(a);
            }
            client
                .patch(&format!("/organizations/{org}/crm/tasks/{task_id}"), &b)
                .await
                .context("PATCH task")?;
            println!("  {} updated task {}", "↻".yellow(), task_id.dimmed());
        }
        TasksCmd::Done { task_id, org } => {
            let org = resolve_org(org)?;
            client
                .post(&format!("/organizations/{org}/crm/tasks/{task_id}/complete"), &json!({}))
                .await
                .context("POST task complete")?;
            println!("  {} completed task {}", "✓".green().bold(), task_id.dimmed());
        }
        TasksCmd::Rm { task_id, org } => {
            let org = resolve_org(org)?;
            client
                .delete(&format!("/organizations/{org}/crm/tasks/{task_id}"))
                .await
                .context("DELETE task")?;
            println!("  {} deleted task {}", "✗".red(), task_id.dimmed());
        }
    }
    Ok(())
}

fn render_tasks(body: &Value) {
    let items = body.as_array().cloned().unwrap_or_default();
    let count = format!("({})", items.len());
    println!();
    println!("  {} {}", "Tasks".bold(), count.dimmed());
    if items.is_empty() {
        println!("\n  {}\n", "no tasks".dimmed());
        return;
    }
    let (h_due, h_title, h_link, h_flag) = (
        format!("{:<10}", "DUE"),
        format!("{:<38}", "TITLE"),
        format!("{:<24}", "LINKED"),
        format!("{:<8}", ""),
    );
    println!();
    println!("  {}  {}  {}  {}", h_due.dimmed(), h_title.dimmed(), h_link.dimmed(), h_flag.dimmed());
    for t in &items {
        let due = format!("{:<10}", short_date(t.get("dueAt")));
        let title = truncate(t.get("title").and_then(Value::as_str).unwrap_or("—"), 38);
        let done = t.get("completedAt").map(|v| !v.is_null()).unwrap_or(false) || t.get("isCompleted").and_then(Value::as_bool).unwrap_or(false);
        let link = task_link(t);
        let link_cell = format!("{:<24}", truncate(&link, 24));
        let flag = if done {
            "DONE".green().to_string()
        } else if is_overdue(t.get("dueAt")) {
            "OVERDUE".red().bold().to_string()
        } else {
            String::new()
        };
        println!("  {}  {:<38}  {}  {}", due.dimmed(), title, link_cell, flag);
    }
    println!();
}

/// A short "linked to" label for a task: deal title, else contact, else company.
fn task_link(t: &Value) -> String {
    for key in ["dealTitle", "deal", "contactName", "contact", "companyName", "company"] {
        if let Some(s) = t.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            return s.to_string();
        }
    }
    for key in ["dealId", "contactId", "companyId"] {
        if let Some(s) = t.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            return s.chars().take(8).collect::<String>();
        }
    }
    "—".into()
}

// ---------------------------------------------------------------------------
// Conversations
// ---------------------------------------------------------------------------

async fn conversations(cmd: ConversationsCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        ConversationsCmd::List { org, contact, deal, json } => {
            let org = resolve_org(org)?;
            let mut params: Vec<String> = Vec::new();
            if let Some(c) = contact.filter(|s| !s.trim().is_empty()) {
                params.push(format!("contactId={}", resolve_contact_id(&client, &org, &c).await?));
            }
            if let Some(d) = deal.filter(|s| !s.trim().is_empty()) {
                params.push(format!("dealId={d}"));
            }
            let path = format!("/organizations/{org}/crm/conversations?{}", params.join("&"));
            let body = client.get(&path).await.context("GET conversations")?;
            if json {
                print_json(&body);
            } else {
                render_conversations(&body);
            }
        }
        ConversationsCmd::Show { conversation_id, org, json } => {
            let org = resolve_org(org)?;
            let body = client
                .get(&format!("/organizations/{org}/crm/conversations/{conversation_id}"))
                .await
                .context("GET conversation")?;
            if json {
                print_json(&body);
            } else {
                render_thread(&body);
            }
        }
        ConversationsCmd::Create {
            name,
            org,
            platform,
            contact,
            subject,
            body,
        } => {
            let org = resolve_org(org)?;
            let mut b = json!({ "platform": platform, "name": name });
            if let Some(c) = contact.filter(|s| !s.trim().is_empty()) {
                b["contactId"] = json!(resolve_contact_id(&client, &org, &c).await?);
            }
            // A body implies an outbound first message.
            if let Some(body) = body.filter(|s| !s.trim().is_empty()) {
                let mut msg = json!({ "direction": "outbound", "body": body });
                if let Some(s) = subject.filter(|s| !s.trim().is_empty()) {
                    msg["subject"] = json!(s);
                }
                b["firstMessage"] = msg;
            }
            let r = client
                .post(&format!("/organizations/{org}/crm/conversations"), &b)
                .await
                .context("POST conversation")?;
            let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
            println!("  {} started conversation {} {}", "✚".green(), id.dimmed(), name.bold());
        }
        ConversationsCmd::AddEmail {
            conversation_id,
            direction,
            body,
            org,
            subject,
            occurred,
        } => {
            let org = resolve_org(org)?;
            let mut b = json!({ "direction": direction, "body": body });
            if let Some(s) = subject.filter(|s| !s.trim().is_empty()) {
                b["subject"] = json!(s);
            }
            if let Some(o) = occurred.filter(|s| !s.trim().is_empty()) {
                b["occurredAt"] = json!(o);
            }
            client
                .post(&format!("/organizations/{org}/crm/conversations/{conversation_id}/messages"), &b)
                .await
                .context("POST message")?;
            let arrow = if direction == "inbound" {
                "←".cyan().to_string()
            } else {
                "→".green().to_string()
            };
            println!("  {} {} message added to {}", "✚".green(), arrow, conversation_id.dimmed());
        }
    }
    Ok(())
}

fn render_conversations(body: &Value) {
    let items = body.as_array().cloned().unwrap_or_default();
    let count = format!("({})", items.len());
    println!();
    println!("  {} {}", "Conversations".bold(), count.dimmed());
    if items.is_empty() {
        println!("\n  {}\n", "none".dimmed());
        return;
    }
    let (h_date, h_name, h_plat, h_id) = (
        format!("{:<10}", "UPDATED"),
        format!("{:<40}", "NAME"),
        format!("{:<10}", "PLATFORM"),
        format!("{:<10}", "ID"),
    );
    println!();
    println!("  {}  {}  {}  {}", h_date.dimmed(), h_name.dimmed(), h_plat.dimmed(), h_id.dimmed());
    for c in &items {
        let date = format!("{:<10}", short_date(c.get("updatedAt").or_else(|| c.get("createdAt"))));
        let name = truncate(c.get("name").and_then(Value::as_str).unwrap_or("—"), 40);
        let plat = format!("{:<10}", truncate(c.get("platform").and_then(Value::as_str).unwrap_or("—"), 10));
        let id = c.get("id").and_then(Value::as_str).unwrap_or("—").chars().take(8).collect::<String>();
        println!("  {}  {:<40}  {}  {}", date.dimmed(), name, plat.cyan(), id.dimmed());
    }
    println!();
}

fn render_thread(body: &Value) {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("—");
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| body.as_array().cloned())
        .unwrap_or_default();
    println!();
    println!("  {}", name.bold());
    if messages.is_empty() {
        println!("\n  {}\n", "no messages".dimmed());
        return;
    }
    for m in &messages {
        let inbound = m.get("direction").and_then(Value::as_str) == Some("inbound");
        let arrow = if inbound { "←".cyan().to_string() } else { "→".green().to_string() };
        let dir = if inbound { "in ".cyan().to_string() } else { "out".green().to_string() };
        let when = short_date(m.get("occurredAt").or_else(|| m.get("createdAt")));
        let subject = m.get("subject").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
        println!();
        let hdr = match subject {
            Some(s) => format!("{arrow} {dir}  {when}  {}", s.bold()),
            None => format!("{arrow} {dir}  {when}"),
        };
        println!("  {hdr}");
        let text = m.get("body").and_then(Value::as_str).unwrap_or("");
        let preview = preview_body(text, 3, 100);
        for line in preview {
            println!("      {}", line.dimmed());
        }
    }
    println!();
}

/// First `max_lines` non-empty lines of a body, each truncated to `width`.
fn preview_body(text: &str, max_lines: usize, width: usize) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(max_lines)
        .map(|l| truncate(l, width))
        .collect()
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

async fn timeline(deal_id: String, org: Option<String>, json: bool) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    let org = resolve_org(org)?;
    let body = client
        .get(&format!("/organizations/{org}/crm/deals/{deal_id}/timeline"))
        .await
        .context("GET deal timeline")?;
    if json {
        print_json(&body);
    } else {
        render_timeline(&body, true);
    }
    Ok(())
}

/// Render a merged, date-sorted event list newest-first. `heading` prints the
/// "Timeline" title (off when appended under `deals show`).
/// Extract the timeline event array from an API response body.
/// The API returns `{ dealId, items: [...] }` (see CrmDealTimeline in
/// smooai/packages/backend/src/routes/crm/crm-deals.ts). Read `items`; keep the
/// bare-array + legacy `events` fallbacks so no consumer regresses.
fn extract_timeline_items(body: &Value) -> Vec<Value> {
    body.as_array()
        .cloned()
        .or_else(|| body.get("items").and_then(Value::as_array).cloned())
        .or_else(|| body.get("events").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

fn render_timeline(body: &Value, heading: bool) {
    let mut items = extract_timeline_items(body);
    // Newest first. `at` is an ISO string; lexical sort matches chronological.
    items.sort_by(|a, b| {
        let av = a.get("at").and_then(Value::as_str).unwrap_or("");
        let bv = b.get("at").and_then(Value::as_str).unwrap_or("");
        bv.cmp(av)
    });
    println!();
    if heading {
        println!("  {}", "Timeline".bold());
    } else {
        println!("  {}", "Timeline".dimmed());
    }
    if items.is_empty() {
        println!("\n  {}\n", "no activity yet".dimmed());
        return;
    }
    for e in &items {
        let kind = e.get("type").and_then(Value::as_str).unwrap_or("event");
        let glyph = timeline_glyph(kind);
        let when = format!("{:<10}", short_date(e.get("at")));
        let summary = e.get("summary").and_then(Value::as_str).unwrap_or("—");
        let kind_cell = format!("{:<8}", truncate(kind, 8));
        println!("  {}  {}  {}  {}", when.dimmed(), glyph, kind_cell.dimmed(), summary);
    }
    println!();
}

/// A single-glyph icon per timeline event type.
fn timeline_glyph(kind: &str) -> &'static str {
    match kind.to_lowercase().as_str() {
        "activity" | "activities" => "◆",
        "note" | "notes" => "✎",
        "conversation" | "conversations" | "message" | "email" => "✉",
        "invoice" | "invoices" => "$",
        "task" | "tasks" => "☑",
        "stage" | "stage_change" => "↻",
        _ => "•",
    }
}

// ---------------------------------------------------------------------------
// Invoices
// ---------------------------------------------------------------------------

async fn invoices(cmd: InvoicesCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        InvoicesCmd::List { org, deal, contact, json } => {
            let org = resolve_org(org)?;
            let mut params: Vec<String> = Vec::new();
            if let Some(d) = deal.filter(|s| !s.trim().is_empty()) {
                params.push(format!("dealId={d}"));
            }
            if let Some(c) = contact.filter(|s| !s.trim().is_empty()) {
                params.push(format!("contactId={c}"));
            }
            let path = format!("/organizations/{org}/invoicing/invoices?{}", params.join("&"));
            let body = client.get(&path).await.context("GET invoices")?;
            if json {
                print_json(&body);
            } else {
                render_invoices(&body);
            }
        }
        InvoicesCmd::Show { invoice_id, org } => {
            let org = resolve_org(org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{org}/invoicing/invoices/{invoice_id}"))
                    .await
                    .context("GET invoice")?,
            );
        }
    }
    Ok(())
}

fn render_invoices(body: &Value) {
    let items = body
        .as_array()
        .cloned()
        .or_else(|| body.get("invoices").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let count = format!("({})", items.len());
    println!();
    println!("  {} {}", "Invoices".bold(), count.dimmed());
    if items.is_empty() {
        println!("\n  {}\n", "none".dimmed());
        return;
    }
    let (h_num, h_status, h_total, h_due) = (
        format!("{:<20}", "NUMBER"),
        format!("{:<14}", "STATUS"),
        format!("{:>13}", "TOTAL"),
        format!("{:<10}", "DUE"),
    );
    println!();
    println!("  {}  {}  {}  {}", h_num.dimmed(), h_status.dimmed(), h_total.dimmed(), h_due.dimmed());
    for inv in &items {
        let num = truncate(inv.get("number").or_else(|| inv.get("id")).and_then(Value::as_str).unwrap_or("—"), 20);
        let status = inv.get("status").and_then(Value::as_str).unwrap_or("");
        let total = fmt_cents(inv.get("total").and_then(Value::as_i64).unwrap_or(0));
        let total_cell = format!("{total:>13}");
        let due = short_date(inv.get("dueAt").or_else(|| inv.get("dueDate")));
        println!("  {:<20}  {}  {}  {}", num, invoice_status_cell(status, 14), total_cell, due);
    }
    println!();
}

// --- small formatting helpers ------------------------------------------------

/// A JSON number OR numeric-string (drizzle serializes numeric(15,2) as a
/// string) as an f64.
fn as_money(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
}

fn fmt_money(v: Option<f64>) -> String {
    match v {
        Some(n) => format!("${}", group_thousands(n)),
        None => "—".to_string(),
    }
}

/// Integer cents → a `$1,234.56` dollar string. Invoice amounts arrive as
/// integer cents (`total`, `amount_paid`, …), unlike deal `value` (dollars).
fn fmt_cents(cents: i64) -> String {
    format!("${}", group_thousands(cents as f64 / 100.0))
}

/// A tiny inline progress bar for a 0–100 probability, e.g. `██████░░░░ 60%`.
fn prob_bar(pct: f64, width: usize) -> String {
    let p = pct.clamp(0.0, 100.0);
    let filled = ((p / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let bar: String = "█".repeat(filled);
    let rest: String = "░".repeat(width - filled);
    let label = format!("{}%", p.round() as i64);
    format!("{}{} {}", bar.cyan(), rest.dimmed(), label.dimmed())
}

/// A green ✓ / dimmed · cell for a boolean flag.
fn yes_no(b: bool) -> String {
    if b {
        "✓".green().to_string()
    } else {
        "·".dimmed().to_string()
    }
}

/// True when an ISO-8601 timestamp is strictly in the past.
fn is_overdue(v: Option<&Value>) -> bool {
    let Some(s) = v.and_then(Value::as_str) else { return false };
    match chrono::DateTime::parse_from_rfc3339(s) {
        Ok(dt) => dt < chrono::Utc::now(),
        Err(_) => false,
    }
}

/// Color + pad an invoice status: paid=green, overdue/uncollectible=red,
/// void=dimmed, else cyan. Pads BEFORE coloring so columns stay aligned.
fn invoice_status_cell(status: &str, width: usize) -> String {
    let plain = truncate(status.trim(), width);
    if plain.is_empty() {
        return format!("{:<width$}", "—").dimmed().to_string();
    }
    let padded = format!("{plain:<width$}");
    match plain.to_lowercase().as_str() {
        "paid" => padded.green().to_string(),
        "overdue" | "uncollectible" => padded.red().to_string(),
        "void" | "draft" => padded.dimmed().to_string(),
        _ => padded.cyan().to_string(),
    }
}

/// `1234567.5` → `1,234,567.50`. Comma-grouped, always 2 decimals.
fn group_thousands(n: f64) -> String {
    let neg = n < 0.0;
    let cents = (n.abs() * 100.0).round() as u64;
    let (dollars, frac) = (cents / 100, cents % 100);
    let ds = dollars.to_string();
    let bytes = ds.as_bytes();
    let len = bytes.len();
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    format!("{}{}.{:02}", if neg { "-" } else { "" }, out, frac)
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

/// Color a stage label (green=won, red=lost, cyan=otherwise). No padding.
fn stage_color(stage: &str) -> String {
    let s = stage.trim();
    if s.is_empty() {
        return "—".dimmed().to_string();
    }
    let low = s.to_lowercase();
    if low.contains("won") {
        s.green().to_string()
    } else if low.contains("lost") {
        s.red().to_string()
    } else {
        s.cyan().to_string()
    }
}

/// Left-pad a stage label to `width` THEN color it, so table columns stay
/// aligned (padding a pre-colored string would count the ANSI escapes).
fn stage_cell(stage: &str, width: usize) -> String {
    let plain = truncate(stage.trim(), width);
    let padded = format!("{plain:<width$}");
    let low = stage.to_lowercase();
    if plain.is_empty() {
        format!("{:<width$}", "—").dimmed().to_string()
    } else if low.contains("won") {
        padded.green().to_string()
    } else if low.contains("lost") {
        padded.red().to_string()
    } else {
        padded.cyan().to_string()
    }
}

/// True for a canonical 8-4-4-4-12 hex uuid, so `--company`/`--contact`
/// accept either an id or a human name.
fn looks_like_uuid(s: &str) -> bool {
    let s = s.trim();
    s.len() == 36
        && s.chars()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// Lowercased, trimmed email if it looks like an email (`x@y`).
fn norm_email(v: &Value) -> Option<String> {
    let s = v.get("email")?.as_str()?.trim().to_lowercase();
    if s.contains('@') && !s.contains(';') && !s.contains(' ') {
        Some(s)
    } else {
        None
    }
}

/// Last 10 digits of the phone, if there are at least 10.
fn norm_phone(v: &Value) -> Option<String> {
    let raw = v.get("phone")?.as_str()?;
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    if digits.len() >= 10 {
        Some(digits[digits.len() - 10..].to_string())
    } else {
        None
    }
}

/// Fetch every contact in the org, paging in blocks of 200. Paces between
/// pages so the existing-contacts scan doesn't itself trip the rate limit.
async fn fetch_all(client: &UserClient, org: &str, rate: std::time::Duration) -> Result<Vec<Value>> {
    let mut all = Vec::new();
    let mut offset = 0u32;
    loop {
        let path = format!("/organizations/{org}/crm/contacts?limit=200&offset={offset}");
        let body = client.get(&path).await.with_context(|| format!("GET contacts (offset {offset})"))?;
        let page = body.as_array().cloned().unwrap_or_default();
        let n = page.len();
        all.extend(page);
        if n < 200 {
            break;
        }
        offset += 200;
        tokio::time::sleep(rate).await;
    }
    Ok(all)
}

/// A single write to perform.
#[derive(Clone, Copy)]
enum Op<'a> {
    Create,
    Update(&'a str),
}

/// Execute one write, pacing first and retrying once-per-minute on the
/// contacts API's "100 requests / 60s" rate-limit error (HTTP 400 whose body
/// mentions "rate limit"). Up to 6 retries (~6 min of backoff) before giving up.
async fn exec(client: &UserClient, org: &str, op: Op<'_>, body: &Value, rate: std::time::Duration) -> Result<Value> {
    let mut attempt = 0u32;
    loop {
        tokio::time::sleep(rate).await;
        let res = match op {
            Op::Create => client.post(&format!("/organizations/{org}/crm/contacts"), body).await,
            Op::Update(id) => client.patch(&format!("/organizations/{org}/crm/contacts/{id}"), body).await,
        };
        match res {
            Ok(v) => return Ok(v),
            Err(e) if e.to_string().contains("rate limit") && attempt < 6 => {
                attempt += 1;
                eprintln!("    rate limited — waiting 61s then retrying (attempt {attempt})");
                tokio::time::sleep(std::time::Duration::from_secs(61)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn import(client: &UserClient, org: &str, file: &str, dry_run: bool, rate_ms: u64) -> Result<()> {
    let rate = std::time::Duration::from_millis(rate_ms);
    let parsed = read_body(file)?;
    let items = parsed.as_array().context("import file must contain a JSON array of contact objects")?;

    if let Some(u) = UserClient::user_label() {
        println!();
        println!(
            "  {} importing {} contacts into org {} as {}{}",
            "●".cyan(),
            items.len().to_string().bold(),
            org.cyan(),
            u.bold(),
            if dry_run { "  [dry-run]".yellow().to_string() } else { String::new() }
        );
    }

    // Build lookup maps from the existing contacts.
    let existing = fetch_all(client, org, rate).await?;
    let mut email_to_id: HashMap<String, String> = HashMap::new();
    let mut phone_to_id: HashMap<String, String> = HashMap::new();
    for c in &existing {
        let Some(id) = c.get("id").and_then(|v| v.as_str()) else { continue };
        if let Some(e) = norm_email(c) {
            email_to_id.entry(e).or_insert_with(|| id.to_string());
        }
        if let Some(p) = norm_phone(c) {
            phone_to_id.entry(p).or_insert_with(|| id.to_string());
        }
    }

    let mut created = 0u32;
    let mut updated = 0u32;
    let mut skipped: HashMap<String, u32> = HashMap::new();

    for item in items {
        if !item.is_object() {
            *skipped.entry("not an object".into()).or_insert(0) += 1;
            continue;
        }
        let email = norm_email(item);
        let phone = norm_phone(item);
        let (key_kind, key) = match (&email, &phone) {
            (Some(e), _) => ("email", e.clone()),
            (None, Some(p)) => ("phone", p.clone()),
            (None, None) => {
                *skipped.entry("no email or phone".into()).or_insert(0) += 1;
                continue;
            }
        };
        let label = email.clone().unwrap_or_else(|| format!("phone:{key}"));

        let existing_id = if key_kind == "email" {
            email_to_id.get(&key).cloned()
        } else {
            phone_to_id.get(&key).cloned()
        };

        if let Some(id) = existing_id {
            if dry_run {
                println!("  {} would update {} {}", "↻".yellow(), id.dimmed(), label.dimmed());
            } else {
                exec(client, org, Op::Update(&id), item, rate)
                    .await
                    .with_context(|| format!("update {label}"))?;
                println!("  {} updated {} {}", "↻".yellow(), id.dimmed(), label.dimmed());
            }
            updated += 1;
        } else if dry_run {
            println!("  {} would create {}", "✚".green(), label.bold());
            created += 1;
            // Reserve the key so a duplicate later in the file dedups in dry-run too.
            remember(&mut email_to_id, &mut phone_to_id, &email, &phone, "(dry-run)");
        } else {
            let resp = exec(client, org, Op::Create, item, rate).await.with_context(|| format!("create {label}"))?;
            let new_id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            println!("  {} created {} {}", "✚".green(), new_id.dimmed(), label.bold());
            created += 1;
            remember(&mut email_to_id, &mut phone_to_id, &email, &phone, &new_id);
        }
    }

    println!();
    println!(
        "  {} {} created, {} updated, {} skipped",
        "✓".green().bold(),
        created.to_string().green().bold(),
        updated.to_string().yellow().bold(),
        skipped.values().sum::<u32>().to_string().bold()
    );
    let mut reasons: Vec<(&String, &u32)> = skipped.iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(a.1));
    for (reason, n) in reasons {
        println!("      {} {} {}", "⊘".dimmed(), n.to_string().dimmed(), reason.dimmed());
    }
    println!();
    Ok(())
}

/// Record a key → id mapping so within-file duplicates upsert rather
/// than double-insert.
fn remember(email_to_id: &mut HashMap<String, String>, phone_to_id: &mut HashMap<String, String>, email: &Option<String>, phone: &Option<String>, id: &str) {
    if let Some(e) = email {
        email_to_id.insert(e.clone(), id.to_string());
    }
    if let Some(p) = phone {
        phone_to_id.insert(p.clone(), id.to_string());
    }
}

// ---------------------------------------------------------------------------
// Associations — link entities to each other (SMOODEV-2644).
//
// Backed by the native api-prime associations endpoints (SmooAI/smooai#3068):
//   GET/POST   /organizations/{org}/crm/associations
//   PATCH/DELETE /organizations/{org}/crm/associations/{id}
// FROM/TO operands are `TYPE:REF` (e.g. `contact:jane@x.com`, `company:Acme`,
// `deal:"Big Deal"`, or `task:<uuid>`). contact/company/deal refs resolve by
// name/email/title via the existing resolvers; other types accept a uuid only.
// ---------------------------------------------------------------------------

/// Sentinel operands that clear a link instead of setting one.
fn is_clear_token(s: &str) -> bool {
    let t = s.trim();
    t == "--" || t == "-" || t.eq_ignore_ascii_case("none")
}

/// A parsed `TYPE:REF` operand, e.g. `contact:jane@x.com` or `company:Acme`.
#[derive(Debug, PartialEq, Eq)]
struct EntityRef {
    entity_type: String,
    reference: String,
}

/// Parse a `TYPE:REF` operand. Splits on the FIRST colon so a REF may itself
/// contain colons; TYPE is normalized, REF is trimmed of surrounding quotes.
fn parse_entity_ref(s: &str) -> Result<EntityRef> {
    let (ty, reference) = s
        .split_once(':')
        .with_context(|| format!("expected TYPE:REF (e.g. contact:jane@x.com, company:Acme, deal:\"Big Deal\") — got '{s}'"))?;
    let entity_type = normalize_entity_type(ty)?;
    let reference = reference.trim().trim_matches('"').trim().to_string();
    if reference.is_empty() {
        anyhow::bail!("empty reference in '{s}' — expected TYPE:REF");
    }
    Ok(EntityRef { entity_type, reference })
}

/// Canonical entity type: lowercased, singularized, validated against the
/// association vocabulary.
fn normalize_entity_type(s: &str) -> Result<String> {
    let t = s.trim().to_lowercase();
    let norm = match t.as_str() {
        "contact" | "contacts" => "contact",
        "company" | "companies" => "company",
        "deal" | "deals" => "deal",
        "task" | "tasks" => "task",
        "proposal" | "proposals" => "proposal",
        "funnel" | "funnels" => "funnel",
        "custom_object" | "custom-object" | "custom_objects" | "customobject" => "custom_object",
        other => anyhow::bail!("unknown entity type '{other}' — expected one of contact|company|deal|task|proposal|funnel|custom_object"),
    };
    Ok(norm.to_string())
}

/// Resolve a `TYPE:REF` to a concrete entity id. contact/company/deal resolve
/// by name/email/title (or uuid); every other type accepts a uuid only.
async fn resolve_entity_id(client: &UserClient, org: &str, er: &EntityRef) -> Result<String> {
    match er.entity_type.as_str() {
        "contact" => resolve_contact_id(client, org, &er.reference).await,
        "company" => resolve_company_id(client, org, &er.reference).await,
        "deal" => resolve_deal_id(client, org, &er.reference).await,
        other => {
            if looks_like_uuid(&er.reference) {
                Ok(er.reference.clone())
            } else {
                anyhow::bail!("{other} references must be a uuid (no name lookup for {other} yet) — got '{}'", er.reference)
            }
        }
    }
}

/// GET an entity's associations, optionally filtered by the other side's type
/// and/or the association type.
async fn get_associations(
    client: &UserClient,
    org: &str,
    entity_type: &str,
    entity_id: &str,
    other_type: Option<&str>,
    association_type: Option<&str>,
) -> Result<Vec<Value>> {
    let mut path = format!("/organizations/{org}/crm/associations?entityType={entity_type}&entityId={entity_id}");
    if let Some(t) = other_type {
        path.push_str(&format!("&otherType={t}"));
    }
    if let Some(a) = association_type.filter(|a| !a.trim().is_empty()) {
        path.push_str(&format!("&associationType={}", urlencoding::encode(a.trim())));
    }
    let body = client.get(&path).await.context("GET associations")?;
    Ok(body.as_array().cloned().unwrap_or_default())
}

async fn associations(cmd: AssocCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        AssocCmd::Link {
            from,
            to,
            association_type,
            label,
            org,
        } => {
            let org = resolve_org(org)?;
            let from_ref = parse_entity_ref(&from)?;
            let to_ref = parse_entity_ref(&to)?;
            let source_id = resolve_entity_id(&client, &org, &from_ref).await?;
            let target_id = resolve_entity_id(&client, &org, &to_ref).await?;
            let mut body = json!({
                "sourceType": from_ref.entity_type,
                "sourceId": source_id,
                "targetType": to_ref.entity_type,
                "targetId": target_id,
            });
            if let Some(a) = association_type.filter(|s| !s.trim().is_empty()) {
                body["associationType"] = json!(a.trim());
            }
            if let Some(l) = label.filter(|s| !s.trim().is_empty()) {
                body["label"] = json!(l);
            }
            let row = client
                .post(&format!("/organizations/{org}/crm/associations"), &body)
                .await
                .context("POST association")?;
            print_json(&row);
        }
        AssocCmd::Unlink {
            from,
            to,
            association_type,
            org,
        } => {
            let org = resolve_org(org)?;
            assoc_unlink(&client, &org, &from, &to, association_type).await?;
        }
        AssocCmd::List {
            entity,
            other_type,
            association_type,
            org,
            json,
        } => {
            let org = resolve_org(org)?;
            let er = parse_entity_ref(&entity)?;
            let id = resolve_entity_id(&client, &org, &er).await?;
            let other = match other_type {
                Some(t) => Some(normalize_entity_type(&t)?),
                None => None,
            };
            let rows = get_associations(&client, &org, &er.entity_type, &id, other.as_deref(), association_type.as_deref()).await?;
            if json {
                print_json(&Value::Array(rows));
            } else {
                render_associations(&er.entity_type, &er.reference, &rows);
            }
        }
        AssocCmd::SetType {
            association_id,
            association_type,
            org,
        } => {
            let org = resolve_org(org)?;
            let body = json!({ "associationType": association_type.trim() });
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/crm/associations/{association_id}"), &body)
                    .await
                    .context("PATCH association type")?,
            );
        }
        AssocCmd::SetLabel { association_id, label, org } => {
            let org = resolve_org(org)?;
            let body = json!({ "label": label });
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/crm/associations/{association_id}"), &body)
                    .await
                    .context("PATCH association label")?,
            );
        }
    }
    Ok(())
}

/// GET the FROM entity's associations, keep the ones whose other side is the TO
/// entity, then DELETE them. Errors (listing the types) when several
/// association types link the pair and `--as` wasn't given.
async fn assoc_unlink(client: &UserClient, org: &str, from: &str, to: &str, association_type: Option<String>) -> Result<()> {
    let from_ref = parse_entity_ref(from)?;
    let to_ref = parse_entity_ref(to)?;
    let from_id = resolve_entity_id(client, org, &from_ref).await?;
    let to_id = resolve_entity_id(client, org, &to_ref).await?;
    let rows = get_associations(
        client,
        org,
        &from_ref.entity_type,
        &from_id,
        Some(&to_ref.entity_type),
        association_type.as_deref(),
    )
    .await?;
    let matches: Vec<&Value> = rows
        .iter()
        .filter(|r| r.get("otherEntityId").and_then(Value::as_str) == Some(to_id.as_str()))
        .collect();
    if matches.is_empty() {
        anyhow::bail!("no association links {from} to {to}");
    }
    if association_type.is_none() {
        let mut types: Vec<String> = matches
            .iter()
            .filter_map(|r| r.get("associationType").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        types.sort();
        types.dedup();
        if types.len() > 1 {
            anyhow::bail!(
                "{from} and {to} are linked by multiple association types ({}) — pass --as <type> to choose",
                types.join(", ")
            );
        }
    }
    for m in &matches {
        let id = m.get("id").and_then(Value::as_str).context("association row missing id")?;
        client
            .delete(&format!("/organizations/{org}/crm/associations/{id}"))
            .await
            .context("DELETE association")?;
        println!("  {} unlinked {} {} {}", "✓".green(), from.bold(), "⇹".dimmed(), to.bold());
    }
    Ok(())
}

/// True when a `parent`-typed row is the queried company's OWN parent link
/// (not one of its children). Parent links are stored child —parent→ parent, so
/// querying the child returns its parent with `direction == "source"`; its
/// children come back with `direction == "target"`. A row missing `direction`
/// is treated as the parent (a company has at most one).
fn parent_row_points_up(row: &Value) -> bool {
    row.get("direction").and_then(Value::as_str).is_none_or(|d| d == "source")
}

/// Delete the company's `parent` association (a company has at most one).
async fn clear_company_parent(client: &UserClient, org: &str, company_id: &str) -> Result<()> {
    let rows = get_associations(client, org, "company", company_id, Some("company"), Some("parent")).await?;
    let parents: Vec<&Value> = rows.iter().filter(|r| parent_row_points_up(r)).collect();
    if parents.is_empty() {
        println!("  {} no parent set", "•".dimmed());
        return Ok(());
    }
    for p in &parents {
        let id = p.get("id").and_then(Value::as_str).context("parent association missing id")?;
        client
            .delete(&format!("/organizations/{org}/crm/associations/{id}"))
            .await
            .context("DELETE parent association")?;
        println!("  {} cleared parent", "✓".green());
    }
    Ok(())
}

fn render_associations(entity_type: &str, reference: &str, rows: &[Value]) {
    println!();
    println!("  {} {}", "Associations".bold(), format!("{entity_type}:{reference}").dimmed());
    if rows.is_empty() {
        println!("\n  {}\n", "no associations".dimmed());
        return;
    }
    let (h_type, h_label, h_other) = (format!("{:<12}", "TYPE"), format!("{:<16}", "LABEL"), format!("{:<32}", "OTHER"));
    println!();
    println!("  {}  {}  {}  {}", h_type.dimmed(), h_label.dimmed(), h_other.dimmed(), "ID".dimmed());
    for r in rows {
        let atype = r.get("associationType").and_then(Value::as_str).unwrap_or("related");
        let label = r.get("label").and_then(Value::as_str).unwrap_or("");
        let other_type = r.get("otherEntityType").and_then(Value::as_str).unwrap_or("?");
        let other_label = r.get("otherEntityLabel").and_then(Value::as_str).unwrap_or("?");
        let sub = r.get("otherEntitySublabel").and_then(Value::as_str).unwrap_or("");
        let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
        let other = if sub.is_empty() {
            format!("{other_type}:{other_label}")
        } else {
            format!("{other_type}:{other_label} ({sub})")
        };
        println!(
            "  {:<12}  {:<16}  {:<32}  {}",
            truncate(atype, 12),
            truncate(label, 16),
            truncate(&other, 32),
            id.dimmed()
        );
    }
    println!();
}

// ---------------------------------------------------------------------------
// Reminders — SMOODEV-2646.
//
// "Remind me about this <entity> at <time>" against any CRM object. A Temporal
// cron delivers due reminders to the assignee. Endpoints (auth: user/m2m):
//   POST   /crm/reminders {entityType,entityId,remindAt,note?,assigneeUserId?}
//   GET    /crm/reminders?mine=true | ?entityType=&entityId=
//   DELETE /crm/reminders/:id   (soft cancel)
// ---------------------------------------------------------------------------

/// Entity kinds a reminder can target — mirrors the api-prime handler's list.
const REMINDER_ENTITY_TYPES: [&str; 7] = ["contact", "company", "deal", "task", "proposal", "funnel", "custom_object"];

#[allow(clippy::too_many_arguments)]
async fn remind(target: String, id: Option<String>, at: Option<String>, note: Option<String>, assignee: Option<String>, org: Option<String>) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    let org = resolve_org(org)?;

    // `remind cancel <id>` shorthand — same soft-cancel as `reminders cancel`.
    if target.eq_ignore_ascii_case("cancel") {
        let rid = id.filter(|s| !s.trim().is_empty()).context("`remind cancel` needs a reminder id")?;
        return cancel_reminder(&client, &org, &rid).await;
    }

    let at = at
        .filter(|s| !s.trim().is_empty())
        .context("--at is required (e.g. --at tomorrow, --at \"in 3 days\", --at 2026-08-01)")?;
    let remind_at = parse_when(&at, chrono::Utc::now())?;
    let (entity_type, entity_id) = resolve_entity_ref(&client, &org, &target).await?;

    let mut body = json!({
        "entityType": entity_type,
        "entityId": entity_id,
        "remindAt": remind_at.to_rfc3339(),
    });
    if let Some(n) = note.filter(|s| !s.trim().is_empty()) {
        body["note"] = json!(n);
    }
    if let Some(a) = assignee.filter(|s| !s.trim().is_empty()) {
        body["assigneeUserId"] = json!(resolve_assignee_id(&client, &org, &a).await?);
    }

    let r = client
        .post(&format!("/organizations/{org}/crm/reminders"), &body)
        .await
        .context("POST reminder")?;
    let rid = r.get("id").and_then(Value::as_str).unwrap_or("?");
    println!(
        "  {} reminder {} set on {} for {}",
        "⏰".green(),
        rid.dimmed(),
        format!("{entity_type}:{entity_id}").bold(),
        remind_at.to_rfc3339().cyan()
    );
    Ok(())
}

async fn reminders(cmd: RemindersCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        RemindersCmd::List { org, mine, entity, json } => {
            let org = resolve_org(org)?;
            let path = if mine {
                format!("/organizations/{org}/crm/reminders?mine=true")
            } else if let Some(e) = entity.filter(|s| !s.trim().is_empty()) {
                let (entity_type, entity_id) = resolve_entity_ref(&client, &org, &e).await?;
                format!("/organizations/{org}/crm/reminders?entityType={entity_type}&entityId={entity_id}")
            } else {
                anyhow::bail!("pass --mine (your reminders) or --entity <TYPE:REF> (one entity's)");
            };
            let body = client.get(&path).await.context("GET reminders")?;
            if json {
                print_json(&body);
            } else {
                render_reminders(&body);
            }
        }
        RemindersCmd::Cancel { id, org } => {
            let org = resolve_org(org)?;
            cancel_reminder(&client, &org, &id).await?;
        }
    }
    Ok(())
}

async fn cancel_reminder(client: &UserClient, org: &str, id: &str) -> Result<()> {
    client
        .delete(&format!("/organizations/{org}/crm/reminders/{id}"))
        .await
        .context("DELETE reminder")?;
    println!("  {} cancelled reminder {}", "🗑".red(), id.dimmed());
    Ok(())
}

/// Split + validate a `TYPE:REF` entity ref (no network). Returns the lowercased
/// entity kind and the raw reference (uuid or human name/email).
fn split_entity_ref(target: &str) -> Result<(String, String)> {
    let (kind, reference) = target
        .split_once(':')
        .context("entity must be TYPE:REF, e.g. contact:jane@acme.com or deal:<uuid>")?;
    let kind = kind.trim().to_lowercase();
    let reference = reference.trim().to_string();
    if reference.is_empty() {
        anyhow::bail!("entity ref '{target}' has no REF after the ':'");
    }
    if !REMINDER_ENTITY_TYPES.contains(&kind.as_str()) {
        anyhow::bail!("unknown entity type '{kind}' — expected one of {}", REMINDER_ENTITY_TYPES.join(", "));
    }
    Ok((kind, reference))
}

/// Resolve a `TYPE:REF` entity ref into `(entityType, entityId)`. REF is a uuid
/// (used directly) or, for contact/company/deal, a human name/email resolved
/// against the org. The other kinds require a uuid (no CLI resolver exists).
async fn resolve_entity_ref(client: &UserClient, org: &str, target: &str) -> Result<(String, String)> {
    let (kind, reference) = split_entity_ref(target)?;
    let id = if looks_like_uuid(&reference) {
        reference
    } else {
        match kind.as_str() {
            "contact" => resolve_contact_id(client, org, &reference).await?,
            "company" => resolve_company_id(client, org, &reference).await?,
            "deal" => resolve_deal_id(client, org, &reference).await?,
            other => anyhow::bail!("pass a uuid for {other} (name resolution only supports contact/company/deal)"),
        }
    };
    Ok((kind, id))
}

/// Resolve `--assignee` (email or user-id uuid) to an `auth.users` id. A uuid is
/// used directly; an email is matched against the org members' `userEmail`.
async fn resolve_assignee_id(client: &UserClient, org: &str, s: &str) -> Result<String> {
    if looks_like_uuid(s) {
        return Ok(s.to_string());
    }
    let body = client.get(&format!("/organizations/{org}/members")).await.context("GET members")?;
    let needle = s.trim().to_lowercase();
    body.get("members")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter().find_map(|m| {
                let email = m.get("userEmail").and_then(Value::as_str)?.trim().to_lowercase();
                (email == needle).then(|| m.get("userId").and_then(Value::as_str).map(str::to_string)).flatten()
            })
        })
        .with_context(|| format!("no org member matches '{s}' — run `th api members list`"))
}

/// Parse a human-ish `--at` value into an absolute UTC instant, relative to
/// `now`. Accepts (in order): RFC3339; a bare `YYYY-MM-DD`; the keywords
/// `now`/`today`/`tomorrow`/`next week`/`next month`; `in <N> <unit>` and the
/// shorthand `<N><unit>` where unit ∈ m/h/d/w (minutes/hours/days/weeks).
///
/// ponytail: bare dates and day-granularity keywords land at 09:00 UTC — a
/// fixed, documented default rather than guessing the user's timezone. Pass full
/// RFC3339 when you need an exact instant.
fn parse_when(input: &str, now: chrono::DateTime<chrono::Utc>) -> Result<chrono::DateTime<chrono::Utc>> {
    let s = input.trim();
    let lower = s.to_lowercase();

    // 1. Full RFC3339 — exact, honored as-is (normalized to UTC).
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&chrono::Utc));
    }

    // 2. Bare calendar date → 09:00:00 UTC that day.
    if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let at_nine = date.and_time(chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        return Ok(chrono::DateTime::from_naive_utc_and_offset(at_nine, chrono::Utc));
    }

    // 3. Keywords.
    match lower.as_str() {
        "now" => return Ok(now),
        "today" => return Ok(at_hour(now, 9)),
        "tomorrow" => return Ok(at_hour(now + chrono::Duration::days(1), 9)),
        "next week" => return Ok(at_hour(now + chrono::Duration::weeks(1), 9)),
        "next month" => return Ok(at_hour(now + chrono::Duration::days(30), 9)),
        _ => {}
    }

    // 4. `in <N> <unit>` or shorthand `<N><unit>`.
    if let Some(dur) = parse_relative(&lower) {
        return Ok(now + dur);
    }

    anyhow::bail!("could not parse --at '{input}' — try `tomorrow`, `next week`, `in 3 days`, `2h`, `2026-08-01`, or full RFC3339")
}

/// Snap a UTC datetime to `hour:00:00` on its own day.
fn at_hour(dt: chrono::DateTime<chrono::Utc>, hour: u32) -> chrono::DateTime<chrono::Utc> {
    let day = dt.date_naive().and_time(chrono::NaiveTime::from_hms_opt(hour, 0, 0).unwrap());
    chrono::DateTime::from_naive_utc_and_offset(day, chrono::Utc)
}

/// Parse `in <N> <unit>` or the shorthand `<N><unit>` into a Duration. Units:
/// m/min(s)/minute(s), h/hr(s)/hour(s), d/day(s), w/wk(s)/week(s). None on any
/// other shape (negative counts included).
fn parse_relative(lower: &str) -> Option<chrono::Duration> {
    let body = lower.strip_prefix("in ").map(str::trim).unwrap_or(lower);
    let (num_str, unit) = match body.split_once(char::is_whitespace) {
        Some((n, u)) => (n.trim(), u.trim()),
        None => {
            // shorthand: leading digits then a unit suffix (e.g. "3d", "30m").
            let split = body.find(|c: char| !c.is_ascii_digit())?;
            (&body[..split], body[split..].trim())
        }
    };
    let n: i64 = num_str.parse().ok()?;
    if n < 0 {
        return None;
    }
    match unit {
        "m" | "min" | "mins" | "minute" | "minutes" => Some(chrono::Duration::minutes(n)),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(chrono::Duration::hours(n)),
        "d" | "day" | "days" => Some(chrono::Duration::days(n)),
        "w" | "wk" | "wks" | "week" | "weeks" => Some(chrono::Duration::weeks(n)),
        _ => None,
    }
}

fn render_reminders(body: &Value) {
    let items = body.as_array().cloned().unwrap_or_default();
    println!();
    println!("  {} {}", "Reminders".bold(), format!("({})", items.len()).dimmed());
    if items.is_empty() {
        println!("\n  {}\n", "none".dimmed());
        return;
    }
    let (h_when, h_status, h_entity) = (format!("{:<26}", "WHEN"), format!("{:<9}", "STATUS"), format!("{:<14}", "ENTITY"));
    println!();
    println!("  {}  {}  {}  {}", h_when.dimmed(), h_status.dimmed(), h_entity.dimmed(), "NOTE".dimmed());
    for r in &items {
        let when = r.get("remindAt").and_then(Value::as_str).unwrap_or("—");
        let etype = r.get("entityType").and_then(Value::as_str).unwrap_or("—");
        let note = r.get("note").and_then(Value::as_str).unwrap_or("");
        println!(
            "  {:<26}  {}  {:<14}  {}",
            truncate(when, 26),
            reminder_status(r),
            truncate(etype, 14),
            truncate(note, 40).dimmed()
        );
    }
    println!();
}

/// A reminder's lifecycle state from its `sentAt` / `cancelledAt` stamps,
/// padded + colored for the table.
fn reminder_status(r: &Value) -> String {
    let present = |k: &str| r.get(k).map(|v| !v.is_null()).unwrap_or(false);
    if present("cancelledAt") {
        format!("{:<9}", "cancelled").red().to_string()
    } else if present("sentAt") {
        format!("{:<9}", "sent").green().to_string()
    } else {
        format!("{:<9}", "pending").cyan().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_timeline_items, fmt_cents, group_thousands, image_content_type, is_clear_token, is_overdue, looks_like_uuid, norm_email, norm_phone,
        normalize_entity_type, own_image_id, parent_row_points_up, parse_entity_ref, parse_relative, parse_when, preview_body, reminder_status,
        split_entity_ref, timeline_glyph, EntityRef, ImageEntity,
    };
    use serde_json::json;

    #[test]
    fn parent_row_points_up_only_for_source_direction() {
        // Querying the child, its parent row comes back as direction=="source".
        assert!(parent_row_points_up(&json!({ "direction": "source", "otherEntityId": "parent-co" })));
        // Its children come back as "target" — not the child's own parent.
        assert!(!parent_row_points_up(&json!({ "direction": "target", "otherEntityId": "child-co" })));
        // Missing direction → treat as the parent (a company has at most one).
        assert!(parent_row_points_up(&json!({ "otherEntityId": "parent-co" })));
    }

    #[test]
    fn entity_ref_parses_type_and_ref() {
        assert_eq!(
            parse_entity_ref("contact:jane@x.com").unwrap(),
            EntityRef {
                entity_type: "contact".into(),
                reference: "jane@x.com".into(),
            }
        );
        // Plural type normalizes to singular; surrounding quotes are stripped.
        assert_eq!(
            parse_entity_ref("companies:\"Acme Corp\"").unwrap(),
            EntityRef {
                entity_type: "company".into(),
                reference: "Acme Corp".into(),
            }
        );
        // A uuid ref passes through untouched.
        assert_eq!(
            parse_entity_ref("deal:660e8400-e29b-41d4-a716-446655440000").unwrap().reference,
            "660e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn entity_ref_splits_on_first_colon_only() {
        // A title containing a colon keeps everything after the first colon.
        let er = parse_entity_ref("deal:Q3: Big Deal").unwrap();
        assert_eq!(er.entity_type, "deal");
        assert_eq!(er.reference, "Q3: Big Deal");
    }

    #[test]
    fn entity_ref_rejects_bad_input() {
        // No colon at all.
        assert!(parse_entity_ref("justaname").is_err());
        // Empty reference.
        assert!(parse_entity_ref("contact:").is_err());
        // Unknown type.
        assert!(parse_entity_ref("widget:x").is_err());
    }

    #[test]
    fn entity_type_normalizes_singular_and_plural() {
        assert_eq!(normalize_entity_type("Contacts").unwrap(), "contact");
        assert_eq!(normalize_entity_type("company").unwrap(), "company");
        assert_eq!(normalize_entity_type("DEALS").unwrap(), "deal");
        assert_eq!(normalize_entity_type("custom-object").unwrap(), "custom_object");
        assert!(normalize_entity_type("nope").is_err());
    }

    #[test]
    fn clear_token_recognizes_sentinels() {
        assert!(is_clear_token("--"));
        assert!(is_clear_token("-"));
        assert!(is_clear_token("none"));
        assert!(is_clear_token("None"));
        assert!(is_clear_token("  none  "));
        assert!(!is_clear_token("Acme"));
        assert!(!is_clear_token(""));
    }

    #[test]
    fn image_content_type_maps_allowed_and_rejects_others() {
        assert_eq!(image_content_type("avatar.png").unwrap(), "image/png");
        assert_eq!(image_content_type("logo.JPG").unwrap(), "image/jpeg"); // case-insensitive
        assert_eq!(image_content_type("pic.jpeg").unwrap(), "image/jpeg");
        assert_eq!(image_content_type("anim.gif").unwrap(), "image/gif");
        assert_eq!(image_content_type("shot.webp").unwrap(), "image/webp");
        // SVG is a script vector — rejected, like anything unrecognized or extensionless.
        assert!(image_content_type("icon.svg").is_err());
        assert!(image_content_type("doc.pdf").is_err());
        assert!(image_content_type("noext").is_err());
    }

    #[test]
    fn image_id_field_per_entity() {
        assert_eq!(ImageEntity::Contact.image_id_field(), "avatarImageId");
        assert_eq!(ImageEntity::Company.image_id_field(), "logoImageId");
        assert_eq!(ImageEntity::Deal.image_id_field(), "imageId");
        assert_eq!(ImageEntity::Contact.wire(), "contact");
        assert_eq!(ImageEntity::Company.seg(), "companies");
    }

    #[test]
    fn own_image_id_reads_the_right_field() {
        let contact = json!({ "id": "c1", "avatarImageId": "img-1" });
        assert_eq!(own_image_id(ImageEntity::Contact, &contact).as_deref(), Some("img-1"));

        let company = json!({ "id": "co1", "logoImageId": "img-2" });
        assert_eq!(own_image_id(ImageEntity::Company, &company).as_deref(), Some("img-2"));

        // A deal whose displayed image is INHERITED carries a null `imageId` —
        // remove-image must treat that as "nothing to remove", not an error.
        let inherited = json!({ "id": "d1", "imageId": null, "displayImageUrl": "https://b/co.png" });
        assert_eq!(own_image_id(ImageEntity::Deal, &inherited), None);
        // Absent field is also None (not every read echoes it).
        assert_eq!(own_image_id(ImageEntity::Deal, &json!({ "id": "d1" })), None);
        // A deal with its own image resolves the id.
        let owned = json!({ "id": "d1", "imageId": "img-3" });
        assert_eq!(own_image_id(ImageEntity::Deal, &owned).as_deref(), Some("img-3"));
    }

    #[test]
    fn timeline_items_read_from_items_key() {
        // The real API shape: `{ dealId, items: [...] }`. Regression guard for the
        // bug where the CLI read `events` and always rendered "no activity yet".
        let body = json!({ "dealId": "d1", "items": [{ "type": "deal_won", "at": "2026-07-04T00:00:00Z", "summary": "Won" }] });
        assert_eq!(extract_timeline_items(&body).len(), 1);
        // Legacy fallbacks still work.
        assert_eq!(extract_timeline_items(&json!([{ "type": "note" }])).len(), 1);
        assert_eq!(extract_timeline_items(&json!({ "events": [{ "type": "note" }] })).len(), 1);
        // No recognized array → empty.
        assert!(extract_timeline_items(&json!({ "dealId": "d1" })).is_empty());
    }

    #[test]
    fn cents_render_as_dollars() {
        assert_eq!(fmt_cents(0), "$0.00");
        assert_eq!(fmt_cents(199), "$1.99");
        assert_eq!(fmt_cents(550000), "$5,500.00");
        assert_eq!(fmt_cents(123456789), "$1,234,567.89");
    }

    #[test]
    fn timeline_glyph_maps_known_types_and_falls_back() {
        assert_eq!(timeline_glyph("invoice"), "$");
        assert_eq!(timeline_glyph("Note"), "✎");
        assert_eq!(timeline_glyph("conversation"), "✉");
        assert_eq!(timeline_glyph("activities"), "◆");
        assert_eq!(timeline_glyph("something_else"), "•");
    }

    #[test]
    fn overdue_only_for_past_timestamps() {
        assert!(is_overdue(Some(&json!("2000-01-01T00:00:00Z"))));
        assert!(!is_overdue(Some(&json!("2999-01-01T00:00:00Z"))));
        assert!(!is_overdue(Some(&json!("not-a-date"))));
        assert!(!is_overdue(None));
    }

    #[test]
    fn body_preview_trims_and_caps_lines() {
        let text = "  first line  \n\n  second  \n third \n fourth ";
        let out = preview_body(text, 2, 100);
        assert_eq!(out, vec!["first line".to_string(), "second".to_string()]);
    }

    #[test]
    fn money_is_comma_grouped_with_two_decimals() {
        assert_eq!(group_thousands(5500.0), "5,500.00");
        assert_eq!(group_thousands(1234567.5), "1,234,567.50");
        assert_eq!(group_thousands(0.0), "0.00");
        assert_eq!(group_thousands(299.0), "299.00");
        // Rounds cents, doesn't truncate.
        assert_eq!(group_thousands(99.995), "100.00");
    }

    #[test]
    fn uuid_detection_gates_name_vs_id() {
        assert!(looks_like_uuid("660e8400-e29b-41d4-a716-446655440000"));
        assert!(!looks_like_uuid("RPM Pizza"));
        assert!(!looks_like_uuid("tim.fikes@rpmpizza.com"));
        // right length, wrong dash positions
        assert!(!looks_like_uuid("660e8400e29b-41d4-a716-4466554400001"));
    }

    #[test]
    fn email_is_lowercased_and_trimmed() {
        assert_eq!(norm_email(&json!({ "email": "  Stephen@DasBBQ.com " })), Some("stephen@dasbbq.com".into()));
    }

    #[test]
    fn email_missing_or_malformed_is_none() {
        assert_eq!(norm_email(&json!({})), None);
        assert_eq!(norm_email(&json!({ "email": "" })), None);
        assert_eq!(norm_email(&json!({ "email": "not-an-email" })), None);
        // Two emails jammed in one field (Tara's row) → rejected, not guessed.
        assert_eq!(norm_email(&json!({ "email": "a@b.com; c@d.com" })), None);
    }

    #[test]
    fn phone_keys_on_last_10_digits() {
        assert_eq!(norm_phone(&json!({ "phone": "(404) 281-4855" })), Some("4042814855".into()));
        // Leading country code → same last-10 key, so +1 and bare forms dedup.
        assert_eq!(norm_phone(&json!({ "phone": "+1 404 281 4855" })), Some("4042814855".into()));
    }

    #[test]
    fn phone_too_short_is_none() {
        assert_eq!(norm_phone(&json!({ "phone": "12345" })), None);
        assert_eq!(norm_phone(&json!({})), None);
    }

    // ---- reminders -------------------------------------------------------

    #[test]
    fn entity_ref_splits_and_validates_type() {
        let (kind, r) = split_entity_ref("contact:jane@acme.com").unwrap();
        assert_eq!(kind, "contact");
        assert_eq!(r, "jane@acme.com");
        // Type is lowercased; ref is trimmed.
        let (kind, r) = split_entity_ref("  Deal : Acme renewal ").unwrap();
        assert_eq!(kind, "deal");
        assert_eq!(r, "Acme renewal");
        // All seven kinds accepted.
        for k in ["contact", "company", "deal", "task", "proposal", "funnel", "custom_object"] {
            assert!(split_entity_ref(&format!("{k}:x")).is_ok(), "{k} should be valid");
        }
    }

    #[test]
    fn entity_ref_rejects_bad_shapes() {
        assert!(split_entity_ref("jane@acme.com").is_err()); // no ':'
        assert!(split_entity_ref("contact:").is_err()); // empty ref
        assert!(split_entity_ref("widget:x").is_err()); // unknown type
    }

    #[test]
    fn parse_when_rfc3339_is_honored_exactly() {
        let now = "2026-07-15T00:00:00Z".parse().unwrap();
        let got = parse_when("2026-08-01T14:30:00Z", now).unwrap();
        assert_eq!(got.to_rfc3339(), "2026-08-01T14:30:00+00:00");
        // A non-UTC offset is normalized to UTC.
        let got = parse_when("2026-08-01T09:00:00-05:00", now).unwrap();
        assert_eq!(got.to_rfc3339(), "2026-08-01T14:00:00+00:00");
    }

    #[test]
    fn parse_when_bare_date_lands_at_0900_utc() {
        let now = "2026-07-15T00:00:00Z".parse().unwrap();
        let got = parse_when("2026-08-01", now).unwrap();
        assert_eq!(got.to_rfc3339(), "2026-08-01T09:00:00+00:00");
    }

    #[test]
    fn parse_when_keywords_are_relative_to_now() {
        let now = "2026-07-15T12:34:00Z".parse().unwrap();
        assert_eq!(parse_when("now", now).unwrap(), now);
        // tomorrow / next week snap to 09:00 UTC on their day.
        assert_eq!(parse_when("tomorrow", now).unwrap().to_rfc3339(), "2026-07-16T09:00:00+00:00");
        assert_eq!(parse_when("Next Week", now).unwrap().to_rfc3339(), "2026-07-22T09:00:00+00:00");
        assert_eq!(parse_when("today", now).unwrap().to_rfc3339(), "2026-07-15T09:00:00+00:00");
    }

    #[test]
    fn parse_when_relative_durations() {
        let now = "2026-07-15T12:00:00Z".parse().unwrap();
        assert_eq!(parse_when("in 3 days", now).unwrap().to_rfc3339(), "2026-07-18T12:00:00+00:00");
        assert_eq!(parse_when("in 2 hours", now).unwrap().to_rfc3339(), "2026-07-15T14:00:00+00:00");
        // shorthand
        assert_eq!(parse_when("30m", now).unwrap().to_rfc3339(), "2026-07-15T12:30:00+00:00");
        assert_eq!(parse_when("2w", now).unwrap().to_rfc3339(), "2026-07-29T12:00:00+00:00");
    }

    #[test]
    fn parse_when_rejects_garbage() {
        let now = "2026-07-15T12:00:00Z".parse().unwrap();
        assert!(parse_when("someday", now).is_err());
        assert!(parse_when("in -3 days", now).is_err()); // negative
        assert!(parse_when("3 fortnights", now).is_err()); // unknown unit
    }

    #[test]
    fn parse_relative_unit_aliases() {
        assert_eq!(parse_relative("in 1 minute"), Some(chrono::Duration::minutes(1)));
        assert_eq!(parse_relative("5 mins"), Some(chrono::Duration::minutes(5)));
        assert_eq!(parse_relative("2 hrs"), Some(chrono::Duration::hours(2)));
        assert_eq!(parse_relative("1 day"), Some(chrono::Duration::days(1)));
        assert_eq!(parse_relative("3w"), Some(chrono::Duration::weeks(3)));
        assert_eq!(parse_relative("nope"), None);
    }

    #[test]
    fn reminder_status_reads_lifecycle_stamps() {
        assert!(reminder_status(&json!({})).contains("pending"));
        assert!(reminder_status(&json!({ "sentAt": "2026-07-01T00:00:00Z" })).contains("sent"));
        assert!(reminder_status(&json!({ "cancelledAt": "2026-07-01T00:00:00Z" })).contains("cancelled"));
        // cancelled wins over sent.
        let both = json!({ "sentAt": "2026-07-01T00:00:00Z", "cancelledAt": "2026-07-02T00:00:00Z" });
        assert!(reminder_status(&both).contains("cancelled"));
        // explicit null stamps read as pending.
        assert!(reminder_status(&json!({ "sentAt": null, "cancelledAt": null })).contains("pending"));
    }
}
