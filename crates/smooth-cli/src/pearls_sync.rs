//! `th pearls sync` — two-way sync of the local pearl store with Smoo
//! Projects work items (pearl th-19cca5, phase 3 of th-d3e842).
//!
//! Offline-first: `~/.smooth/pearls.db` stays the read path; this is the
//! explicit reconcile step against the same work-items API `smoo work` wraps.
//!
//! - **Identity**: `sync_map(pearl_id ↔ remote_id)` in the store, plus
//!   `external_ref = "<pearl_id>@<project-slug>"` on the remote item so a
//!   fresh machine adopts existing items by id instead of duplicating them.
//! - **Last-writer-wins** on `updated_at`, with the per-side baseline the
//!   map recorded at the last sync. Both sides changed → the newer one wins
//!   and the loser is *logged* in the report (never silent).
//! - **Create-on-first-sight** both ways. **Never deletes** — a pearl or item
//!   that vanished on one side is reported and left alone on the other.
//! - **Deps ↔ `blocks` links**, **comments ↔ comments** (ours carry a
//!   `pearl-comment:<id>` first line so they aren't pulled back as echoes).
//!
//! ponytail: deps + comments are reconciled only for the ACTIVE set (mapped
//! pearls that are not closed, or were touched this run) — two GETs each.
//! A full sweep of closed history is `--full` if it ever matters.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use owo_colors::OwoColorize;
use serde::Serialize;
use serde_json::{json, Value};
use smooth_pearls::{Pearl, PearlComment, PearlDepType, PearlQuery, PearlStatus, PearlStore, PearlType, Priority, SyncMapEntry};

use crate::smooai::user_client::UserClient;

/// Store config keys (per local project).
pub const CONFIG_PROJECT_ID: &str = "sync.project_id";
pub const CONFIG_PROJECT_KEY: &str = "sync.project_key";
pub const CONFIG_LAST_PULL: &str = "sync.last_pull_at";

/// First line of a comment we pushed: `pearl-comment:<local comment id>`.
const COMMENT_MARKER: &str = "pearl-comment:";
/// Work items have no `epic` type; the pearl type survives as this label.
const EPIC_LABEL: &str = "epic";
const PAGE: u32 = 200;

#[derive(Debug, Clone, Copy)]
pub struct SyncOptions {
    pub pull: bool,
    pub push: bool,
    pub dry_run: bool,
}

/// What a run did (or, with `dry_run`, would do). `--json` prints it verbatim.
#[derive(Debug, Default, Serialize)]
pub struct SyncReport {
    pub project_id: String,
    pub dry_run: bool,
    pub pulled_created: Vec<String>,
    pub pulled_updated: Vec<String>,
    pub pushed_created: Vec<String>,
    pub pushed_updated: Vec<String>,
    pub deps_pulled: usize,
    pub links_pushed: usize,
    pub comments_pulled: usize,
    pub comments_pushed: usize,
    /// Both sides changed since the last sync — who won and why.
    pub conflicts: Vec<String>,
    /// Things left alone on purpose (vanished on one side, unmappable, …).
    pub skipped: Vec<String>,
}

// ---------------------------------------------------------------------------
// Pure field mapping — every table here has a round-trip test below.
// ---------------------------------------------------------------------------

/// `th-xxxxxx@<slug>` — the remote `externalRef` for a pearl.
pub fn external_ref(pearl_id: &str, slug: &str) -> String {
    format!("{pearl_id}@{slug}")
}

/// Inverse of [`external_ref`]: `(pearl_id, slug)`.
pub fn parse_external_ref(s: &str) -> Option<(&str, &str)> {
    let (id, slug) = s.split_once('@')?;
    (id.starts_with("th-") && !slug.is_empty()).then_some((id, slug))
}

/// The project half of the external ref: the checkout's directory name.
pub fn project_slug(store: &PearlStore) -> String {
    store
        .project_root()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "project".to_string())
}

pub fn status_to_remote(s: PearlStatus) -> &'static str {
    match s {
        PearlStatus::Open => "open",
        PearlStatus::InProgress => "in_progress",
        PearlStatus::Closed => "done",
        PearlStatus::Deferred => "blocked",
    }
}

pub fn status_from_remote(s: &str) -> Option<PearlStatus> {
    match s {
        "open" => Some(PearlStatus::Open),
        "in_progress" | "in_review" => Some(PearlStatus::InProgress),
        "blocked" => Some(PearlStatus::Deferred),
        "done" | "cancelled" => Some(PearlStatus::Closed),
        _ => None,
    }
}

/// The status to PATCH, or `None` when the remote's finer-grained state
/// (`in_review`, `cancelled`) already maps to the local one — pushing would
/// only demote it.
pub fn push_status(local: PearlStatus, remote_current: Option<&str>) -> Option<&'static str> {
    match remote_current.and_then(status_from_remote) {
        Some(mapped) if mapped == local => None,
        _ => Some(status_to_remote(local)),
    }
}

/// Pearls: 0 = critical … 4 = backlog. Work items: 0 = lowest … 4 = highest.
pub fn priority_to_remote(p: Priority) -> i64 {
    4 - i64::from(p.as_u8())
}

pub fn priority_from_remote(p: i64) -> Priority {
    // `4 - clamp` is always 0..=4, so `from_u8` can't fail; Medium is belt-and-braces.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Priority::from_u8((4 - p.clamp(0, 4)) as u8).unwrap_or(Priority::Medium)
}

pub fn type_to_remote(t: PearlType) -> &'static str {
    match t {
        PearlType::Task => "task",
        PearlType::Bug => "bug",
        PearlType::Feature | PearlType::Epic => "feature",
    }
}

pub fn type_from_remote(t: &str, labels: &[String]) -> PearlType {
    if labels.iter().any(|l| l == EPIC_LABEL) {
        return PearlType::Epic;
    }
    match t {
        "bug" | "incident" => PearlType::Bug,
        "feature" => PearlType::Feature,
        _ => PearlType::Task,
    }
}

/// Labels as the remote sees them: the pearl's own plus the `epic` marker.
pub fn labels_to_remote(pearl: &Pearl) -> Vec<String> {
    let mut out = pearl.labels.clone();
    if pearl.pearl_type == PearlType::Epic && !out.iter().any(|l| l == EPIC_LABEL) {
        out.push(EPIC_LABEL.to_string());
    }
    out
}

/// Labels as the pearl keeps them: the `epic` marker folds into the type.
pub fn labels_from_remote(labels: &[String]) -> Vec<String> {
    labels.iter().filter(|l| l.as_str() != EPIC_LABEL).cloned().collect()
}

pub fn mark_comment(c: &PearlComment) -> String {
    format!("{COMMENT_MARKER}{}\n{}", c.id, c.content)
}

/// The local comment id a pushed comment carries, if it is one of ours.
pub fn marked_id(body: &str) -> Option<&str> {
    body.lines().next()?.strip_prefix(COMMENT_MARKER).map(str::trim).filter(|s| !s.is_empty())
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn string_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

fn ts_field(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    str_field(v, key)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A remote work item as a local pearl. `id` and `parent_id` are the
/// caller's (they need the map); timestamps are the remote's so
/// last-writer-wins compares like with like.
pub fn remote_to_pearl(item: &Value, id: String, parent_id: Option<String>) -> Result<Pearl> {
    let remote_labels = string_list(item, "labels");
    let status = str_field(item, "status")
        .and_then(status_from_remote)
        .with_context(|| format!("work item {id}: unknown status {:?}", item.get("status")))?;
    let updated_at = ts_field(item, "updatedAt").context("work item missing updatedAt")?;
    Ok(Pearl {
        id,
        title: str_field(item, "title").unwrap_or("(untitled)").to_string(),
        description: str_field(item, "description").unwrap_or_default().to_string(),
        status,
        priority: priority_from_remote(item.get("priority").and_then(Value::as_i64).unwrap_or(2)),
        pearl_type: type_from_remote(str_field(item, "type").unwrap_or("task"), &remote_labels),
        labels: labels_from_remote(&remote_labels),
        assigned_to: None,
        parent_id,
        created_at: ts_field(item, "createdAt").unwrap_or(updated_at),
        updated_at,
        closed_at: ts_field(item, "completedAt").or_else(|| (status == PearlStatus::Closed).then_some(updated_at)),
        scheduled_at: None,
    })
}

/// The create body (`remote_current = None`) or PATCH body for a pearl.
pub fn pearl_to_body(pearl: &Pearl, slug: &str, project_id: &str, parent_remote: Option<&str>, remote_current: Option<&Value>) -> Value {
    let mut body = json!({
        "title": pearl.title,
        "description": if pearl.description.is_empty() { Value::Null } else { json!(pearl.description) },
        "type": type_to_remote(pearl.pearl_type),
        "priority": priority_to_remote(pearl.priority),
        "labels": labels_to_remote(pearl),
        "parentWorkItemId": parent_remote,
    });
    if let Some(s) = push_status(pearl.status, remote_current.and_then(|r| str_field(r, "status"))) {
        body["status"] = json!(s);
    }
    if remote_current.is_none() {
        body["projectId"] = json!(project_id);
        body["externalRef"] = json!(external_ref(&pearl.id, slug));
    }
    body
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

struct Ctx<'a> {
    store: &'a PearlStore,
    client: &'a UserClient,
    org: &'a str,
    project_id: &'a str,
    slug: String,
    opts: SyncOptions,
    now: DateTime<Utc>,
    report: SyncReport,
    /// Remote items seen this run (full rows), by remote id.
    remote: HashMap<String, Value>,
    /// Mapped pearls touched this run (pulled/pushed/created).
    touched: HashSet<String>,
}

impl Ctx<'_> {
    fn path(&self, rest: &str) -> String {
        format!("/organizations/{}/{rest}", self.org)
    }

    fn write(&self) -> bool {
        !self.opts.dry_run
    }

    fn maps(&self) -> Result<HashMap<String, SyncMapEntry>> {
        Ok(self.store.sync_map_list()?.into_iter().map(|e| (e.pearl_id.clone(), e)).collect())
    }

    fn map_upsert(&self, pearl_id: &str, remote_id: &str, remote_updated_at: DateTime<Utc>, local_updated_at: DateTime<Utc>) -> Result<()> {
        if self.write() {
            self.store.sync_map_upsert(&SyncMapEntry {
                pearl_id: pearl_id.to_string(),
                remote_id: remote_id.to_string(),
                remote_updated_at,
                local_updated_at,
                last_synced_at: self.now,
            })?;
        }
        Ok(())
    }

    async fn fetch_items(&self, since: Option<&str>) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        let mut offset = 0u32;
        loop {
            let mut q = format!("work-items?projectId={}&limit={PAGE}&offset={offset}", self.project_id);
            if let Some(s) = since {
                let _ = write!(q, "&updatedSince={s}");
            }
            let page = self.client.get(&self.path(&q)).await.context("GET work-items")?;
            let rows = page.as_array().cloned().unwrap_or_default();
            let n = rows.len();
            all.extend(rows);
            if n < PAGE as usize {
                return Ok(all);
            }
            offset += PAGE;
        }
    }

    // ── pull ────────────────────────────────────────────────────────────

    #[allow(clippy::too_many_lines)]
    async fn pull(&mut self) -> Result<()> {
        let since = self.store.get_config(CONFIG_LAST_PULL)?;
        let items = self.fetch_items(since.as_deref()).await?;
        let mut max_seen = since
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        let mut maps = self.maps()?;
        let mut by_remote: HashMap<String, String> = maps.values().map(|e| (e.remote_id.clone(), e.pearl_id.clone())).collect();

        // Parents first so a child's parent is mapped by the time it lands.
        let mut items = items;
        items.sort_by_key(|i| i.get("parentWorkItemId").map_or(0, |p| u8::from(!p.is_null())));

        for item in items {
            let Some(rid) = str_field(&item, "id").map(str::to_string) else { continue };
            let Some(r_upd) = ts_field(&item, "updatedAt") else { continue };
            if max_seen.is_none_or(|m| ms(r_upd) > ms(m)) {
                max_seen = Some(r_upd);
            }
            self.remote.insert(rid.clone(), item.clone());
            let parent_id = str_field(&item, "parentWorkItemId").and_then(|p| by_remote.get(p).cloned());

            if let Some(pid) = by_remote.get(&rid).cloned() {
                let Some(map) = maps.get(&pid).cloned() else { continue };
                let Some(local) = self.store.get(&pid)? else {
                    self.report.skipped.push(format!("{pid}: deleted locally; remote item {rid} left alone"));
                    continue;
                };
                // "Changed" = differs from the baseline, not "newer": a local
                // edit after a pull carries the laptop clock, the pulled row the
                // server's — skew must not hide it. Timestamps only break ties.
                let remote_changed = ms(r_upd) != ms(map.remote_updated_at);
                let local_changed = ms(local.updated_at) != ms(map.local_updated_at);
                if !remote_changed {
                    continue;
                }
                if local_changed {
                    if ms(local.updated_at) >= ms(r_upd) {
                        self.report
                            .conflicts
                            .push(format!("{pid}: changed on both sides; local is newer → local wins (pushed)"));
                        continue;
                    }
                    self.report
                        .conflicts
                        .push(format!("{pid}: changed on both sides; remote is newer → remote wins"));
                }
                let pearl = remote_to_pearl(&item, pid.clone(), parent_id)?;
                if self.write() {
                    self.store.import_pearl(&pearl)?;
                    self.store.replace_labels(&pid, &pearl.labels)?;
                }
                self.map_upsert(&pid, &rid, r_upd, r_upd)?;
                maps.insert(
                    pid.clone(),
                    SyncMapEntry {
                        remote_updated_at: r_upd,
                        local_updated_at: r_upd,
                        ..map
                    },
                );
                self.touched.insert(pid.clone());
                self.report.pulled_updated.push(pid);
                continue;
            }

            // Never seen: adopt by external ref, else create locally.
            let claimed = str_field(&item, "externalRef")
                .and_then(parse_external_ref)
                .filter(|(_, slug)| *slug == self.slug)
                .map(|(id, _)| id.to_string());
            if let Some(pid) = claimed.as_deref() {
                if let Some(local) = self.store.get(pid)? {
                    // Adoption: same id both sides, last writer wins. The
                    // MIN baseline makes the push phase see "local changed".
                    if ms(r_upd) >= ms(local.updated_at) {
                        let pearl = remote_to_pearl(&item, pid.to_string(), parent_id)?;
                        if self.write() {
                            self.store.import_pearl(&pearl)?;
                            self.store.replace_labels(pid, &pearl.labels)?;
                        }
                        self.map_upsert(pid, &rid, r_upd, r_upd)?;
                        self.report.pulled_updated.push(pid.to_string());
                    } else {
                        self.map_upsert(pid, &rid, r_upd, DateTime::<Utc>::MIN_UTC)?;
                        self.report
                            .conflicts
                            .push(format!("{pid}: adopted remote item {rid}; local is newer → local wins (pushed)"));
                    }
                    by_remote.insert(rid.clone(), pid.to_string());
                    maps.insert(
                        pid.to_string(),
                        self.store.sync_map_get(pid)?.unwrap_or_else(|| SyncMapEntry {
                            pearl_id: pid.to_string(),
                            remote_id: rid.clone(),
                            remote_updated_at: r_upd,
                            local_updated_at: r_upd,
                            last_synced_at: self.now,
                        }),
                    );
                    self.touched.insert(pid.to_string());
                    continue;
                }
            }
            // Keep the id from the external ref when it's free (stable ids
            // across machines); otherwise mint one.
            let pid = match claimed {
                Some(id) if self.store.get(&id)?.is_none() => id,
                _ => self.store.new_id()?,
            };
            let pearl = remote_to_pearl(&item, pid.clone(), parent_id)?;
            if self.write() {
                self.store.import_pearl(&pearl)?;
                self.store.replace_labels(&pid, &pearl.labels)?;
            }
            self.map_upsert(&pid, &rid, r_upd, r_upd)?;
            by_remote.insert(rid.clone(), pid.clone());
            maps.insert(
                pid.clone(),
                SyncMapEntry {
                    pearl_id: pid.clone(),
                    remote_id: rid,
                    remote_updated_at: r_upd,
                    local_updated_at: r_upd,
                    last_synced_at: self.now,
                },
            );
            self.touched.insert(pid.clone());
            self.report.pulled_created.push(pid);
        }
        if let Some(m) = max_seen {
            if self.write() {
                self.store.set_config(CONFIG_LAST_PULL, &rfc3339(m))?;
            }
        }
        Ok(())
    }

    // ── push ────────────────────────────────────────────────────────────

    async fn push(&mut self) -> Result<()> {
        let maps = self.maps()?;
        let mut locals = self.store.list(&PearlQuery::new().with_limit(0))?;
        // Parents first so the child's parentWorkItemId resolves.
        locals.sort_by_key(|p| u8::from(p.parent_id.is_some()));
        let mut remote_of: HashMap<String, String> = maps.values().map(|e| (e.pearl_id.clone(), e.remote_id.clone())).collect();

        for local in locals {
            let parent_remote = local.parent_id.as_ref().and_then(|p| remote_of.get(p).cloned());
            let Some(map) = maps.get(&local.id) else {
                let body = pearl_to_body(&local, &self.slug, self.project_id, parent_remote.as_deref(), None);
                if self.write() {
                    let created = self
                        .client
                        .post(&self.path("work-items"), &body)
                        .await
                        .with_context(|| format!("POST work item for {}", local.id))?;
                    let rid = str_field(&created, "id").context("create response missing id")?.to_string();
                    let r_upd = ts_field(&created, "updatedAt").unwrap_or(self.now);
                    self.map_upsert(&local.id, &rid, r_upd, local.updated_at)?;
                    remote_of.insert(local.id.clone(), rid.clone());
                    self.remote.insert(rid, created);
                }
                self.touched.insert(local.id.clone());
                self.report.pushed_created.push(local.id.clone());
                continue;
            };
            if ms(local.updated_at) == ms(map.local_updated_at) {
                continue;
            }
            let rid = map.remote_id.clone();
            let current = match self.remote.get(&rid) {
                Some(v) => v.clone(),
                None => match self.client.get(&self.path(&format!("work-items/{rid}"))).await {
                    Ok(v) => v,
                    Err(e) if e.to_string().contains("HTTP 404") => {
                        self.report.skipped.push(format!("{}: remote item {rid} is gone; local pearl kept", local.id));
                        continue;
                    }
                    Err(e) => return Err(e),
                },
            };
            let r_upd = ts_field(&current, "updatedAt").unwrap_or(map.remote_updated_at);
            if ms(r_upd) != ms(map.remote_updated_at) {
                if ms(r_upd) > ms(local.updated_at) {
                    if !self.opts.pull {
                        self.report.conflicts.push(format!(
                            "{}: remote is newer and --push-only skipped it; run `th pearls sync` to pull",
                            local.id
                        ));
                    }
                    continue;
                }
                self.report
                    .conflicts
                    .push(format!("{}: changed on both sides; local is newer → local wins", local.id));
            }
            let body = pearl_to_body(&local, &self.slug, self.project_id, parent_remote.as_deref(), Some(&current));
            if self.write() {
                let updated = self
                    .client
                    .patch(&self.path(&format!("work-items/{rid}")), &body)
                    .await
                    .with_context(|| format!("PATCH work item {rid} for {}", local.id))?;
                let r_upd = ts_field(&updated, "updatedAt").unwrap_or(self.now);
                self.map_upsert(&local.id, &rid, r_upd, local.updated_at)?;
                self.remote.insert(rid, updated);
            }
            self.touched.insert(local.id.clone());
            self.report.pushed_updated.push(local.id.clone());
        }
        Ok(())
    }

    // ── deps ↔ blocks links, comments ↔ comments ────────────────────────

    #[allow(clippy::too_many_lines)]
    async fn relations(&mut self) -> Result<()> {
        let maps = self.maps()?;
        let remote_of: HashMap<&str, &str> = maps.values().map(|e| (e.pearl_id.as_str(), e.remote_id.as_str())).collect();
        let pearl_of: HashMap<&str, &str> = maps.values().map(|e| (e.remote_id.as_str(), e.pearl_id.as_str())).collect();
        let deps = self.store.all_deps()?;
        // pid → pearls it blocks (local edge: `pearl_id` depends on `depends_on`).
        let mut blocks: HashMap<&str, Vec<&str>> = HashMap::new();
        for d in deps.iter().filter(|d| d.dep_type == PearlDepType::Blocks) {
            blocks.entry(d.depends_on.as_str()).or_default().push(d.pearl_id.as_str());
        }

        let active: Vec<Pearl> = self
            .store
            .list(&PearlQuery::new().with_limit(0))?
            .into_iter()
            .filter(|p| remote_of.contains_key(p.id.as_str()) && (p.status != PearlStatus::Closed || self.touched.contains(&p.id)))
            .collect();

        for pearl in active {
            let pid = pearl.id.as_str();
            let rid = remote_of[pid];

            // Links: `rid --blocks--> target` means "this item blocks target".
            let links = self
                .client
                .get(&self.path(&format!("work-items/{rid}/links")))
                .await
                .with_context(|| format!("GET links for {pid}"))?;
            let remote_targets: HashSet<String> = links
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|l| str_field(l, "targetKind") == Some("work_item") && str_field(l, "linkType") == Some("blocks"))
                        .filter_map(|l| str_field(l, "targetId").map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let local_targets: HashSet<&str> = blocks.get(pid).map(|v| v.iter().copied().collect()).unwrap_or_default();
            if self.opts.push {
                for blocked in &local_targets {
                    let Some(&target_rid) = remote_of.get(blocked) else { continue };
                    if remote_targets.contains(target_rid) {
                        continue;
                    }
                    if self.write() {
                        let body = json!({ "targetKind": "work_item", "targetId": target_rid, "linkType": "blocks" });
                        self.client
                            .post(&self.path(&format!("work-items/{rid}/links")), &body)
                            .await
                            .with_context(|| format!("POST blocks link {pid} → {blocked}"))?;
                    }
                    self.report.links_pushed += 1;
                }
            }
            if self.opts.pull {
                for target_rid in &remote_targets {
                    let Some(&blocked) = pearl_of.get(target_rid.as_str()) else { continue };
                    if local_targets.contains(blocked) {
                        continue;
                    }
                    if self.write() {
                        self.store.add_dep(blocked, pid)?;
                    }
                    self.report.deps_pulled += 1;
                }
            }

            // Comments.
            let remote_comments = self
                .client
                .get(&self.path(&format!("work-items/{rid}/comments")))
                .await
                .with_context(|| format!("GET comments for {pid}"))?;
            let remote_comments = remote_comments.as_array().cloned().unwrap_or_default();
            let remote_ids: HashSet<&str> = remote_comments.iter().filter_map(|c| str_field(c, "id")).collect();
            let pushed_ids: HashSet<&str> = remote_comments.iter().filter_map(|c| str_field(c, "body").and_then(marked_id)).collect();
            let local_comments = self.store.get_comments(pid)?;
            let local_ids: HashSet<&str> = local_comments.iter().map(|c| c.id.as_str()).collect();
            if self.opts.push {
                for c in local_comments
                    .iter()
                    .filter(|c| !remote_ids.contains(c.id.as_str()) && !pushed_ids.contains(c.id.as_str()))
                {
                    if self.write() {
                        self.client
                            .post(&self.path(&format!("work-items/{rid}/comments")), &json!({ "body": mark_comment(c) }))
                            .await
                            .with_context(|| format!("POST comment {} for {pid}", c.id))?;
                    }
                    self.report.comments_pushed += 1;
                }
            }
            if self.opts.pull {
                for c in &remote_comments {
                    let Some(cid) = str_field(c, "id") else { continue };
                    let body = str_field(c, "body").unwrap_or_default();
                    if local_ids.contains(cid) || marked_id(body).is_some() {
                        continue;
                    }
                    if self.write() {
                        self.store.import_comment(&PearlComment {
                            id: cid.to_string(),
                            pearl_id: pid.to_string(),
                            content: body.to_string(),
                            created_at: ts_field(c, "createdAt").unwrap_or(self.now),
                        })?;
                    }
                    self.report.comments_pulled += 1;
                }
            }
        }
        Ok(())
    }
}

/// Run one sync against an already-resolved org + project.
pub async fn run(store: &PearlStore, client: &UserClient, org: &str, project_id: &str, opts: SyncOptions) -> Result<SyncReport> {
    let mut ctx = Ctx {
        store,
        client,
        org,
        project_id,
        slug: project_slug(store),
        opts,
        now: Utc::now(),
        report: SyncReport {
            project_id: project_id.to_string(),
            dry_run: opts.dry_run,
            ..Default::default()
        },
        remote: HashMap::new(),
        touched: HashSet::new(),
    };
    if opts.pull {
        ctx.pull().await?;
    }
    if opts.push {
        ctx.push().await?;
    }
    ctx.relations().await?;
    Ok(ctx.report)
}

/// `th pearls sync` entry point: resolve the org + project binding, run,
/// print.
pub async fn cmd(store: &PearlStore, project: Option<String>, opts: SyncOptions, json: bool, org: Option<String>) -> Result<()> {
    let org = crate::active_org::resolve(org)?;
    let client = UserClient::from_user_session().await?;
    let project_id = match project {
        Some(p) => {
            let id = crate::smooai::work::resolve_project_id(&client, &org, &p).await?;
            if !opts.dry_run {
                store.set_config(CONFIG_PROJECT_ID, &id)?;
                store.set_config(CONFIG_PROJECT_KEY, &p)?;
            }
            id
        }
        None => match store.get_config(CONFIG_PROJECT_ID)? {
            Some(id) => id,
            None => bail!("this project isn't bound to a Smoo project yet — run `th pearls sync --project <KEY>` once (see `smoo work projects list`)"),
        },
    };
    let report = run(store, &client, &org, &project_id, opts).await?;
    if json {
        crate::smooai::print_json(&serde_json::to_value(&report)?);
    } else {
        print!("{}", render(&report, store.get_config(CONFIG_PROJECT_KEY)?.as_deref()));
    }
    Ok(())
}

pub fn render(r: &SyncReport, project_key: Option<&str>) -> String {
    let mut out = String::new();
    let head = format!("pearls sync ↔ {}", project_key.unwrap_or(&r.project_id));
    if r.dry_run {
        let _ = writeln!(out, "{} {}", head.bold(), "(dry run — nothing written)".yellow());
    } else {
        let _ = writeln!(out, "{}", head.bold());
    }
    let _ = writeln!(
        out,
        "  pull: {} created, {} updated, {} deps, {} comments",
        r.pulled_created.len(),
        r.pulled_updated.len(),
        r.deps_pulled,
        r.comments_pulled
    );
    let _ = writeln!(
        out,
        "  push: {} created, {} updated, {} links, {} comments",
        r.pushed_created.len(),
        r.pushed_updated.len(),
        r.links_pushed,
        r.comments_pushed
    );
    for line in &r.conflicts {
        let _ = writeln!(out, "  {} {line}", "conflict:".yellow());
    }
    for line in &r.skipped {
        let _ = writeln!(out, "  {} {line}", "skipped:".dimmed());
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, Query, State};
    use axum::routing::get;
    use axum::{Json, Router};
    use smooth_pearls::NewPearl;

    use super::*;

    // ── pure mapping tables ────────────────────────────────────────────

    #[test]
    fn external_ref_round_trips() {
        let r = external_ref("th-abc123", "smooth");
        assert_eq!(r, "th-abc123@smooth");
        assert_eq!(parse_external_ref(&r), Some(("th-abc123", "smooth")));
        assert_eq!(parse_external_ref("nope"), None);
        assert_eq!(parse_external_ref("SMOODEV-1@x"), None);
        assert_eq!(parse_external_ref("th-abc123@"), None);
    }

    #[test]
    fn status_table_round_trips_and_keeps_remote_refinements() {
        for s in [PearlStatus::Open, PearlStatus::InProgress, PearlStatus::Closed, PearlStatus::Deferred] {
            assert_eq!(status_from_remote(status_to_remote(s)), Some(s));
        }
        assert_eq!(status_from_remote("in_review"), Some(PearlStatus::InProgress));
        assert_eq!(status_from_remote("cancelled"), Some(PearlStatus::Closed));
        assert_eq!(status_from_remote("weird"), None);
        // in_review already means "in progress" locally → don't demote it.
        assert_eq!(push_status(PearlStatus::InProgress, Some("in_review")), None);
        assert_eq!(push_status(PearlStatus::Closed, Some("cancelled")), None);
        assert_eq!(push_status(PearlStatus::Closed, Some("in_review")), Some("done"));
        assert_eq!(push_status(PearlStatus::Open, None), Some("open"));
    }

    #[test]
    fn priority_scale_is_inverted_and_round_trips() {
        assert_eq!(priority_to_remote(Priority::Critical), 4);
        assert_eq!(priority_to_remote(Priority::Backlog), 0);
        for p in [Priority::Critical, Priority::High, Priority::Medium, Priority::Low, Priority::Backlog] {
            assert_eq!(priority_from_remote(priority_to_remote(p)), p);
        }
        assert_eq!(priority_from_remote(99), Priority::Critical);
        assert_eq!(priority_from_remote(-3), Priority::Backlog);
    }

    #[test]
    fn type_table_keeps_epic_via_label() {
        let epic = Pearl {
            pearl_type: PearlType::Epic,
            labels: vec!["pearls".into()],
            ..pearl("th-e", "Epic")
        };
        let labels = labels_to_remote(&epic);
        assert_eq!(labels, vec!["pearls", "epic"]);
        assert_eq!(type_from_remote(type_to_remote(PearlType::Epic), &labels), PearlType::Epic);
        assert_eq!(labels_from_remote(&labels), vec!["pearls"]);
        for t in [PearlType::Task, PearlType::Bug, PearlType::Feature] {
            assert_eq!(type_from_remote(type_to_remote(t), &[]), t);
        }
        assert_eq!(type_from_remote("incident", &[]), PearlType::Bug);
        assert_eq!(type_from_remote("issue", &[]), PearlType::Task);
    }

    #[test]
    fn comment_marker_round_trips() {
        let c = PearlComment {
            id: "th-c0ffee".into(),
            pearl_id: "th-p".into(),
            content: "two\nlines".into(),
            created_at: Utc::now(),
        };
        let body = mark_comment(&c);
        assert_eq!(marked_id(&body), Some("th-c0ffee"));
        assert!(body.ends_with("two\nlines"));
        assert_eq!(marked_id("plain remote comment"), None);
        assert_eq!(marked_id("pearl-comment:\nempty id"), None);
    }

    #[test]
    fn remote_item_becomes_pearl_and_back() {
        let item = json!({
            "id": "r1", "title": "T", "description": "D", "status": "in_review", "priority": 3,
            "type": "feature", "labels": ["epic", "x"], "createdAt": "2026-09-01T00:00:00Z",
            "updatedAt": "2026-09-02T00:00:00.500Z", "completedAt": null
        });
        let p = remote_to_pearl(&item, "th-000001".into(), Some("th-parent".into())).unwrap();
        assert_eq!(p.status, PearlStatus::InProgress);
        assert_eq!(p.priority, Priority::High);
        assert_eq!(p.pearl_type, PearlType::Epic);
        assert_eq!(p.labels, vec!["x"]);
        assert_eq!(p.parent_id.as_deref(), Some("th-parent"));
        assert_eq!(p.updated_at.timestamp_millis(), 1_788_307_200_500);
        let body = pearl_to_body(&p, "smooth", "proj", Some("rp"), Some(&item));
        assert_eq!(body["priority"], 3);
        assert_eq!(body["type"], "feature");
        assert_eq!(body["labels"], json!(["x", "epic"]));
        assert_eq!(body["parentWorkItemId"], "rp");
        assert!(body.get("status").is_none(), "in_review already maps to in_progress");
        assert!(body.get("externalRef").is_none());
        let create = pearl_to_body(&p, "smooth", "proj", None, None);
        assert_eq!(create["externalRef"], "th-000001@smooth");
        assert_eq!(create["projectId"], "proj");
        assert_eq!(create["status"], "in_progress");
        assert!(remote_to_pearl(&json!({"id": "x", "status": "weird", "updatedAt": "2026-09-02T00:00:00Z"}), "a".into(), None).is_err());
    }

    #[test]
    fn closed_remote_item_gets_closed_at() {
        let item = json!({"id": "r", "title": "t", "status": "done", "updatedAt": "2026-09-02T00:00:00Z"});
        let p = remote_to_pearl(&item, "th-1".into(), None).unwrap();
        assert_eq!(p.status, PearlStatus::Closed);
        assert_eq!(p.closed_at, Some(p.updated_at));
    }

    // ── mock work-items API ────────────────────────────────────────────

    #[derive(Default)]
    struct Mock {
        items: Vec<Value>,
        links: Vec<Value>,
        comments: Vec<Value>,
        deletes: usize,
        /// Bumps so every write gets a strictly later updatedAt.
        tick: i64,
        seq: u32,
    }

    type Shared = Arc<Mutex<Mock>>;

    fn now_plus(tick: i64) -> String {
        rfc3339(Utc::now() + chrono::Duration::milliseconds(tick))
    }

    impl Mock {
        fn stamp(&mut self) -> String {
            self.tick += 1;
            now_plus(self.tick)
        }
        fn next_id(&mut self) -> String {
            self.seq += 1;
            format!("00000000-0000-4000-8000-{:012}", self.seq)
        }
        fn seed_item(&mut self, fields: &Value) -> String {
            let id = self.next_id();
            let ts = self.stamp();
            let mut item = json!({
                "id": id, "title": "seed", "description": null, "status": "open", "priority": 2, "type": "task",
                "labels": [], "externalRef": null, "parentWorkItemId": null, "createdAt": ts, "updatedAt": ts
            });
            for (k, v) in fields.as_object().unwrap() {
                item[k] = v.clone();
            }
            self.items.push(item);
            id
        }
    }

    async fn list_items(State(s): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
        let m = s.lock().unwrap();
        let since = q.get("updatedSince").and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        let limit: usize = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50);
        let offset: usize = q.get("offset").and_then(|l| l.parse().ok()).unwrap_or(0);
        let rows: Vec<Value> = m
            .items
            .iter()
            .filter(|i| since.is_none_or(|s| ts_field(i, "updatedAt").unwrap() > s.with_timezone(&Utc)))
            .skip(offset)
            .take(limit)
            .cloned()
            .collect();
        Json(Value::Array(rows))
    }

    async fn create_item(State(s): State<Shared>, Json(body): Json<Value>) -> (axum::http::StatusCode, Json<Value>) {
        let mut m = s.lock().unwrap();
        let mut item =
            json!({"labels": [], "description": null, "parentWorkItemId": null, "externalRef": null, "status": "open", "priority": 2, "type": "task"});
        for (k, v) in body.as_object().unwrap() {
            item[k] = v.clone();
        }
        let ts = m.stamp();
        item["id"] = json!(m.next_id());
        item["createdAt"] = json!(ts);
        item["updatedAt"] = json!(ts);
        m.items.push(item.clone());
        (axum::http::StatusCode::CREATED, Json(item))
    }

    async fn get_item(State(s): State<Shared>, Path((_org, id)): Path<(String, String)>) -> Result<Json<Value>, axum::http::StatusCode> {
        let m = s.lock().unwrap();
        m.items
            .iter()
            .find(|i| i["id"] == id)
            .cloned()
            .map(Json)
            .ok_or(axum::http::StatusCode::NOT_FOUND)
    }

    async fn patch_item(
        State(s): State<Shared>,
        Path((_org, id)): Path<(String, String)>,
        Json(body): Json<Value>,
    ) -> Result<Json<Value>, axum::http::StatusCode> {
        let mut m = s.lock().unwrap();
        let ts = m.stamp();
        let item = m.items.iter_mut().find(|i| i["id"] == id).ok_or(axum::http::StatusCode::NOT_FOUND)?;
        for (k, v) in body.as_object().unwrap() {
            item[k] = v.clone();
        }
        item["updatedAt"] = json!(ts);
        Ok(Json(item.clone()))
    }

    async fn deny_delete(State(s): State<Shared>) -> axum::http::StatusCode {
        s.lock().unwrap().deletes += 1;
        axum::http::StatusCode::METHOD_NOT_ALLOWED
    }

    async fn list_links(State(s): State<Shared>, Path((_org, id)): Path<(String, String)>) -> Json<Value> {
        let m = s.lock().unwrap();
        Json(Value::Array(m.links.iter().filter(|l| l["workItemId"] == id).cloned().collect()))
    }

    async fn create_link(State(s): State<Shared>, Path((_org, id)): Path<(String, String)>, Json(body): Json<Value>) -> Json<Value> {
        let mut m = s.lock().unwrap();
        let mut link = body;
        link["id"] = json!(m.next_id());
        link["workItemId"] = json!(id);
        m.links.push(link.clone());
        Json(link)
    }

    async fn list_comments(State(s): State<Shared>, Path((_org, id)): Path<(String, String)>) -> Json<Value> {
        let m = s.lock().unwrap();
        Json(Value::Array(m.comments.iter().filter(|c| c["workItemId"] == id).cloned().collect()))
    }

    async fn create_comment(State(s): State<Shared>, Path((_org, id)): Path<(String, String)>, Json(body): Json<Value>) -> Json<Value> {
        let mut m = s.lock().unwrap();
        let ts = m.stamp();
        let c = json!({"id": m.next_id(), "workItemId": id, "body": body["body"], "createdAt": ts});
        m.comments.push(c.clone());
        Json(c)
    }

    async fn serve(state: Shared) -> String {
        let app = Router::new()
            .route("/organizations/{org}/work-items", get(list_items).post(create_item))
            .route("/organizations/{org}/work-items/{id}", get(get_item).patch(patch_item).delete(deny_delete))
            .route("/organizations/{org}/work-items/{id}/links", get(list_links).post(create_link))
            .route("/organizations/{org}/work-items/{id}/links/{link}", axum::routing::delete(deny_delete))
            .route("/organizations/{org}/work-items/{id}/comments", get(list_comments).post(create_comment))
            .route("/organizations/{org}/work-items/{id}/comments/{c}", axum::routing::delete(deny_delete))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn pearl(id: &str, title: &str) -> Pearl {
        Pearl {
            id: id.into(),
            title: title.into(),
            description: String::new(),
            status: PearlStatus::Open,
            priority: Priority::Medium,
            pearl_type: PearlType::Task,
            labels: vec![],
            assigned_to: None,
            parent_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            scheduled_at: None,
        }
    }

    fn new_task(title: &str) -> NewPearl {
        NewPearl {
            title: title.into(),
            description: String::new(),
            pearl_type: PearlType::Task,
            priority: Priority::Medium,
            assigned_to: None,
            parent_id: None,
            labels: vec![],
        }
    }

    fn test_store() -> PearlStore {
        let tmp = tempfile::tempdir().unwrap();
        let store = PearlStore::open_with_db(&tmp.path().join("pearls.db"), &tmp.path().join("smooth")).unwrap();
        std::mem::forget(tmp);
        store
    }

    const BOTH: SyncOptions = SyncOptions {
        pull: true,
        push: true,
        dry_run: false,
    };

    async fn harness() -> (PearlStore, UserClient, Shared) {
        let state: Shared = Arc::default();
        let base = serve(state.clone()).await;
        (test_store(), UserClient::with_bearer(base, "t"), state)
    }

    #[tokio::test]
    async fn pull_creates_local_pearls_with_deps_and_comments() {
        let (store, client, state) = harness().await;
        let (a, b) = {
            let mut m = state.lock().unwrap();
            let a = m.seed_item(&json!({"title": "Remote A", "status": "done", "priority": 4, "labels": ["ops", "epic"], "type": "task"}));
            let b = m.seed_item(&json!({"title": "Remote B", "status": "in_review"}));
            m.links
                .push(json!({"id": "l1", "workItemId": a, "targetKind": "work_item", "targetId": b, "linkType": "blocks"}));
            m.comments
                .push(json!({"id": "c1", "workItemId": b, "body": "from the web", "createdAt": now_plus(0)}));
            m.comments
                .push(json!({"id": "c2", "workItemId": b, "body": "pearl-comment:th-x\necho", "createdAt": now_plus(0)}));
            (a, b)
        };
        let r = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert_eq!(r.pulled_created.len(), 2);
        assert_eq!(r.deps_pulled, 1);
        assert_eq!(r.comments_pulled, 1, "marked echo is not pulled back");
        assert!(r.pushed_created.is_empty(), "pulled pearls are mapped, not re-pushed");
        let pa = store.get(&store.sync_map_by_remote(&a).unwrap().unwrap().pearl_id).unwrap().unwrap();
        assert_eq!(
            (pa.status, pa.priority, pa.pearl_type),
            (PearlStatus::Closed, Priority::Critical, PearlType::Epic)
        );
        assert_eq!(pa.labels, vec!["ops"]);
        let pb_id = store.sync_map_by_remote(&b).unwrap().unwrap().pearl_id;
        let pb = store.get(&pb_id).unwrap().unwrap();
        assert_eq!(pb.status, PearlStatus::InProgress);
        // A blocks B → B depends on A.
        let deps = store.get_deps(&pb_id).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on, pa.id);
        let comments = store.get_comments(&pb_id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, "c1");
        // Second run is a no-op (incremental pull + baselines).
        let r2 = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert!(r2.pulled_created.is_empty() && r2.pulled_updated.is_empty() && r2.pushed_updated.is_empty());
        assert_eq!(r2.deps_pulled + r2.comments_pulled + r2.links_pushed + r2.comments_pushed, 0);
        assert!(store.get_config(CONFIG_LAST_PULL).unwrap().is_some());
        assert_eq!(state.lock().unwrap().deletes, 0);
    }

    #[tokio::test]
    #[allow(clippy::similar_names)]
    async fn push_creates_remote_items_with_external_ref_links_and_marked_comments() {
        let (store, client, state) = harness().await;
        let blocker = store.create(&new_task("Blocker")).unwrap();
        let mut blocked = store.create(&new_task("Blocked")).unwrap();
        store.add_dep(&blocked.id, &blocker.id).unwrap();
        store.add_label(&blocked.id, "pearls").unwrap();
        store.add_comment(&blocked.id, "local note").unwrap();
        blocked = store
            .update(
                &blocked.id,
                &smooth_pearls::PearlUpdate {
                    priority: Some(Priority::Critical),
                    ..Default::default()
                },
            )
            .unwrap();

        let r = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert_eq!(r.pushed_created.len(), 2);
        assert_eq!(r.links_pushed, 1);
        assert_eq!(r.comments_pushed, 1);
        {
            let m = state.lock().unwrap();
            let remote_blocked = m.items.iter().find(|i| i["title"] == "Blocked").unwrap();
            assert_eq!(remote_blocked["externalRef"], format!("{}@smooth", blocked.id));
            assert_eq!(remote_blocked["projectId"], "proj");
            assert_eq!(remote_blocked["priority"], 4);
            assert_eq!(remote_blocked["labels"], json!(["pearls"]));
            assert_eq!(remote_blocked["status"], "open");
            let remote_blocker = m.items.iter().find(|i| i["title"] == "Blocker").unwrap();
            assert_eq!(m.links.len(), 1);
            assert_eq!(m.links[0]["workItemId"], remote_blocker["id"]);
            assert_eq!(m.links[0]["targetId"], remote_blocked["id"]);
            assert!(m.comments[0]["body"].as_str().unwrap().starts_with("pearl-comment:th-"));
        }
        // Idempotent.
        let r2 = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert!(r2.pushed_created.is_empty() && r2.pushed_updated.is_empty());
        assert_eq!(r2.links_pushed + r2.comments_pushed + r2.comments_pulled + r2.deps_pulled, 0);
        assert_eq!(state.lock().unwrap().items.len(), 2);
    }

    #[tokio::test]
    async fn updates_flow_both_ways_by_last_writer() {
        let (store, client, state) = harness().await;
        let p = store.create(&new_task("Local")).unwrap();
        run(&store, &client, "org", "proj", BOTH).await.unwrap();
        let rid = store.sync_map_get(&p.id).unwrap().unwrap().remote_id;

        // Remote edit → pulled.
        {
            let mut m = state.lock().unwrap();
            let ts = m.stamp();
            let item = m.items.iter_mut().find(|i| i["id"] == rid).unwrap();
            item["title"] = json!("Renamed remotely");
            item["status"] = json!("in_progress");
            item["updatedAt"] = json!(ts);
        }
        let r = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert_eq!(r.pulled_updated, vec![p.id.clone()]);
        assert!(r.pushed_updated.is_empty());
        let local = store.get(&p.id).unwrap().unwrap();
        assert_eq!(local.title, "Renamed remotely");
        assert_eq!(local.status, PearlStatus::InProgress);

        // Local edit (strictly later) → pushed; remote's in_progress kept.
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.close(&[&p.id]).unwrap();
        let r = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert_eq!(r.pushed_updated, vec![p.id.clone()]);
        assert!(r.conflicts.is_empty(), "{:?}", r.conflicts);
        let m = state.lock().unwrap();
        let item = m.items.iter().find(|i| i["id"] == rid).unwrap();
        assert_eq!(item["status"], "done");
        assert_eq!(item["title"], "Renamed remotely");
    }

    #[tokio::test]
    async fn conflict_is_logged_and_newer_side_wins() {
        let (store, client, state) = harness().await;
        let p = store.create(&new_task("Both")).unwrap();
        run(&store, &client, "org", "proj", BOTH).await.unwrap();
        let rid = store.sync_map_get(&p.id).unwrap().unwrap().remote_id;
        // Local edit first, then a later remote edit.
        store
            .update(
                &p.id,
                &smooth_pearls::PearlUpdate {
                    title: Some("local wins?".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        {
            let mut m = state.lock().unwrap();
            let ts = m.stamp();
            let item = m.items.iter_mut().find(|i| i["id"] == rid).unwrap();
            item["title"] = json!("remote wins");
            item["updatedAt"] = json!(ts);
        }
        let r = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert_eq!(r.conflicts.len(), 1, "{:?}", r.conflicts);
        assert!(r.conflicts[0].contains("remote wins"));
        assert_eq!(store.get(&p.id).unwrap().unwrap().title, "remote wins");
        assert!(r.pushed_updated.is_empty());
    }

    #[tokio::test]
    async fn adopts_remote_item_by_external_ref_instead_of_duplicating() {
        let (store, client, state) = harness().await;
        let p = store.create(&new_task("Same pearl, other machine")).unwrap();
        state
            .lock()
            .unwrap()
            .seed_item(&json!({"title": "older remote copy", "externalRef": format!("{}@smooth", p.id)}));
        let r = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert!(r.pulled_created.is_empty() && r.pushed_created.is_empty());
        assert_eq!(state.lock().unwrap().items.len(), 1, "no duplicate created");
        assert!(store.sync_map_get(&p.id).unwrap().is_some());
        // Remote is newer here (seeded after the pearl) → its title landed.
        assert_eq!(store.get(&p.id).unwrap().unwrap().title, "older remote copy");
        assert_eq!(r.pulled_updated, vec![p.id]);
    }

    #[tokio::test]
    async fn pull_keeps_free_id_from_external_ref() {
        let (store, client, state) = harness().await;
        state
            .lock()
            .unwrap()
            .seed_item(&json!({"title": "made elsewhere", "externalRef": "th-feed00@smooth"}));
        let r = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert_eq!(r.pulled_created, vec!["th-feed00"]);
        assert!(store.get("th-feed00").unwrap().is_some());
        // A different project's ref is NOT adopted as an id.
        state
            .lock()
            .unwrap()
            .seed_item(&json!({"title": "other project", "externalRef": "th-feed01@elsewhere"}));
        let r = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert_eq!(r.pulled_created.len(), 1);
        assert_ne!(r.pulled_created[0], "th-feed01");
    }

    #[tokio::test]
    async fn never_deletes_either_side() {
        let (store, client, state) = harness().await;
        let gone_local = store.create(&new_task("deleted locally later")).unwrap();
        let gone_remote = store.create(&new_task("deleted remotely later")).unwrap();
        run(&store, &client, "org", "proj", BOTH).await.unwrap();
        let rid = store.sync_map_get(&gone_remote.id).unwrap().unwrap().remote_id;
        store.delete(&gone_local.id).unwrap();
        state.lock().unwrap().items.retain(|i| i["id"] != rid);
        // Touch the remote-orphaned pearl so push wants to PATCH it.
        store
            .update(
                &gone_remote.id,
                &smooth_pearls::PearlUpdate {
                    title: Some("edited".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        // And bump the local-orphaned remote item so pull wants to apply it.
        {
            let mut m = state.lock().unwrap();
            let ts = m.stamp();
            let item = m.items.iter_mut().find(|i| i["title"] == "deleted locally later").unwrap();
            item["updatedAt"] = json!(ts);
        }
        let r = run(&store, &client, "org", "proj", BOTH).await.unwrap();
        assert_eq!(r.skipped.len(), 2, "{:?}", r.skipped);
        assert_eq!(state.lock().unwrap().deletes, 0);
        assert_eq!(state.lock().unwrap().items.len(), 1, "remote orphan kept, nothing recreated");
        assert!(store.get(&gone_remote.id).unwrap().is_some(), "local orphan kept");
        assert!(store.get(&gone_local.id).unwrap().is_none(), "not resurrected");
    }

    #[tokio::test]
    async fn dry_run_writes_nothing() {
        let (store, client, state) = harness().await;
        store.create(&new_task("local")).unwrap();
        state.lock().unwrap().seed_item(&json!({"title": "remote"}));
        let r = run(
            &store,
            &client,
            "org",
            "proj",
            SyncOptions {
                pull: true,
                push: true,
                dry_run: true,
            },
        )
        .await
        .unwrap();
        assert!(r.dry_run);
        assert_eq!((r.pulled_created.len(), r.pushed_created.len()), (1, 1));
        assert_eq!(state.lock().unwrap().items.len(), 1);
        assert_eq!(store.list(&PearlQuery::new()).unwrap().len(), 1);
        assert!(store.sync_map_list().unwrap().is_empty());
        assert!(store.get_config(CONFIG_LAST_PULL).unwrap().is_none());
        let text = render(&r, Some("SMOOTH"));
        assert!(text.contains("dry run") && text.contains("SMOOTH"));
    }

    #[tokio::test]
    async fn push_only_reports_remote_newer_instead_of_clobbering() {
        let (store, client, state) = harness().await;
        let p = store.create(&new_task("p")).unwrap();
        run(&store, &client, "org", "proj", BOTH).await.unwrap();
        let rid = store.sync_map_get(&p.id).unwrap().unwrap().remote_id;
        store
            .update(
                &p.id,
                &smooth_pearls::PearlUpdate {
                    title: Some("local".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        {
            let mut m = state.lock().unwrap();
            let ts = m.stamp();
            let item = m.items.iter_mut().find(|i| i["id"] == rid).unwrap();
            item["title"] = json!("remote");
            item["updatedAt"] = json!(ts);
        }
        let r = run(
            &store,
            &client,
            "org",
            "proj",
            SyncOptions {
                pull: false,
                push: true,
                dry_run: false,
            },
        )
        .await
        .unwrap();
        assert!(r.pushed_updated.is_empty());
        assert_eq!(r.conflicts.len(), 1);
        assert!(r.conflicts[0].contains("--push-only"));
        assert_eq!(state.lock().unwrap().items[0]["title"], "remote");
    }
}
