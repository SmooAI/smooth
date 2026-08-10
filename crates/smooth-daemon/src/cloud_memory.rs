//! Cloud-backed [`Memory`] — routes the daemon's `remember`/`recall` to the
//! platform memory home (Family AI M3 Phase B.1, ADR-009) with a **local
//! fallback** (Phase B.2, pearl th-5189f0).
//!
//! # What this is
//!
//! Phase B.1 (already shipped to smooai main) added an org-scoped, owner-tiered
//! `memories` REST surface on `api.smoo.ai`:
//!
//! - `POST   /organizations/:org/memories`        — embed-on-write, returns the row
//! - `POST   /organizations/:org/memories/search` — hybrid dense+BM25 search
//! - `GET    /organizations/:org/memories`        — owner-filtered list
//! - `DELETE /organizations/:org/memories/:id`    — forget (owner/admin)
//!
//! [`CloudMemory`] implements the engine's synchronous [`Memory`] trait by
//! calling that surface, so a member's `remember` is durable-and-synced across
//! every device the family signs in on, and `recall` (plus the engine's
//! auto-recall) reads the shared home.
//!
//! # Fail-soft, always
//!
//! Any cloud error — network, 5xx, a missing/expired session — falls back to the
//! bound LOCAL store instead of failing the turn. A flaky platform must never
//! make Big Smooth unable to remember something. Every fallback is logged.
//!
//! # Known limitation (deliberate, ponytail)
//!
//! Store is cloud-first, local-on-error; recall is cloud-first, local-on-error.
//! So a fact stored locally during a cloud outage is invisible to a later
//! cloud-up recall (and vice-versa) until the two converge. A dual-read merge is
//! deliberately out of scope for B.2 — the fail-soft contract is "never hard-fail
//! a turn", not "perfectly reconcile two stores". Upgrade path: merge cloud +
//! local hits in `recall` and replay local-only writes when cloud recovers.
//!
//! # Scope
//!
//! A memory is written **personal** by default (owner = the acting principal), so
//! a member's `remember` is private-and-synced. An entry may opt into the org-wide
//! **shared** tier by carrying a `scope = "shared"` key in its
//! [`MemoryEntry::metadata`] — the clear, trait-preserving path for a future
//! `remember({scope})` tool arg to set, with no change to the `Memory` trait.
//!
//! # Auth
//!
//! The bearer is read at call-time from the daemon's live **user** session
//! (`smooai-user.json`), which the credential heartbeat
//! ([`crate::auth_login::spawn_credential_heartbeat`]) keeps fresh — so this type
//! does not re-implement token refresh. Personal scope requires that human
//! identity: an M2M session can only write shared memories (B.1 rejects personal
//! for a token with no user), which fail-soft turns into a local write.

use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use reqwest::Client;
use serde_json::{Map, Value};
use tokio::runtime::Handle;

use smooai_client_shared::auth::storage::CredentialsStore;
use smooth_operator::{Memory, MemoryEntry, MemoryType};

/// Default production API base — override with `SMOOAI_API_URL` (matches the
/// convention `smooth-api-client` and `apps/web` use).
const DEFAULT_API_BASE_URL: &str = "https://api.smoo.ai";

/// The write tier for a memory (ADR-009 §1). Mirrors the server's `scope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryScope {
    /// Owner = the acting principal; visible to that user (and, later, guardians).
    Personal,
    /// Owner NULL; visible org-wide.
    Shared,
}

impl MemoryScope {
    /// The wire value sent as the request's `scope` field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Shared => "shared",
        }
    }

    /// Parse a scope hint (from entry metadata). Unknown → `None` (caller keeps
    /// its bound default rather than silently downgrading).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "personal" => Some(Self::Personal),
            "shared" => Some(Self::Shared),
            _ => None,
        }
    }
}

/// The per-adapter shared bits every `CloudMemory` instance needs.
///
/// Cheap to clone (a reqwest `Client` is an `Arc` inside; the rest are small).
/// Built once and handed to each per-turn `CloudMemory` bound to that turn's org
/// + principal.
#[derive(Clone)]
pub struct CloudMemoryDeps {
    /// HTTP client for the platform calls.
    pub http: Client,
    /// API base (`https://api.smoo.ai`, or `SMOOAI_API_URL`).
    pub base_url: String,
    /// The runtime handle the sync→async bridge spawns onto.
    pub handle: Handle,
    /// Source of the live user bearer (kept fresh by the credential heartbeat).
    pub store: CredentialsStore,
}

impl CloudMemoryDeps {
    /// Build the deps from the environment: default user credentials store +
    /// `SMOOAI_API_URL` (or the production default). Call from within the tokio
    /// runtime so [`Handle::current`] is valid.
    ///
    /// # Errors
    /// Fails only if the default user credentials store path can't be resolved
    /// (`$HOME` unset and no `SMOOAI_AUTH_FILE`).
    pub fn from_env(handle: Handle) -> Result<Self> {
        Ok(Self {
            http: Client::builder().build()?,
            base_url: std::env::var("SMOOAI_API_URL").unwrap_or_else(|_| DEFAULT_API_BASE_URL.to_owned()),
            handle,
            store: CredentialsStore::default_user()?,
        })
    }
}

/// A [`Memory`] backend bound to one `(org, principal)` that routes to the
/// platform memory home, falling back to a local store on any cloud trouble.
pub struct CloudMemory {
    deps: CloudMemoryDeps,
    /// Org the REST path is scoped to.
    org_id: String,
    /// Default write tier — `Personal`. Overridable per-entry via metadata.
    scope: MemoryScope,
    /// Attribution: which principal (family member) taught the fact. `None` when
    /// the turn had no resolved user identity → the server defaults it.
    source: Option<String>,
    /// Fallback store used on any cloud error (the daemon's durable sqlite).
    local: Arc<dyn Memory>,
}

impl CloudMemory {
    /// Bind a cloud memory to `org_id`, attributing writes to `principal`, with
    /// `local` as the fail-soft fallback. Writes default to the **personal** tier.
    #[must_use]
    pub fn new(deps: CloudMemoryDeps, org_id: impl Into<String>, principal: Option<String>, local: Arc<dyn Memory>) -> Self {
        Self {
            deps,
            org_id: org_id.into(),
            scope: MemoryScope::Personal,
            source: principal.filter(|s| !s.trim().is_empty()),
            local,
        }
    }

    /// Read the current user bearer from the (heartbeat-kept-fresh) session.
    fn bearer(&self) -> Result<String> {
        let creds = self
            .deps
            .store
            .load()?
            .ok_or_else(|| anyhow!("no Smoo AI session on disk (run `th auth login`)"))?;
        if creds.access_token.trim().is_empty() {
            bail!("Smoo AI session has an empty access token");
        }
        Ok(creds.access_token)
    }

    /// The effective scope for an entry — a valid `scope` metadata hint wins,
    /// else the bound default.
    fn scope_for(&self, entry: &MemoryEntry) -> MemoryScope {
        entry.metadata.get("scope").and_then(|s| MemoryScope::parse(s)).unwrap_or(self.scope)
    }

    /// Build the `POST /memories` body from an entry (matches B.1 `CreateBody`:
    /// `content`, `memoryType`, `scope`, optional `source`).
    fn create_body(&self, entry: &MemoryEntry) -> Value {
        let mut obj = Map::new();
        obj.insert("content".to_owned(), Value::String(entry.content.clone()));
        obj.insert("memoryType".to_owned(), Value::String(memory_type_to_str(entry.memory_type)));
        obj.insert("scope".to_owned(), Value::String(self.scope_for(entry).as_str().to_owned()));
        if let Some(src) = &self.source {
            obj.insert("source".to_owned(), Value::String(src.clone()));
        }
        Value::Object(obj)
    }

    fn memories_url(&self) -> String {
        format!("{}/organizations/{}/memories", self.deps.base_url.trim_end_matches('/'), self.org_id)
    }

    /// Drive an async future to completion from a synchronous trait method.
    ///
    /// Same bridge as the postgres `PgMemory`: spawn onto the captured runtime so
    /// the request's I/O makes progress on that reactor, then block on the
    /// `JoinHandle` from a throwaway OS thread — never `Handle::block_on` on a
    /// worker thread (which panics "Cannot start a runtime from within a runtime").
    fn run_blocking<F, T>(&self, fut: F) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        let join = self.deps.handle.spawn(fut);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| -> Result<T> {
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                let joined = rt.block_on(join);
                joined.map_err(|e| anyhow!("cloud memory task panicked or was cancelled: {e}"))?
            })();
            let _ = tx.send(result);
        });
        rx.recv().map_err(|e| anyhow!("cloud memory task channel closed: {e}"))?
    }
}

/// Serialize a [`MemoryType`] to its bare serde tag (e.g. `"Project"` → `Project`).
fn memory_type_to_str(mt: MemoryType) -> String {
    serde_json::to_string(&mt).map_or_else(|_| "LongTerm".to_owned(), |s| s.trim_matches('"').to_owned())
}

/// Parse a stored `memoryType` text back into a [`MemoryType`]; unknown/absent → `LongTerm`.
fn memory_type_from_str(s: &str) -> MemoryType {
    serde_json::from_str(&format!("\"{s}\"")).unwrap_or(MemoryType::LongTerm)
}

/// Map a `POST /memories/search` response (`{results: [{id, content, memoryType,
/// score, ...}]}`) back into engine [`MemoryEntry`]s, capped at `limit`.
fn parse_search_results(body: &Value, limit: usize) -> Vec<MemoryEntry> {
    let Some(results) = body.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    results.iter().take(limit).filter_map(parse_one_result).collect()
}

/// One search result → a `MemoryEntry`. Requires `content`; everything else
/// degrades gracefully (missing type → `LongTerm`, null score → 0.0).
fn parse_one_result(row: &Value) -> Option<MemoryEntry> {
    let content = row.get("content").and_then(Value::as_str)?;
    let memory_type = row.get("memoryType").and_then(Value::as_str).map_or(MemoryType::LongTerm, memory_type_from_str);
    let mut entry = MemoryEntry::new(content, memory_type);
    if let Some(id) = row.get("id").and_then(Value::as_str) {
        id.clone_into(&mut entry.id);
    }
    #[allow(clippy::cast_possible_truncation, reason = "relevance is a display score; f32 precision is ample")]
    {
        entry.relevance = row.get("score").and_then(Value::as_f64).unwrap_or(0.0) as f32;
    }
    Some(entry)
}

/// POST `body` to `url` with `bearer`, returning the parsed JSON (or an error on
/// any non-2xx / transport / parse failure — the caller fails soft on `Err`).
async fn post_json(http: Client, bearer: String, url: String, body: Value) -> Result<Value> {
    let resp = http.post(&url).bearer_auth(bearer).json(&body).send().await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("cloud memory POST {url} -> HTTP {status}: {text}");
    }
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&text).map_err(|e| anyhow!("parse cloud memory response from {url}: {e}"))
}

/// DELETE `url` with `bearer`; 2xx is success (404 is treated as failure so the
/// caller can fall back to a local forget by the same id).
async fn delete(http: Client, bearer: String, url: String) -> Result<()> {
    let resp = http.delete(&url).bearer_auth(bearer).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("cloud memory DELETE {url} -> HTTP {status}: {text}");
    }
    Ok(())
}

impl Memory for CloudMemory {
    fn store(&self, entry: MemoryEntry) -> Result<()> {
        let bearer = match self.bearer() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "cloud memory store: no session; writing to local fallback");
                return self.local.store(entry);
            }
        };
        let (http, url, body) = (self.deps.http.clone(), self.memories_url(), self.create_body(&entry));
        match self.run_blocking(post_json(http, bearer, url, body)) {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "cloud memory store failed; writing to local fallback");
                self.local.store(entry)
            }
        }
    }

    fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let bearer = match self.bearer() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "cloud memory recall: no session; reading local fallback");
                return self.local.recall(query, limit);
            }
        };
        let url = format!("{}/search", self.memories_url());
        let body = serde_json::json!({ "query": query, "limit": limit });
        let http = self.deps.http.clone();
        match self.run_blocking(post_json(http, bearer, url, body)) {
            Ok(v) => Ok(parse_search_results(&v, limit)),
            Err(e) => {
                tracing::warn!(error = %e, "cloud memory recall failed; reading local fallback");
                self.local.recall(query, limit)
            }
        }
    }

    fn forget(&self, id: &str) -> Result<()> {
        // Best-effort: cloud ids and local ids diverge (each store mints its own),
        // so forget-by-id is inherently approximate. Try cloud, fall back to a
        // local forget with the same id on any error.
        let bearer = match self.bearer() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "cloud memory forget: no session; forgetting locally");
                return self.local.forget(id);
            }
        };
        let url = format!("{}/{}", self.memories_url(), id);
        let http = self.deps.http.clone();
        match self.run_blocking(delete(http, bearer, url)) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "cloud memory forget failed; forgetting locally");
                self.local.forget(id)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use chrono::Utc;
    use serde_json::json;
    use smooai_client_shared::auth::storage::{CredentialKind, Credentials};
    use smooth_operator::InMemoryMemory;

    use super::*;

    // ── pure mapping helpers ────────────────────────────────────────────────

    #[test]
    fn memory_type_round_trips_and_is_unquoted() {
        for mt in [
            MemoryType::ShortTerm,
            MemoryType::LongTerm,
            MemoryType::Entity,
            MemoryType::User,
            MemoryType::Feedback,
            MemoryType::Project,
            MemoryType::Reference,
        ] {
            let s = memory_type_to_str(mt);
            assert!(!s.contains('"'), "stored form must be unquoted: {s:?}");
            assert_eq!(memory_type_from_str(&s), mt);
        }
        // Unknown text degrades to LongTerm, never panics.
        assert_eq!(memory_type_from_str("nonsense"), MemoryType::LongTerm);
    }

    #[test]
    fn scope_parse_and_wire_values() {
        assert_eq!(MemoryScope::parse("personal"), Some(MemoryScope::Personal));
        assert_eq!(MemoryScope::parse(" SHARED "), Some(MemoryScope::Shared));
        assert_eq!(MemoryScope::parse("household"), None);
        assert_eq!(MemoryScope::Personal.as_str(), "personal");
        assert_eq!(MemoryScope::Shared.as_str(), "shared");
    }

    #[test]
    fn parse_search_results_maps_fields_and_defaults() {
        let body = json!({
            "results": [
                { "id": "m1", "content": "the user is Brent", "memoryType": "User", "score": 0.9 },
                { "id": "m2", "content": "lexical only", "memoryType": null, "score": null },
                { "content": "no id row", "memoryType": "Project", "score": 0.5 },
                { "memoryType": "User", "score": 0.1 } // no content → dropped
            ]
        });
        let hits = parse_search_results(&body, 10);
        assert_eq!(hits.len(), 3, "content-less row dropped");
        assert_eq!(hits[0].id, "m1");
        assert_eq!(hits[0].memory_type, MemoryType::User);
        assert!((hits[0].relevance - 0.9).abs() < 1e-6);
        // Null type → LongTerm, null score → 0.0.
        assert_eq!(hits[1].memory_type, MemoryType::LongTerm);
        assert!((hits[1].relevance - 0.0).abs() < 1e-6);
        // Missing id still parses (a fresh uuid was minted, not empty).
        assert!(!hits[2].id.is_empty());
    }

    #[test]
    fn parse_search_results_missing_results_is_empty_and_respects_limit() {
        assert!(parse_search_results(&json!({}), 5).is_empty());
        let body = json!({ "results": [
            { "content": "a" }, { "content": "b" }, { "content": "c" }
        ]});
        assert_eq!(parse_search_results(&body, 2).len(), 2, "limit is honored");
    }

    // ── body construction (scope default + metadata override + source) ──────

    fn deps_for(base_url: &str, token: Option<&str>) -> (CloudMemoryDeps, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsStore::at(dir.path().join("smooai-user.json"));
        if let Some(t) = token {
            store
                .save(&Credentials {
                    access_token: t.to_owned(),
                    refresh_token: None,
                    expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                    user: Some("brent@smoo.ai".to_owned()),
                    active_org_id: Some("org-1".to_owned()),
                    client_id: None,
                    client_secret: None,
                    kind: CredentialKind::User,
                    created_at: Utc::now(),
                })
                .unwrap();
        }
        let deps = CloudMemoryDeps {
            http: Client::builder().build().unwrap(),
            base_url: base_url.to_owned(),
            handle: Handle::current(),
            store,
        };
        (deps, dir)
    }

    #[tokio::test]
    async fn create_body_defaults_personal_and_carries_source() {
        let (deps, _d) = deps_for("http://127.0.0.1:0", None);
        let cm = CloudMemory::new(deps, "org-1", Some("member-jr".to_owned()), Arc::new(InMemoryMemory::new()));
        let body = cm.create_body(&MemoryEntry::new("likes trains", MemoryType::User));
        assert_eq!(body["content"], "likes trains");
        assert_eq!(body["memoryType"], "User");
        assert_eq!(body["scope"], "personal", "default tier is personal");
        assert_eq!(body["source"], "member-jr", "attribution = principal id");
    }

    #[tokio::test]
    async fn create_body_metadata_overrides_scope_and_omits_empty_source() {
        let (deps, _d) = deps_for("http://127.0.0.1:0", None);
        let cm = CloudMemory::new(deps, "org-1", None, Arc::new(InMemoryMemory::new()));
        let entry = MemoryEntry::new("trash goes out Tuesday", MemoryType::Project).with_metadata("scope", "shared");
        let body = cm.create_body(&entry);
        assert_eq!(body["scope"], "shared", "metadata hint overrides the default");
        assert!(body.get("source").is_none(), "no principal → source omitted, server defaults it");
    }

    // ── live round-trip against a mock api.smoo.ai (multi-thread: the sync
    //    Memory methods block a worker while the request runs on another) ─────

    #[derive(Clone, Default)]
    struct Captured {
        create_bodies: Arc<Mutex<Vec<Value>>>,
        search_bodies: Arc<Mutex<Vec<Value>>>,
    }

    async fn mock_create(State(cap): State<Captured>, Json(body): Json<Value>) -> Json<Value> {
        cap.create_bodies.lock().unwrap().push(body.clone());
        Json(json!({ "id": "srv-1", "content": body["content"], "memoryType": body["memoryType"], "source": body["source"] }))
    }

    async fn mock_search(State(cap): State<Captured>, Json(body): Json<Value>) -> Json<Value> {
        cap.search_bodies.lock().unwrap().push(body.clone());
        Json(json!({ "query": body["query"], "results": [
            { "id": "srv-9", "content": "recalled from cloud", "memoryType": "Feedback", "score": 0.77 }
        ]}))
    }

    /// Spin a mock api.smoo.ai on an ephemeral port; return (base_url, captured).
    async fn spawn_mock() -> (String, Captured) {
        let cap = Captured::default();
        let app = Router::new()
            .route("/organizations/{org}/memories", post(mock_create))
            .route("/organizations/{org}/memories/search", post(mock_search))
            .with_state(cap.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), cap)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn store_posts_to_cloud_and_recall_maps_results() {
        let (base, cap) = spawn_mock().await;
        let (deps, _d) = deps_for(&base, Some("user-jwt"));
        let local = Arc::new(InMemoryMemory::new());
        let cm = CloudMemory::new(deps, "org-1", Some("dad".to_owned()), local.clone());

        cm.store(MemoryEntry::new("garage code is 1234", MemoryType::User)).unwrap();
        let created = cap.create_bodies.lock().unwrap().clone();
        assert_eq!(created.len(), 1, "store hit the cloud, not the local fallback");
        assert_eq!(created[0]["content"], "garage code is 1234");
        assert_eq!(created[0]["scope"], "personal");
        assert_eq!(created[0]["source"], "dad");
        // Fell through to cloud → nothing written locally.
        assert!(
            local.recall("garage", 5).unwrap().is_empty(),
            "successful cloud store does not double-write local"
        );

        let hits = cm.recall("garage code", 5).unwrap();
        assert_eq!(cap.search_bodies.lock().unwrap().len(), 1, "recall hit the cloud");
        assert_eq!(cap.search_bodies.lock().unwrap()[0]["query"], "garage code");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, "recalled from cloud");
        assert_eq!(hits[0].memory_type, MemoryType::Feedback);
        assert!((hits[0].relevance - 0.77).abs() < 1e-6);
    }

    // ── fail-soft: any cloud error → local store/recall ─────────────────────

    async fn spawn_500() -> String {
        async fn boom() -> (axum::http::StatusCode, &'static str) {
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "nope")
        }
        let app = Router::new()
            .route("/organizations/{org}/memories", post(boom))
            .route("/organizations/{org}/memories/search", post(boom));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn store_falls_back_to_local_on_cloud_5xx() {
        let base = spawn_500().await;
        let (deps, _d) = deps_for(&base, Some("user-jwt"));
        let local = Arc::new(InMemoryMemory::new());
        let cm = CloudMemory::new(deps, "org-1", Some("dad".to_owned()), local.clone());

        // The turn must NOT fail — store returns Ok, having written locally.
        cm.store(MemoryEntry::new("dog's name is Rex", MemoryType::User)).unwrap();
        let hits = local.recall("dog Rex", 5).unwrap();
        assert_eq!(hits.len(), 1, "cloud 5xx → the fact landed in the local fallback");
        assert!(hits[0].content.contains("Rex"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recall_falls_back_to_local_on_cloud_5xx() {
        let base = spawn_500().await;
        let (deps, _d) = deps_for(&base, Some("user-jwt"));
        let local: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        local.store(MemoryEntry::new("wifi password is hunter2", MemoryType::Reference)).unwrap();
        let cm = CloudMemory::new(deps, "org-1", None, local);

        let hits = cm.recall("wifi password", 5).unwrap();
        assert_eq!(hits.len(), 1, "cloud 5xx → recall reads the local fallback");
        assert!(hits[0].content.contains("hunter2"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_session_falls_back_to_local() {
        // No token saved → bearer() errors → straight to local, no network needed.
        let (deps, _d) = deps_for("http://127.0.0.1:0", None);
        let local = Arc::new(InMemoryMemory::new());
        let cm = CloudMemory::new(deps, "org-1", None, local.clone());
        cm.store(MemoryEntry::new("no session fact", MemoryType::User)).unwrap();
        assert_eq!(local.recall("session fact", 5).unwrap().len(), 1, "no session → local write");
    }
}
